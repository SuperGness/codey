use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use hyper_util::client::proxy::matcher::Matcher as SystemProxyMatcher;
use reqwest::header::{
    AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue,
};
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::server::{
    ErrorResponse as WebSocketErrorResponse, Request as WebSocketRequest,
    Response as WebSocketResponse,
};
use tokio_tungstenite::tungstenite::http::{
    StatusCode as WebSocketStatusCode, Uri as WebSocketUri,
};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{
    Error as WebSocketError, Message as WebSocketMessage, Utf8Bytes as WebSocketText,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, accept_hdr_async_with_config, connect_async_with_config,
};
use uuid::Uuid;

use crate::codex_config::CHATGPT_CODEX_BASE_URL;
use crate::config::{
    CodeyConfig, ProviderProfile, RouteRequestLogBackend, UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
    UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS, UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
};
use crate::model_id;
use crate::route_request_log::{
    FirstByteSource, RequestProtocol, RouteRequestLogClearResult, RouteRequestLogController,
    RouteRequestLogGuard, RouteRequestLogProbe, RouteRequestLogQuery, RouteRequestLogReconfigure,
    RouteRequestLogStart, UpstreamTransport,
};

pub(crate) const ROUTER_PROVIDER_ID: &str = "codey_router";
pub(crate) const ROUTER_AUTH_HEADER: &str = "x-codey-router-token";
const TURN_METADATA_HEADER: &str = "x-codex-turn-metadata";
const ROUTE_METADATA_KEY: &str = "codey_route";
const CHATGPT_ACCOUNT_ID_HEADER: &str = "chatgpt-account-id";
const PROMPT_CACHE_KEY_HEADER: &str = "prompt-cache-key";
const PROMPT_CACHE_KEY_COMPAT_HEADER: &str = "prompt_cache_key";
const PROMPT_CACHE_KEY_BODY_FIELD: &str = "prompt_cache_key";
pub(crate) const CODEX_AUTO_REVIEW_MODEL: &str = "codex-auto-review";

const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_UPSTREAM_ERROR_BYTES: usize = 64 * 1024;
const MAX_UPSTREAM_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MAX_UPSTREAM_SSE_BUFFER_BYTES: usize = 2 * 1024 * 1024;
const UPSTREAM_SSE_SNIFF_BYTES: usize = 1024;
const REQUEST_JSON_OFFLOAD_BYTES: usize = 256 * 1024;
const MAX_CUSTOM_TOOL_BRIDGE_DESCRIPTION_BYTES: usize = 8 * 1024;
const MAX_CUSTOM_TOOL_SOURCE_DESCRIPTION_BYTES: usize = 2 * 1024;
const MAX_CONCURRENT_CONNECTIONS: usize = 64;
const MAX_CONCURRENT_REJECTIONS: usize = 4;
const REQUEST_BODY_BUDGET_BYTES: usize = 256 * 1024 * 1024;
const REQUEST_BODY_BUDGET_UNIT_BYTES: usize = 64 * 1024;
// serde_json trees and protocol conversion buffers live alongside the encoded
// request. Reserve a conservative multiple of the wire size so the semaphore
// represents the request's working set instead of only its first Vec<u8>.
const REQUEST_MEMORY_BUDGET_MULTIPLIER: usize = 4;
const REQUEST_BODY_BUDGET_PERMITS: usize =
    REQUEST_BODY_BUDGET_BYTES / REQUEST_BODY_BUDGET_UNIT_BYTES;
const MAX_ROUTE_BINDINGS: usize = 4096;
const MAX_UPSTREAM_WEBSOCKET_BACKOFFS: usize = 128;
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(30);
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const UPSTREAM_RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(30);
// A non-streaming upstream may not send response headers until generation is
// complete, so its header wait is also the model's total generation budget.
const UPSTREAM_NON_STREAM_RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const UPSTREAM_READ_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const DOWNSTREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const UPSTREAM_HTTP_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const UPSTREAM_TCP_KEEPALIVE_IDLE: Duration = Duration::from_secs(15);
const UPSTREAM_TCP_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const UPSTREAM_TCP_KEEPALIVE_RETRIES: u32 = 3;
const UPSTREAM_WEBSOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const UPSTREAM_WEBSOCKET_BACKOFF_STEPS: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(15 * 60),
];
const UPSTREAM_WEBSOCKET_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(25);
const UPSTREAM_WEBSOCKET_PONG_TIMEOUT: Duration = Duration::from_secs(10);
const UPSTREAM_WEBSOCKET_MAX_REUSE_AGE: Duration = Duration::from_secs(55 * 60);
const RESPONSES_WEBSOCKET_BETA: &str = "responses_websockets=2026-02-06";
const RESPONSES_WEBSOCKET_PATHS: [&str; 2] = ["/v1/responses", "/responses"];
const REQUEST_LOG_PAGE_PATH: &str = "/codey/request-logs";
const REQUEST_LOG_SCRIPT_PATH: &str = "/codey/request-logs.js";
const REQUEST_LOG_PAGE: &str = r#"<!doctype html>
<html lang="zh-CN">
<head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Codey 请求日志</title></head>
<body><div id="root"></div><script src="/codey/request-logs.js"></script></body>
</html>"#;
#[cfg(not(test))]
const ROUTER_SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(test)]
const ROUTER_SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_millis(100);

tokio::task_local! {
    static ROUTER_REQUEST_ID: String;
    static ROUTER_REQUEST_STARTED_AT: Instant;
}

fn record_router_failure_nonblocking(
    event: &'static str,
    operation: &'static str,
    error: impl Into<String>,
    context: Value,
) {
    let error = error.into();
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        // ponytail: router concurrency/backoff bounds task volume; use a shared bounded
        // error-log queue if failure storms ever make this measurable.
        drop(runtime.spawn_blocking(move || {
            crate::error_log::record_failure(event, operation, error, context);
        }));
    } else {
        crate::error_log::record_failure(event, operation, error, context);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimeRouterEndpoint {
    pub base_url: String,
    pub token: String,
    pub supports_websockets: bool,
    pub supports_remote_compaction: bool,
    /// Official ChatGPT routes are available this launch. Codex keeps its
    /// native OpenAI login for this provider; the independent router header
    /// authenticates the loopback hop and the gateway isolates upstream auth.
    pub requires_openai_auth: bool,
}

impl RuntimeRouterEndpoint {
    pub(crate) fn request_log_url(&self) -> String {
        format!(
            "{}/codey/request-logs#{}",
            self.base_url.trim_end_matches("/v1"),
            self.token
        )
    }
}

#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestLogCatalog {
    profiles: Vec<RequestLogProfile>,
    selected_models_by_provider: BTreeMap<String, Vec<String>>,
    declared_official_models_by_provider: BTreeMap<String, Vec<String>>,
    upstream_models_by_provider: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestLogProfile {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_provider_id: Option<String>,
}

impl RequestLogCatalog {
    fn from_config(config: &CodeyConfig) -> Self {
        Self {
            profiles: config
                .profiles
                .iter()
                .map(|profile| RequestLogProfile {
                    id: profile.id.clone(),
                    name: profile.name.clone(),
                    source_provider_id: profile.source_provider_id.clone(),
                })
                .collect(),
            selected_models_by_provider: config.selected_models_by_provider.clone(),
            declared_official_models_by_provider: config
                .declared_official_models_by_provider
                .clone(),
            upstream_models_by_provider: config.upstream_models_by_provider.clone(),
        }
    }
}

pub(crate) struct LocalRouter {
    endpoint: RuntimeRouterEndpoint,
    snapshot: Arc<RwLock<Arc<RouterSnapshot>>>,
    websocket_backoffs: Arc<Mutex<UpstreamWebSocketBackoffs>>,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    request_log: Arc<RouteRequestLogController>,
}

impl LocalRouter {
    pub(crate) async fn start(config: &CodeyConfig) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("启动 Codey 本地路由失败")?;
        let port = listener
            .local_addr()
            .context("读取 Codey 本地路由监听地址失败")?
            .port();
        let token = format!("codey-router-{}", Uuid::new_v4());
        let endpoint = RuntimeRouterEndpoint {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            token,
            supports_websockets: config.runtime_supports_websockets(),
            supports_remote_compaction: config.runtime_supports_remote_compaction(),
            requires_openai_auth: config.router_requires_openai_auth(),
        };
        let snapshot = Arc::new(RwLock::new(Arc::new(RouterSnapshot::from_config(config))));
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let official_auth_path = crate::codex_config::codex_home().join("auth.json");
        let websocket_backoffs = Arc::new(Mutex::new(UpstreamWebSocketBackoffs::default()));
        let request_log = Arc::new(RouteRequestLogController::new());
        if let Err(error) = request_log.reconfigure(&config.route_request_log).await {
            record_router_failure_nonblocking(
                "route_request_log_start_failed",
                "start_route_request_log",
                format!("{error:#}"),
                serde_json::json!({}),
            );
        }
        let server = RouterServer {
            token: endpoint.token.clone(),
            bearer_token: format!("Bearer {}", endpoint.token),
            snapshot: Arc::clone(&snapshot),
            connection_limit: Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS)),
            rejection_limit: Arc::new(Semaphore::new(MAX_CONCURRENT_REJECTIONS)),
            request_body_budget: Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS)),
            bindings: Arc::new(Mutex::new(RouteBindings::default())),
            websocket_backoffs: Arc::clone(&websocket_backoffs),
            client: reqwest::Client::builder()
                .user_agent(format!("Codey-Router/{}", env!("CARGO_PKG_VERSION")))
                .connect_timeout(UPSTREAM_CONNECT_TIMEOUT)
                // Reuse a warm TLS connection across normal tool turns while
                // TCP probes evict half-open sockets before the next request.
                .pool_idle_timeout(Some(UPSTREAM_HTTP_POOL_IDLE_TIMEOUT))
                .http2_adaptive_window(true)
                .tcp_nodelay(true)
                .tcp_keepalive(Some(UPSTREAM_TCP_KEEPALIVE_IDLE))
                .tcp_keepalive_interval(Some(UPSTREAM_TCP_KEEPALIVE_INTERVAL))
                .tcp_keepalive_retries(Some(UPSTREAM_TCP_KEEPALIVE_RETRIES))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .context("创建 Codey 本地路由 HTTP 客户端失败")?,
            official_auth_path,
            official_auth_cache: Arc::new(Mutex::new(
                crate::account_usage::OfficialAuthCache::default(),
            )),
            request_log: Arc::clone(&request_log),
        };
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    joined = connections.join_next(), if !connections.is_empty() => {
                        if let Some(Err(error)) = joined
                            && error.is_panic()
                        {
                            record_router_failure_nonblocking(
                                "local_router_connection_task_failed",
                                "join_local_router_connection",
                                error.to_string(),
                                serde_json::json!({}),
                            );
                        }
                    }
                    result = listener.accept() => {
                        match result {
                            Ok((stream, _)) => {
                                // Chunked SSE writes each event as three small
                                // writes (size line, payload, CRLF). Nagle would
                                // hold those back waiting on delayed ACKs and add
                                // latency to every streamed token.
                                let _ = stream.set_nodelay(true);
                                let server = server.clone();
                                let permit = match Arc::clone(&server.connection_limit)
                                    .try_acquire_owned()
                                {
                                    Ok(permit) => permit,
                                    Err(_) => {
                                        if let Ok(rejection_permit) = Arc::clone(
                                            &server.rejection_limit,
                                        )
                                        .try_acquire_owned()
                                        {
                                            let request_id = Uuid::new_v4().simple().to_string();
                                            connections.spawn(async move {
                                                ROUTER_REQUEST_ID.scope(request_id, async move {
                                                    let _rejection_permit = rejection_permit;
                                                    let mut stream = stream;
                                                    let _ = tokio::time::timeout(
                                                        DOWNSTREAM_WRITE_TIMEOUT,
                                                        write_error_response(
                                                            &mut stream,
                                                            503,
                                                            "router_busy",
                                                            "Codey 本地路由当前请求过多，请稍后重试",
                                                            None,
                                                        ),
                                                    )
                                                    .await;
                                                }).await;
                                            });
                                        }
                                        continue;
                                    }
                                };
                                let request_id = Uuid::new_v4().simple().to_string();
                                connections.spawn(ROUTER_REQUEST_ID.scope(
                                    request_id.clone(),
                                    ROUTER_REQUEST_STARTED_AT.scope(Instant::now(), async move {
                                        let _permit = permit;
                                        if let Err(error) = server.handle_connection(stream).await {
                                            record_router_failure_nonblocking(
                                                "local_router_request_failed",
                                                "handle_local_router_connection",
                                                format!("{error:#}"),
                                                serde_json::json!({ "requestId": request_id }),
                                            );
                                        }
                                    }),
                                ));
                            }
                            Err(error) => {
                                record_router_failure_nonblocking(
                                    "local_router_accept_failed",
                                    "accept_local_router_connection",
                                    error.to_string(),
                                    serde_json::json!({}),
                                );
                                break;
                            }
                        }
                    }
                }
            }
            drop(listener);
            let drained = tokio::time::timeout(ROUTER_SHUTDOWN_DRAIN_TIMEOUT, async {
                while let Some(joined) = connections.join_next().await {
                    if let Err(error) = joined
                        && error.is_panic()
                    {
                        record_router_failure_nonblocking(
                            "local_router_connection_task_failed",
                            "drain_local_router_connection",
                            error.to_string(),
                            serde_json::json!({}),
                        );
                    }
                }
            })
            .await;
            if drained.is_err() {
                connections.abort_all();
                while connections.join_next().await.is_some() {}
            }
        });
        Ok(Self {
            endpoint,
            snapshot,
            websocket_backoffs,
            shutdown: Mutex::new(Some(shutdown_tx)),
            task: Mutex::new(Some(task)),
            request_log,
        })
    }

    pub(crate) fn endpoint(&self) -> RuntimeRouterEndpoint {
        self.endpoint.clone()
    }

    pub(crate) fn update_config(&self, config: &CodeyConfig) {
        let next = Arc::new(RouterSnapshot::from_config(config));
        *self
            .snapshot
            .write()
            .expect("local router snapshot lock poisoned") = next;
        self.websocket_backoffs
            .lock()
            .expect("upstream WebSocket backoff mutex poisoned")
            .clear();
    }

    pub(crate) async fn reconfigure_request_log(
        &self,
        config: &crate::config::RouteRequestLogConfig,
    ) -> Result<RouteRequestLogReconfigure> {
        self.request_log.reconfigure(config).await
    }

    pub(crate) async fn clear_request_logs(&self) -> RouteRequestLogClearResult {
        self.request_log.clear().await
    }

    pub(crate) async fn stop(&self) -> Result<()> {
        if let Some(shutdown) = self
            .shutdown
            .lock()
            .expect("local router shutdown mutex poisoned")
            .take()
        {
            let _ = shutdown.send(());
        }
        let task = self
            .task
            .lock()
            .expect("local router task mutex poisoned")
            .take();
        let task_result = match task {
            Some(task) => task.await.context("关闭 Codey 本地路由任务异常退出"),
            None => Ok(()),
        };
        if let Some(stats) = self.request_log.stop().await
            && stats.degraded()
        {
            eprintln!(
                "Codey 路由请求日志已静默降级：accepted={} written={} sampled_out={} dropped_full={} dropped_closed={} write_failures={} write_dropped={} observer_panics={} writer_panics={} shutdown_timeouts={}",
                stats.accepted,
                stats.entries_written,
                stats.sampled_out,
                stats.dropped_full,
                stats.dropped_closed,
                stats.write_failures,
                stats.write_dropped,
                stats.observer_panics,
                stats.writer_panics,
                stats.shutdown_timeouts,
            );
        }
        task_result
    }
}

#[cfg(not(test))]
pub(crate) fn outbound_proxy_applies_to_route(profile: &ProviderProfile) -> bool {
    let base_url = if profile.official_account {
        CHATGPT_CODEX_BASE_URL
    } else {
        profile.base_url.as_str()
    };
    outbound_proxy_applies_to_url_with_matcher(base_url, &SystemProxyMatcher::from_system())
}

#[cfg(test)]
pub(crate) fn outbound_proxy_applies_to_route(_profile: &ProviderProfile) -> bool {
    false
}

fn outbound_proxy_applies_to_url_with_matcher(url: &str, matcher: &SystemProxyMatcher) -> bool {
    url.parse::<WebSocketUri>()
        .ok()
        .is_some_and(|uri| matcher.intercept(&uri).is_some())
}

impl Drop for LocalRouter {
    fn drop(&mut self) {
        if let Ok(mut shutdown) = self.shutdown.lock()
            && let Some(shutdown) = shutdown.take()
        {
            let _ = shutdown.send(());
        }
        if let Ok(mut task) = self.task.lock()
            && let Some(task) = task.take()
        {
            task.abort();
        }
    }
}

#[derive(Clone)]
struct RouterServer {
    token: String,
    bearer_token: String,
    snapshot: Arc<RwLock<Arc<RouterSnapshot>>>,
    connection_limit: Arc<Semaphore>,
    rejection_limit: Arc<Semaphore>,
    request_body_budget: Arc<Semaphore>,
    bindings: Arc<Mutex<RouteBindings>>,
    websocket_backoffs: Arc<Mutex<UpstreamWebSocketBackoffs>>,
    client: reqwest::Client,
    official_auth_path: PathBuf,
    official_auth_cache: Arc<Mutex<crate::account_usage::OfficialAuthCache>>,
    request_log: Arc<RouteRequestLogController>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResponsesRequestKind {
    Create,
    Compact,
}

impl ResponsesRequestKind {
    fn label(self) -> &'static str {
        match self {
            Self::Create => "responses",
            Self::Compact => "responses_compact",
        }
    }
}

#[derive(Clone, Debug, Default)]
struct RouteBindings {
    routes: HashMap<String, RouteBinding>,
    order: VecDeque<(String, u64)>,
    next_generation: u64,
}

#[derive(Clone, Debug)]
struct RouteBinding {
    provider_id: String,
    generation: u64,
}

impl RouteBindings {
    fn route_for_keys(&self, keys: &[String]) -> Option<String> {
        // A concrete thread binding wins over its session-tree fallback. This
        // lets subagents self-route without changing the parent thread while
        // still giving metadata-free child/compaction requests a safe fallback.
        keys.iter().find_map(|key| {
            self.routes
                .get(key)
                .map(|binding| binding.provider_id.clone())
        })
    }

    fn remember(&mut self, keys: &[String], provider_id: &str, refresh_session_binding: bool) {
        for key in keys {
            if key.starts_with("session-id:")
                && self.routes.contains_key(key)
                && !refresh_session_binding
            {
                continue;
            }
            self.next_generation = self.next_generation.wrapping_add(1);
            let generation = self.next_generation;
            self.routes.insert(
                key.clone(),
                RouteBinding {
                    provider_id: provider_id.to_string(),
                    generation,
                },
            );
            self.order.push_back((key.clone(), generation));
        }
        while self.routes.len() > MAX_ROUTE_BINDINGS {
            let Some((expired, generation)) = self.order.pop_front() else {
                break;
            };
            if self
                .routes
                .get(&expired)
                .is_some_and(|binding| binding.generation == generation)
            {
                self.routes.remove(&expired);
            }
        }
        // Repeated turns refresh the same thread binding. Keep those updates
        // amortized O(1) and periodically collapse stale queue entries instead
        // of scanning the whole LRU on every request.
        if self.order.len() > MAX_ROUTE_BINDINGS * 4 {
            let mut live = self
                .routes
                .iter()
                .map(|(key, binding)| (key.clone(), binding.generation))
                .collect::<Vec<_>>();
            live.sort_unstable_by_key(|(_, generation)| *generation);
            self.order = live.into();
        }
    }
}

#[derive(Clone, Debug, Default)]
struct RouterSnapshot {
    routes: HashMap<String, Arc<RouteTarget>>,
    aliases: HashMap<String, AliasTarget>,
    raw_models: HashMap<String, Vec<AliasTarget>>,
    model_alias_history: BTreeMap<String, String>,
    model_ids: Vec<String>,
    default_model: String,
    request_log_backend: RouteRequestLogBackend,
    request_log_catalog: RequestLogCatalog,
}

impl RouterSnapshot {
    fn from_config(config: &CodeyConfig) -> Self {
        let mut routes = HashMap::new();
        let mut aliases = HashMap::new();
        let mut raw_models = HashMap::<String, Vec<AliasTarget>>::new();
        for profile in &config.profiles {
            if profile.official_account && !config.official_account_available_this_launch {
                continue;
            }
            let provider_id = profile.provider_id().trim();
            if provider_id.is_empty() {
                continue;
            }
            let base_url = if profile.official_account {
                CHATGPT_CODEX_BASE_URL.to_string()
            } else {
                profile.normalized_base_url()
            };
            if base_url.is_empty() {
                continue;
            }
            let protocol = UpstreamProtocol::from_profile(
                profile.official_account,
                &profile.upstream_protocol,
            );
            let mut target = RouteTarget {
                provider_id: provider_id.to_string(),
                route_name: profile.name.trim().to_string(),
                upstream_url: prepare_upstream_url(protocol, &base_url),
                upstream_compact_url: prepare_upstream_compact_url(protocol, &base_url),
                upstream_websocket_url: prepare_upstream_websocket_url(protocol, &base_url),
                upstream_headers: prepare_upstream_headers(profile, protocol),
                upstream_authority: upstream_authority(&base_url),
                protocol,
                official_account: profile.official_account,
                supports_websockets: protocol == UpstreamProtocol::OpenAiResponses
                    && config.route_supports_websockets_this_launch(profile),
                models: HashSet::new(),
            };
            for model in route_models(config, profile, provider_id) {
                let alias_target = AliasTarget {
                    provider_id: provider_id.to_string(),
                    model: model.clone(),
                };
                aliases.insert(
                    model_id::key(&model_alias(provider_id, &model)),
                    alias_target.clone(),
                );
                raw_models
                    .entry(model_id::key(&model))
                    .or_default()
                    .push(alias_target.clone());
                target.models.insert(model.clone());
            }
            routes.insert(provider_id.to_string(), Arc::new(target));
        }
        let mut model_ids = raw_models
            .values()
            .filter_map(|models| models.first().map(|model| model.model.clone()))
            .collect::<Vec<_>>();
        model_ids.sort_unstable();
        Self {
            routes,
            aliases,
            raw_models,
            model_alias_history: config.model_alias_history.clone(),
            model_ids,
            default_model: config.default_model().unwrap_or_default().to_string(),
            request_log_backend: config.route_request_log.backend,
            request_log_catalog: RequestLogCatalog::from_config(config),
        }
    }

    fn target_for_request(
        &self,
        requested_model: &str,
        route_hint: Option<&str>,
        bound_route: Option<&str>,
    ) -> Result<RouteSelection> {
        RouteResolver::new(self).resolve(RouteRequest {
            requested_model,
            route_hint,
            bound_route,
        })
    }

    fn target_for_auxiliary_request(
        &self,
        route_hint: Option<&str>,
        bound_route: Option<&str>,
    ) -> Result<Arc<RouteTarget>> {
        for provider_id in [route_hint, bound_route].into_iter().flatten() {
            if let Some(route) = self.routes.get(provider_id) {
                return Ok(Arc::clone(route));
            }
        }
        Ok(self
            .target_for_request(self.default_model.trim(), None, None)?
            .route)
    }

    fn resolve_request(&self, request: RouteRequest<'_>) -> Result<RouteSelection> {
        let requested_model = request.requested_model.trim();
        if requested_model.is_empty() {
            anyhow::bail!("请求缺少 model 字段");
        }
        if let Some(alias) = self.aliases.get(&model_id::key(requested_model)) {
            // A qualified `provider/model` selector already identifies the
            // route. Codex can replay client metadata from an earlier turn, so
            // an independent route hint must not redirect an explicit alias.
            return self.target_for_route_model(&alias.provider_id, &alias.model, requested_model);
        }
        if !self
            .raw_models
            .contains_key(&model_id::key(requested_model))
            && let Some(source_model) =
                model_id::historical_source(requested_model, &self.model_alias_history)
        {
            // Resolve a recorded upstream id as raw data, never recursively as
            // another selector (an upstream id can itself contain a slash).
            return self
                .resolve_raw_request(
                    source_model,
                    request.route_hint,
                    request.bound_route,
                    requested_model,
                )
                .with_context(|| {
                    format!(
                        "历史线路已不可用：{requested_model}；请为模型 {source_model} 选择可用线路"
                    )
                });
        }
        self.resolve_raw_request(
            requested_model,
            request.route_hint,
            request.bound_route,
            requested_model,
        )
    }

    fn resolve_raw_request(
        &self,
        model: &str,
        route_hint: Option<&str>,
        bound_route: Option<&str>,
        requested_model: &str,
    ) -> Result<RouteSelection> {
        let candidates = self
            .raw_models
            .get(&model_id::key(model))
            .map(Vec::as_slice)
            .unwrap_or_default();
        if let Some(route_hint) = route_hint
            && let Some(candidate) = candidates
                .iter()
                .find(|candidate| candidate.provider_id == route_hint)
        {
            return self.target_for_route_model(route_hint, &candidate.model, requested_model);
        }
        // Raw ids in the mixed runtime catalog are native OpenAI entries;
        // third-party selections remain route-qualified. An explicit hint
        // above can still select a third-party route with the same model.
        if !model_id::equal(model, CODEX_AUTO_REVIEW_MODEL)
            && let Some(official) = candidates.iter().find(|candidate| {
                self.routes
                    .get(&candidate.provider_id)
                    .is_some_and(|route| route.official_account)
            })
        {
            return self.target_for_route_model(
                &official.provider_id,
                &official.model,
                requested_model,
            );
        }
        // Codex can replay Responses client metadata from an earlier turn
        // after the sticky model has changed. An invalid hint therefore is
        // not sufficient evidence of a current route choice. Continue into
        // the bound/unique lookup; valid hints still win above, and equal
        // raw model ids on multiple routes still fail closed below.
        if let Some(bound_route) = bound_route
            && let Some(candidate) = candidates
                .iter()
                .find(|candidate| candidate.provider_id == bound_route)
        {
            return self.target_for_route_model(bound_route, &candidate.model, requested_model);
        }
        // Codex starts automatic approval review as a separate request with a
        // fixed hidden model. Some builds omit turn route metadata on that
        // request, so prefer the official route when no capable hint or thread
        // binding identified a route above. A capable bound third-party route
        // still wins before this fallback.
        if model_id::equal(model, CODEX_AUTO_REVIEW_MODEL)
            && let Some(official_route) = self.routes.values().find(|target| {
                target.official_account && target.models.contains(CODEX_AUTO_REVIEW_MODEL)
            })
        {
            return self.target_for_route_model(
                &official_route.provider_id,
                CODEX_AUTO_REVIEW_MODEL,
                requested_model,
            );
        }
        // A thread binding describes the route used by its previous turn,
        // not an explicit choice for every future model. When the user
        // changes models and the old route cannot serve it, continue into
        // the normal unique-candidate lookup below. Ambiguous raw ids still
        // fail closed, so this fallback never guesses between routes.
        if candidates.len() == 1 {
            let candidate = &candidates[0];
            return self.target_for_route_model(
                &candidate.provider_id,
                &candidate.model,
                requested_model,
            );
        }
        if candidates.len() > 1 {
            anyhow::bail!("模型 {requested_model} 同时存在于多条线路，缺少明确的 Codey 线路元数据");
        }
        anyhow::bail!("模型未在线路路由表中启用：{requested_model}")
    }

    #[cfg(test)]
    fn target_for_model(&self, requested_model: &str) -> Result<RouteSelection> {
        self.target_for_request(requested_model, None, None)
    }

    fn target_for_route_model(
        &self,
        provider_id: &str,
        model: &str,
        requested_model: &str,
    ) -> Result<RouteSelection> {
        let target = self
            .routes
            .get(provider_id)
            .ok_or_else(|| anyhow::anyhow!("线路已不存在：{provider_id}"))?;
        if !target.models.contains(model) {
            anyhow::bail!("线路「{}」未启用模型 {model}", route_display_name(target));
        }
        Ok(RouteSelection {
            provider_id: target.provider_id.clone(),
            protocol: target.protocol,
            requested_model: requested_model.to_string(),
            route: Arc::clone(target),
            upstream_model: model.to_string(),
        })
    }

    fn model_ids(&self) -> &[String] {
        &self.model_ids
    }

    #[cfg(test)]
    fn model_aliases(&self) -> Vec<String> {
        let mut aliases = self.aliases.keys().cloned().collect::<Vec<_>>();
        aliases.sort_unstable();
        aliases
    }
}

#[derive(Clone, Debug)]
struct RouteTarget {
    provider_id: String,
    route_name: String,
    upstream_url: std::result::Result<String, String>,
    upstream_compact_url: std::result::Result<String, String>,
    upstream_websocket_url: std::result::Result<String, String>,
    upstream_headers: std::result::Result<HeaderMap, String>,
    upstream_authority: String,
    protocol: UpstreamProtocol,
    official_account: bool,
    supports_websockets: bool,
    models: HashSet<String>,
}

#[derive(Clone, Debug)]
struct AliasTarget {
    provider_id: String,
    model: String,
}

#[derive(Clone, Debug)]
struct RouteSelection {
    provider_id: String,
    protocol: UpstreamProtocol,
    requested_model: String,
    route: Arc<RouteTarget>,
    upstream_model: String,
}

#[derive(Clone, Copy, Debug)]
struct RouteRequest<'a> {
    requested_model: &'a str,
    route_hint: Option<&'a str>,
    bound_route: Option<&'a str>,
}

struct RouteResolver<'a> {
    snapshot: &'a RouterSnapshot,
}

impl<'a> RouteResolver<'a> {
    fn new(snapshot: &'a RouterSnapshot) -> Self {
        Self { snapshot }
    }

    fn resolve(&self, request: RouteRequest<'_>) -> Result<RouteSelection> {
        self.snapshot.resolve_request(request)
    }
}

fn route_models(
    config: &CodeyConfig,
    profile: &crate::config::ProviderProfile,
    provider_id: &str,
) -> Vec<String> {
    let mut models = if profile.official_account {
        config.enabled_official_route_models(provider_id)
    } else {
        config.enabled_route_models(provider_id)
    };
    let supports_auto_review = profile.official_account || profile.supports_auto_review;
    if supports_auto_review
        && !models
            .iter()
            .any(|model| model.eq_ignore_ascii_case(CODEX_AUTO_REVIEW_MODEL))
    {
        models.push(CODEX_AUTO_REVIEW_MODEL.to_string());
    }
    models
}

pub(crate) fn model_alias(provider_id: &str, model: &str) -> String {
    format!("{}/{}", encode_alias_component(provider_id), model.trim())
}

fn encode_alias_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.trim().bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    encoded
}

impl RouterServer {
    async fn handle_connection(&self, mut stream: TcpStream) -> Result<()> {
        if request_looks_like_responses_websocket(&stream).await? {
            return self.handle_responses_websocket(stream).await;
        }
        let pending =
            match tokio::time::timeout(REQUEST_READ_TIMEOUT, read_http_request_head(&mut stream))
                .await
            {
                Ok(Ok(request)) => request,
                Ok(Err(error)) => {
                    write_error_response(
                        &mut stream,
                        400,
                        "invalid_http_request",
                        format!("本地路由请求无效：{error:#}"),
                        None,
                    )
                    .await?;
                    return Ok(());
                }
                Err(_) => {
                    write_error_response(
                        &mut stream,
                        408,
                        "request_timeout",
                        "读取本地路由请求超时",
                        None,
                    )
                    .await?;
                    return Ok(());
                }
            };
        if pending.request.path == "/healthz" {
            write_json_response(&mut stream, 200, &json!({"status":"ok"})).await?;
            return Ok(());
        }
        if pending.request.method == "GET" && pending.request.path == REQUEST_LOG_PAGE_PATH {
            write_static_response(
                &mut stream,
                "text/html; charset=utf-8",
                REQUEST_LOG_PAGE.as_bytes(),
            )
            .await?;
            return Ok(());
        }
        if pending.request.method == "GET" && pending.request.path == REQUEST_LOG_SCRIPT_PATH {
            write_static_response(
                &mut stream,
                "text/javascript; charset=utf-8",
                crate::cdp::SETTINGS_OVERLAY_SCRIPT.as_bytes(),
            )
            .await?;
            return Ok(());
        }
        if !self.authorized(&pending.request) {
            write_error_response(
                &mut stream,
                401,
                "invalid_router_token",
                "Codey 本地路由认证失败",
                None,
            )
            .await?;
            return Ok(());
        }
        let request = match tokio::time::timeout(
            REQUEST_READ_TIMEOUT,
            read_http_request_body_with_budget(
                &mut stream,
                pending,
                Some(&self.request_body_budget),
            ),
        )
        .await
        {
            Ok(Ok(request)) => request,
            Ok(Err(error))
                if error
                    .downcast_ref::<RequestBodyBudgetUnavailable>()
                    .is_some() =>
            {
                write_error_response(
                    &mut stream,
                    503,
                    "router_memory_busy",
                    "Codey 本地路由请求缓冲区已满，请稍后重试",
                    None,
                )
                .await?;
                return Ok(());
            }
            Ok(Err(error)) => {
                write_error_response(
                    &mut stream,
                    400,
                    "invalid_http_request",
                    format!("本地路由请求无效：{error:#}"),
                    None,
                )
                .await?;
                return Ok(());
            }
            Err(_) => {
                write_error_response(
                    &mut stream,
                    408,
                    "request_timeout",
                    "读取本地路由请求超时",
                    None,
                )
                .await?;
                return Ok(());
            }
        };
        let route_path = request.path.as_str();
        match (request.method.as_str(), route_path) {
            ("GET", "/v1/models") | ("GET", "/models") => {
                let snapshot = Arc::clone(
                    &self
                        .snapshot
                        .read()
                        .expect("local router snapshot lock poisoned"),
                );
                let data = snapshot
                    .model_ids()
                    .iter()
                    .map(|id| json!({"id":id,"object":"model","owned_by":"codey"}))
                    .collect::<Vec<_>>();
                write_json_response(&mut stream, 200, &json!({"object":"list","data":data}))
                    .await?;
            }
            ("POST", "/codey/api/load_codey_config") => {
                let catalog = self
                    .snapshot
                    .read()
                    .expect("local router snapshot lock poisoned")
                    .request_log_catalog
                    .clone();
                write_json_response(&mut stream, 200, &json!({"config": catalog})).await?;
            }
            ("POST", "/codey/api/query_route_request_logs") => {
                let query = match serde_json::from_slice::<RouteRequestLogQuery>(&request.body) {
                    Ok(query) => query,
                    Err(error) => {
                        write_error_response(
                            &mut stream,
                            400,
                            "invalid_request_log_query",
                            format!("请求日志查询参数无效：{error}"),
                            None,
                        )
                        .await?;
                        return Ok(());
                    }
                };
                let backend = self
                    .snapshot
                    .read()
                    .expect("local router snapshot lock poisoned")
                    .request_log_backend;
                let root = codey_runtime_core::paths::default_app_state_dir();
                match tokio::task::spawn_blocking(move || {
                    crate::route_request_log::query_route_request_logs(&root, backend, query)
                })
                .await
                {
                    Ok(Ok(page)) => {
                        write_json_response(
                            &mut stream,
                            200,
                            &serde_json::to_value(page).context("序列化请求日志查询结果失败")?,
                        )
                        .await?;
                    }
                    Ok(Err(error)) => {
                        write_error_response(
                            &mut stream,
                            500,
                            "request_log_query_failed",
                            format!("查询请求日志失败：{error:#}"),
                            None,
                        )
                        .await?;
                    }
                    Err(error) => {
                        write_error_response(
                            &mut stream,
                            500,
                            "request_log_query_failed",
                            format!("请求日志查询任务异常退出：{error}"),
                            None,
                        )
                        .await?;
                    }
                }
            }
            ("POST", "/codey/api/clear_route_request_logs") => {
                write_json_response(
                    &mut stream,
                    200,
                    &serde_json::to_value(self.request_log.clear().await)
                        .context("序列化请求日志清理结果失败")?,
                )
                .await?;
            }
            ("POST", "/v1/responses") | ("POST", "/responses") => {
                self.proxy_responses(request, stream, ResponsesRequestKind::Create)
                    .await?;
            }
            ("POST", "/v1/images/generations") | ("POST", "/images/generations") => {
                self.proxy_image_generation(request, stream).await?;
            }
            ("POST", "/v1/responses/compact")
            | ("POST", "/responses/compact")
            | ("POST", "/v1/v1/responses/compact")
            | ("POST", "/codex/v1/responses/compact") => {
                self.proxy_responses(request, stream, ResponsesRequestKind::Compact)
                    .await?;
            }
            _ => {
                write_error_response(
                    &mut stream,
                    404,
                    "route_not_found",
                    "Codey 本地路由不支持该路径",
                    None,
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn proxy_image_generation(
        &self,
        mut request: HttpRequest,
        mut stream: TcpStream,
    ) -> Result<()> {
        let mut body = match serde_json::from_slice::<Value>(&request.body) {
            Ok(body) if body.is_object() => body,
            Ok(_) => {
                write_error_response(
                    &mut stream,
                    400,
                    "invalid_request_body",
                    "Images 请求体必须是 JSON 对象",
                    None,
                )
                .await?;
                return Ok(());
            }
            Err(error) => {
                write_error_response(
                    &mut stream,
                    400,
                    "invalid_request_body",
                    format!("Images 请求体不是有效 JSON：{error}"),
                    None,
                )
                .await?;
                return Ok(());
            }
        };
        let (route_hint, body_mutated) = match take_codey_route_metadata(&mut request, &mut body) {
            Ok(extracted) => extracted,
            Err(error) => {
                write_error_response(
                    &mut stream,
                    400,
                    "route_metadata_invalid",
                    format!("Codey 线路元数据无效：{error:#}"),
                    None,
                )
                .await?;
                return Ok(());
            }
        };
        let snapshot = Arc::clone(
            &self
                .snapshot
                .read()
                .expect("local router snapshot lock poisoned"),
        );
        let binding_keys = request_binding_keys(&request);
        let bound_route = self
            .bindings
            .lock()
            .expect("local router bindings mutex poisoned")
            .route_for_keys(&binding_keys);
        let route = match snapshot
            .target_for_auxiliary_request(route_hint.as_deref(), bound_route.as_deref())
        {
            Ok(route) => route,
            Err(error) => {
                write_error_response(
                    &mut stream,
                    404,
                    "route_not_enabled",
                    format!("图片生成请求没有可用线路：{error:#}"),
                    None,
                )
                .await?;
                return Ok(());
            }
        };
        if route.protocol == UpstreamProtocol::AnthropicMessages {
            write_error_response(
                &mut stream,
                400,
                "image_generation_not_supported",
                format!(
                    "线路「{}」使用 Anthropic Messages，不能转发 OpenAI Images 请求",
                    route_display_name(&route)
                ),
                Some(&route),
            )
            .await?;
            return Ok(());
        }
        let upstream_base_url = match &route.upstream_url {
            Ok(url) => url,
            Err(error) => {
                write_error_response(
                    &mut stream,
                    502,
                    "route_configuration_error",
                    format!("线路「{}」的 {error}", route_display_name(&route)),
                    Some(&route),
                )
                .await?;
                return Ok(());
            }
        };
        let upstream_url = match image_generation_endpoint(upstream_base_url) {
            Ok(url) => url,
            Err(error) => {
                write_error_response(
                    &mut stream,
                    502,
                    "route_configuration_error",
                    format!(
                        "线路「{}」的 Images API URL 无效：{error:#}",
                        route_display_name(&route)
                    ),
                    Some(&route),
                )
                .await?;
                return Ok(());
            }
        };
        let headers = match self
            .prepare_upstream_request_headers(&request, &route)
            .await
        {
            Ok(headers) => headers,
            Err((status, code, message)) => {
                write_error_response(&mut stream, status, code, message, Some(&route)).await?;
                return Ok(());
            }
        };
        let stream_requested = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
        let request_builder = self
            .client
            .post(&upstream_url)
            .headers(headers)
            .header(CONTENT_TYPE, "application/json")
            .body(if body_mutated {
                serde_json::to_vec(&body).context("序列化 Images 上游请求失败")?
            } else {
                request.body
            });
        let response_header_timeout = if stream_requested {
            UPSTREAM_RESPONSE_HEADER_TIMEOUT
        } else {
            UPSTREAM_NON_STREAM_RESPONSE_HEADER_TIMEOUT
        };
        let response =
            match tokio::time::timeout(response_header_timeout, request_builder.send()).await {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => {
                    let timeout = error.is_timeout();
                    let (status, code, message) = if timeout {
                        (
                            504,
                            "upstream_timeout",
                            format!(
                                "Codey 线路「{}」请求图片生成上游超时",
                                route_display_name(&route)
                            ),
                        )
                    } else {
                        (
                            424,
                            "upstream_unreachable",
                            format!(
                                "Codey 线路「{}」无法连接图片生成上游",
                                route_display_name(&route)
                            ),
                        )
                    };
                    write_text_error_response(&mut stream, status, code, message).await?;
                    return Ok(());
                }
                Err(_) => {
                    write_text_error_response(
                        &mut stream,
                        504,
                        "upstream_header_timeout",
                        format!(
                            "Codey 线路「{}」等待图片生成上游返回响应头超时",
                            route_display_name(&route)
                        ),
                    )
                    .await?;
                    return Ok(());
                }
            };
        write_proxy_response(&mut stream, response, None).await
    }

    async fn prepare_upstream_request_headers(
        &self,
        request: &HttpRequest,
        route: &RouteTarget,
    ) -> std::result::Result<HeaderMap, (u16, &'static str, String)> {
        let prepared_headers = route
            .upstream_headers
            .as_ref()
            .map_err(|error| (502, "route_configuration_error", error.clone()))?;
        let mut headers = HeaderMap::with_capacity(request.headers.len() + prepared_headers.len());
        for (name, value) in &request.headers {
            if should_forward_incoming_header(name, route.official_account)
                && let (Ok(name), Ok(value)) = (
                    HeaderName::from_bytes(name.as_bytes()),
                    HeaderValue::from_str(value),
                )
            {
                headers.insert(name, value);
            }
        }
        for (name, value) in prepared_headers {
            headers.insert(name, value.clone());
        }
        if let Some(request_id) = current_router_request_id()
            && let Ok(value) = HeaderValue::from_str(&request_id)
        {
            headers.insert(HeaderName::from_static("x-codey-request-id"), value);
        }
        if route.official_account {
            let official_auth = resolve_official_upstream_auth(
                request,
                &self.bearer_token,
                &self.official_auth_path,
                &self.official_auth_cache,
            )
            .await
            .ok_or_else(|| {
                (
                    401,
                    "openai_auth_missing",
                    "官方账号线路缺少 Codex OpenAI 登录态，请重新登录后重试".to_string(),
                )
            })?;
            let value = HeaderValue::from_str(&official_auth.authorization).map_err(|_| {
                (
                    401,
                    "openai_auth_invalid",
                    "官方账号线路的 Codex OpenAI 登录态无效，请重新登录后重试".to_string(),
                )
            })?;
            headers.insert(AUTHORIZATION, value);
            headers.remove(CHATGPT_ACCOUNT_ID_HEADER);
            if let Some(account_id) = official_auth.account_id.as_deref()
                && let Ok(value) = HeaderValue::from_str(account_id)
            {
                headers.insert(HeaderName::from_static(CHATGPT_ACCOUNT_ID_HEADER), value);
            }
        }
        Ok(headers)
    }

    fn authorized(&self, request: &HttpRequest) -> bool {
        request.headers.iter().any(|(name, value)| {
            (name.eq_ignore_ascii_case(ROUTER_AUTH_HEADER)
                && constant_time_eq(value.trim().as_bytes(), self.token.as_bytes()))
                || (name.eq_ignore_ascii_case("authorization")
                    && constant_time_eq(value.trim().as_bytes(), self.bearer_token.as_bytes()))
        })
    }

    // Tungstenite's handshake callback fixes the error type to an HTTP
    // response value; its size is imposed by the external Callback contract.
    #[allow(clippy::result_large_err)]
    async fn handle_responses_websocket(&self, stream: TcpStream) -> Result<()> {
        let handshake_context = Arc::new(Mutex::new(None));
        let captured_context = Arc::clone(&handshake_context);
        let token = self.token.clone();
        let bearer_token = self.bearer_token.clone();
        let request_id = current_router_request_id();
        let websocket_config = WebSocketConfig::default()
            // Responses events are latency-sensitive and already framed. Do
            // not wait for tungstenite's default 128 KiB write threshold.
            .write_buffer_size(0)
            .max_write_buffer_size(MAX_UPSTREAM_RESPONSE_BYTES)
            .max_message_size(Some(MAX_REQUEST_BYTES))
            .max_frame_size(Some(MAX_REQUEST_BYTES));
        let socket = tokio::time::timeout(
            REQUEST_READ_TIMEOUT,
            accept_hdr_async_with_config(
                stream,
                move |request: &WebSocketRequest, mut response: WebSocketResponse| {
                    if !RESPONSES_WEBSOCKET_PATHS.contains(&request.uri().path()) {
                        return Err(websocket_handshake_error(
                            WebSocketStatusCode::NOT_FOUND,
                            "Codey 本地路由不支持该 WebSocket 路径",
                        ));
                    }
                    if !websocket_request_authorized(request, &token, &bearer_token) {
                        return Err(websocket_handshake_error(
                            WebSocketStatusCode::UNAUTHORIZED,
                            "Codey 本地路由 WebSocket 认证失败",
                        ));
                    }
                    *captured_context
                        .lock()
                        .expect("local router websocket handshake context poisoned") =
                        Some(WebSocketRequestContext {
                            headers: websocket_forward_headers(request),
                        });
                    if let Some(request_id) = request_id.as_deref()
                        && let Ok(value) = request_id.parse()
                    {
                        response.headers_mut().insert("x-codey-request-id", value);
                    }
                    response.headers_mut().insert(
                        "openai-beta",
                        HeaderValue::from_static(RESPONSES_WEBSOCKET_BETA),
                    );
                    Ok(response)
                },
                Some(websocket_config),
            ),
        )
        .await
        .context("Codey Responses WebSocket 握手超时")?
        .context("Codey Responses WebSocket 握手失败")?;
        let context = handshake_context
            .lock()
            .expect("local router websocket handshake context poisoned")
            .take()
            .context("Codey Responses WebSocket 缺少握手上下文")?;
        let mut downstream = WebSocketResponsesDownstream::with_shared_backoffs(
            socket,
            Arc::clone(&self.websocket_backoffs),
            Arc::clone(&self.request_body_budget),
        );

        while let Some(message) = downstream.next_message().await? {
            downstream.clear_stream_id();
            match message {
                WebSocketMessage::Text(text) => {
                    let body_budget_permit =
                        match acquire_request_body_budget(&self.request_body_budget, text.len()) {
                            Ok(permit) => permit,
                            Err(error)
                                if error
                                    .downcast_ref::<RequestBodyBudgetUnavailable>()
                                    .is_some() =>
                            {
                                downstream
                                    .write_error(
                                        503,
                                        "router_memory_busy",
                                        "Codey 本地路由请求缓冲区已满，请稍后重试".to_string(),
                                        None,
                                    )
                                    .await?;
                                continue;
                            }
                            Err(error) => return Err(error),
                        };
                    let mut body = match serde_json::from_str::<Value>(text.as_str()) {
                        Ok(Value::Object(body)) => Value::Object(body),
                        Ok(_) => {
                            downstream
                                .write_error(
                                    400,
                                    "invalid_request_body",
                                    "Responses WebSocket 消息必须是 JSON 对象".to_string(),
                                    None,
                                )
                                .await?;
                            continue;
                        }
                        Err(error) => {
                            downstream
                                .write_error(
                                    400,
                                    "invalid_request_body",
                                    format!("Responses WebSocket 消息不是有效 JSON：{error}"),
                                    None,
                                )
                                .await?;
                            continue;
                        }
                    };
                    let message_type = body
                        .as_object_mut()
                        .and_then(|body| body.remove("type"))
                        .and_then(|value| value.as_str().map(str::to_string));
                    if message_type.as_deref() != Some("response.create") {
                        downstream
                            .write_error(
                                400,
                                "unsupported_websocket_message",
                                "Codey Responses WebSocket 仅支持 response.create".to_string(),
                                None,
                            )
                            .await?;
                        continue;
                    }
                    let stream_id = match responses_websocket_stream_id(&body) {
                        Ok(stream_id) => stream_id,
                        Err(error) => {
                            downstream
                                .write_error(
                                    400,
                                    "invalid_stream_id",
                                    format!("Responses WebSocket stream_id 无效：{error:#}"),
                                    None,
                                )
                                .await?;
                            continue;
                        }
                    };
                    if body
                        .get("stream")
                        .is_some_and(|stream| stream.as_bool() != Some(true))
                    {
                        downstream
                            .write_error(
                                400,
                                "websocket_stream_required",
                                "Responses WebSocket 的 stream 字段只能省略或设为 true".to_string(),
                                None,
                            )
                            .await?;
                        continue;
                    }
                    if body
                        .get("background")
                        .is_some_and(|background| background.as_bool() != Some(false))
                    {
                        downstream
                            .write_error(
                                400,
                                "websocket_background_unsupported",
                                "Responses WebSocket 不支持 background 模式".to_string(),
                                None,
                            )
                            .await?;
                        continue;
                    }
                    if let Some(body) = body.as_object_mut() {
                        // These are HTTP transport fields. The shared proxy
                        // path restores `stream = true` only for an HTTP/SSE
                        // fallback and never forwards either field over WS.
                        body.remove("stream");
                        body.remove("background");
                    }
                    downstream.set_stream_id(stream_id);
                    let request = HttpRequest {
                        method: "POST".to_string(),
                        path: "/v1/responses".to_string(),
                        headers: context.headers.clone(),
                        body: Vec::new(),
                        _body_budget_permit: body_budget_permit,
                    };
                    let request_id = Uuid::new_v4().simple().to_string();
                    let result = ROUTER_REQUEST_ID
                        .scope(
                            request_id.clone(),
                            ROUTER_REQUEST_STARTED_AT.scope(
                                Instant::now(),
                                self.proxy_parsed_responses(
                                    request,
                                    body,
                                    None,
                                    ResponsesRequestKind::Create,
                                    &mut downstream,
                                ),
                            ),
                        )
                        .await;
                    if let Err(error) = result {
                        if error.is::<DownstreamClosed>() {
                            // Reading a Close queues tungstenite's close reply.
                            // Drop the proxy future/upstream before flushing it.
                            let _ = tokio::time::timeout(
                                DOWNSTREAM_WRITE_TIMEOUT,
                                downstream.socket.flush(),
                            )
                            .await;
                            break;
                        }
                        record_router_failure_nonblocking(
                            "local_router_websocket_request_failed",
                            "proxy_local_router_websocket_request",
                            format!("{error:#}"),
                            serde_json::json!({ "requestId": request_id }),
                        );
                        if !downstream.terminal_started {
                            downstream
                                .write_error(
                                    502,
                                    "websocket_proxy_failed",
                                    "Codey 本地路由处理 WebSocket 请求失败".to_string(),
                                    None,
                                )
                                .await?;
                        }
                    }
                }
                WebSocketMessage::Ping(payload) => downstream.write_pong(payload).await?,
                WebSocketMessage::Pong(_) => {}
                WebSocketMessage::Close(frame) => {
                    downstream.close(frame).await?;
                    break;
                }
                WebSocketMessage::Binary(_) | WebSocketMessage::Frame(_) => {
                    downstream
                        .write_error(
                            400,
                            "unsupported_websocket_message",
                            "Codey Responses WebSocket 仅接受 JSON 文本消息".to_string(),
                            None,
                        )
                        .await?;
                }
            }
        }
        Ok(())
    }

    async fn proxy_responses(
        &self,
        mut request: HttpRequest,
        stream: TcpStream,
        request_kind: ResponsesRequestKind,
    ) -> Result<()> {
        let encoded_body =
            match decode_responses_request_body(&mut request, &self.request_body_budget).await {
                Ok(body) => body,
                Err(error)
                    if error
                        .downcast_ref::<RequestBodyBudgetUnavailable>()
                        .is_some() =>
                {
                    let mut downstream = HttpResponsesDownstream::new(stream);
                    downstream
                        .write_error(
                            503,
                            "router_memory_busy",
                            "Codey 本地路由请求缓冲区已满，请稍后重试".to_string(),
                            None,
                        )
                        .await?;
                    return Ok(());
                }
                Err(error)
                    if error
                        .downcast_ref::<UnsupportedRequestContentEncoding>()
                        .is_some() =>
                {
                    let mut downstream = HttpResponsesDownstream::new(stream);
                    downstream
                        .write_error(415, "unsupported_content_encoding", error.to_string(), None)
                        .await?;
                    return Ok(());
                }
                Err(error) => {
                    let mut downstream = HttpResponsesDownstream::new(stream);
                    downstream
                        .write_error(
                            400,
                            "invalid_request_body",
                            format!("Responses 请求体解码失败：{error:#}"),
                            None,
                        )
                        .await?;
                    return Ok(());
                }
            };
        let mut downstream = HttpResponsesDownstream::new(stream);
        let (encoded_body, parsed_body) = match parse_responses_request_body(encoded_body).await {
            Ok(parsed) => parsed,
            Err(error) => {
                downstream
                    .write_error(
                        500,
                        "request_parse_failed",
                        format!("Responses 请求解析任务失败：{error:#}"),
                        None,
                    )
                    .await?;
                return Ok(());
            }
        };
        let body = match parsed_body {
            Ok(body) if body.is_object() => body,
            Ok(_) => {
                downstream
                    .write_error(
                        400,
                        "invalid_request_body",
                        "Responses 请求体必须是 JSON 对象".to_string(),
                        None,
                    )
                    .await?;
                return Ok(());
            }
            Err(error) => {
                downstream
                    .write_error(
                        400,
                        "invalid_request_body",
                        format!("Responses 请求体不是有效 JSON：{error}"),
                        None,
                    )
                    .await?;
                return Ok(());
            }
        };
        self.proxy_parsed_responses(
            request,
            body,
            Some(encoded_body),
            request_kind,
            &mut downstream,
        )
        .await
    }

    async fn proxy_parsed_responses<D>(
        &self,
        request: HttpRequest,
        body: Value,
        encoded_body: Option<Vec<u8>>,
        request_kind: ResponsesRequestKind,
        downstream: &mut D,
    ) -> Result<()>
    where
        D: ResponsesDownstream + ?Sized,
    {
        let probe = self.request_log.begin(|producer| {
            let request_id = current_router_request_id().unwrap_or_default();
            let (codex_session_id, codex_session_is_parent) = request_log_codex_session(&request);
            let downstream_websocket = downstream.is_websocket();
            let stream_requested = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
            let request_protocol = if downstream_websocket {
                RequestProtocol::WebSocket
            } else if stream_requested {
                RequestProtocol::Sse
            } else {
                RequestProtocol::Http
            };
            let requested_model = body
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let reasoning_effort = body
                .pointer("/reasoning/effort")
                .or_else(|| body.get("reasoning_effort"))
                .and_then(Value::as_str);
            let thinking_budget_tokens = [
                "/thinking/budget_tokens",
                "/thinking/budgetTokens",
                "/thinking_budget_tokens",
                "/thinkingBudgetTokens",
            ]
            .into_iter()
            .find_map(|pointer| body.pointer(pointer).and_then(Value::as_u64));
            producer.begin(RouteRequestLogStart {
                request_id: &request_id,
                started_at: current_router_request_started_at().unwrap_or_else(Instant::now),
                request_protocol,
                request_kind: request_kind.label(),
                requested_model,
                reasoning_effort,
                thinking_budget_tokens,
                codex_session_id,
                codex_session_is_parent,
            })
        });
        if probe.is_none() {
            return self
                .proxy_parsed_responses_inner(request, body, encoded_body, request_kind, downstream)
                .await;
        }
        let _request_log_guard = RouteRequestLogGuard::new(probe.clone());
        let mut observed = ObservedResponsesDownstream::new(downstream, probe);
        self.proxy_parsed_responses_inner(request, body, encoded_body, request_kind, &mut observed)
            .await
    }

    async fn proxy_parsed_responses_inner<D>(
        &self,
        mut request: HttpRequest,
        mut body: Value,
        mut encoded_body: Option<Vec<u8>>,
        request_kind: ResponsesRequestKind,
        downstream: &mut D,
    ) -> Result<()>
    where
        D: ResponsesDownstream + ?Sized,
    {
        let downstream_websocket = downstream.is_websocket();
        if downstream_websocket {
            debug_assert_eq!(request_kind, ResponsesRequestKind::Create);
            body.as_object_mut()
                .expect("validated Responses body must remain an object")
                .remove("stream_id");
            // HTTP/SSE is the deterministic fallback for a downstream WS
            // request, so the shared proxy path always asks an HTTP upstream
            // to stream. `stream_id` is local to the downstream Codex socket
            // and is reattached only to events written back to that socket.
            body.as_object_mut()
                .expect("validated Responses body must remain an object")
                .insert("stream".to_string(), Value::Bool(true));
        }
        let snapshot = Arc::clone(
            &self
                .snapshot
                .read()
                .expect("local router snapshot lock poisoned"),
        );
        let requested_model = body
            .as_object()
            .and_then(|body| body.get("model"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let model_was_defaulted = requested_model.is_empty();
        let model = if model_was_defaulted {
            snapshot.default_model.trim().to_string()
        } else {
            requested_model
        };
        if model.is_empty() {
            downstream
                .write_error(
                    400,
                    "model_required",
                    "Responses 请求缺少有效的 model 字段".to_string(),
                    None,
                )
                .await?;
            return Ok(());
        }
        if model_was_defaulted {
            body.as_object_mut()
                .expect("validated Responses body must remain an object")
                .insert("model".to_string(), Value::String(model.clone()));
        }
        let (route_hint, mut body_mutated) =
            match take_codey_route_metadata(&mut request, &mut body) {
                Ok(extracted) => extracted,
                Err(error) => {
                    downstream
                        .write_error(
                            400,
                            "route_metadata_invalid",
                            format!("Codey 线路元数据无效：{error:#}"),
                            None,
                        )
                        .await?;
                    return Ok(());
                }
            };
        body_mutated |= model_was_defaulted;
        let subagent_request = request_is_subagent(&request);
        let binding_keys = request_binding_keys(&request);
        // Route lookup and binding refresh are both synchronous hash lookups.
        // Keeping them under one short critical section halves mutex traffic on
        // the request hot path without holding the lock across any I/O.
        let resolved = {
            let mut bindings = self
                .bindings
                .lock()
                .expect("local router bindings mutex poisoned");
            let bound_route = bindings.route_for_keys(&binding_keys);
            let resolved =
                snapshot.target_for_request(&model, route_hint.as_deref(), bound_route.as_deref());
            if let Ok(resolved) = &resolved {
                let refresh_session_binding = route_hint.is_some() && !subagent_request;
                bindings.remember(
                    &binding_keys,
                    &resolved.provider_id,
                    refresh_session_binding,
                );
            }
            resolved
        };
        let resolved = match resolved {
            Ok(resolved) => resolved,
            Err(error) => {
                downstream
                    .write_error(404, "model_not_enabled", format!("{error:#}"), None)
                    .await?;
                return Ok(());
            }
        };
        if model != resolved.upstream_model {
            body.as_object_mut()
                .expect("validated Responses body must remain an object")
                .insert(
                    "model".to_string(),
                    Value::String(resolved.upstream_model.clone()),
                );
            body_mutated = true;
        }
        if !resolved.route.official_account && normalize_responses_tool_parameter_roots(&mut body) {
            body_mutated = true;
            encoded_body = None;
        }
        let stream_requested = body
            .as_object()
            .and_then(|body| body.get("stream"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let Some(probe) = downstream.request_log_probe() {
            probe.set_request_protocol(if downstream_websocket {
                RequestProtocol::WebSocket
            } else if stream_requested {
                RequestProtocol::Sse
            } else {
                RequestProtocol::Http
            });
        }
        let bridge = ProtocolBridge::from_upstream_protocol(resolved.protocol);
        if let Some(probe) = downstream.request_log_probe() {
            probe.resolve_route(
                &resolved.provider_id,
                &resolved.route.route_name,
                &resolved.requested_model,
                &resolved.upstream_model,
                &resolved.route.upstream_authority,
                bridge.upstream_protocol().label(),
                bridge.label(),
                subagent_request,
            );
        }
        if bridge != ProtocolBridge::NativeResponses {
            downstream.prepare_adapted_response_context(&mut body);
        }
        if bridge == ProtocolBridge::NativeResponses
            && remove_codey_synthetic_previous_response_id(&mut body)
        {
            body_mutated = true;
        }
        let force_upstream_stream = should_force_upstream_streaming(
            bridge,
            request_kind,
            downstream_websocket,
            stream_requested,
        );
        if force_upstream_stream {
            body.as_object_mut()
                .expect("validated Responses body must remain an object")
                .insert("stream".to_string(), Value::Bool(true));
            body_mutated = true;
        }
        let upstream_url = match request_kind {
            ResponsesRequestKind::Create => &resolved.route.upstream_url,
            ResponsesRequestKind::Compact => &resolved.route.upstream_compact_url,
        };
        let upstream_url = match upstream_url {
            Ok(upstream_url) => upstream_url.as_str(),
            Err(error) => {
                downstream
                    .write_error(
                        502,
                        "route_configuration_error",
                        format!("线路「{}」的 {error}", route_display_name(&resolved.route)),
                        Some(&resolved.route),
                    )
                    .await?;
                return Ok(());
            }
        };
        let mut tool_bridge = ResponsesToolBridge::default();
        let offload_conversion = bridge != ProtocolBridge::NativeResponses
            && encoded_body
                .as_ref()
                .is_some_and(|body| body.len() >= REQUEST_JSON_OFFLOAD_BYTES);
        let (body, converted) = if offload_conversion {
            match tokio::task::spawn_blocking(move || {
                let converted = bridge.convert_responses_body(&body);
                (body, converted)
            })
            .await
            {
                Ok(converted) => converted,
                Err(error) => {
                    downstream
                        .write_error(
                            500,
                            "request_conversion_failed",
                            format!("等待 Responses 协议转换任务失败：{error}"),
                            Some(&resolved.route),
                        )
                        .await?;
                    return Ok(());
                }
            }
        } else {
            let converted = bridge.convert_responses_body(&body);
            (body, converted)
        };
        let mut upstream_body = match converted {
            Ok(converted) => {
                if let Some(converted) = converted {
                    tool_bridge = converted.tool_bridge;
                    converted.body
                } else {
                    body
                }
            }
            Err(error) => {
                downstream
                    .write_error(
                        400,
                        "unsupported_responses_payload",
                        format!(
                            "线路「{}」选择了 {}，但当前请求无法转换：{error:#}",
                            route_display_name(&resolved.route),
                            bridge.upstream_protocol().label()
                        ),
                        Some(&resolved.route),
                    )
                    .await?;
                return Ok(());
            }
        };
        let mut headers = match self
            .prepare_upstream_request_headers(&request, &resolved.route)
            .await
        {
            Ok(headers) => headers,
            Err((status, code, message)) => {
                downstream
                    .write_error(status, code, message, Some(&resolved.route))
                    .await?;
                return Ok(());
            }
        };
        if bridge == ProtocolBridge::NativeResponses {
            ensure_native_prompt_cache_key(
                &mut headers,
                &upstream_body,
                &resolved.provider_id,
                upstream_url,
                &resolved.upstream_model,
            );
        }
        // Every downstream socket owns its upstream WebSocket cache. Subagents
        // therefore keep incremental `previous_response_id` state on their own
        // upstream connection without sharing the main agent's connection.
        if downstream_websocket
            && request_kind == ResponsesRequestKind::Create
            && stream_requested
            && bridge == ProtocolBridge::NativeResponses
            && resolved.route.supports_websockets
        {
            let websocket_attempt = downstream
                .try_proxy_upstream_websocket(&resolved.route, &headers, &mut upstream_body)
                .await?;
            if websocket_attempt == UpstreamWebSocketAttempt::Completed {
                return Ok(());
            }
            if let Some(probe) = downstream.request_log_probe() {
                probe.mark_fallback("websocket_to_http_sse");
            }
        }
        let upstream_stream_requested = upstream_body
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let fallback_headers =
            (subagent_request && upstream_stream_requested).then(|| headers.clone());
        let mut request_builder = self.client.post(upstream_url).headers(headers);
        request_builder = if bridge == ProtocolBridge::NativeResponses {
            // Native HTTP requests keep large input/tool fields as their raw
            // JSON slices. Only the small top-level fields that Codey can
            // legitimately change are re-encoded, avoiding a full second
            // serialization of long conversations.
            let passthrough_body = match encoded_body.take() {
                Some(body)
                    if should_passthrough_native_responses(
                        bridge,
                        &model,
                        resolved.upstream_model.as_str(),
                        body_mutated,
                    ) =>
                {
                    body
                }
                Some(body) => {
                    rewrite_native_responses_encoded_body_offloaded(body, &upstream_body).await?
                }
                None => serde_json::to_vec(&upstream_body)
                    .context("序列化 Responses WebSocket 上游请求失败")?,
            };
            request_builder
                .header(CONTENT_TYPE, "application/json")
                .body(passthrough_body)
        } else {
            drop(encoded_body.take());
            request_builder.json(&upstream_body)
        };
        let response_header_timeout = if upstream_stream_requested {
            UPSTREAM_RESPONSE_HEADER_TIMEOUT
        } else {
            UPSTREAM_NON_STREAM_RESPONSE_HEADER_TIMEOUT
        };
        if let Some(probe) = downstream.request_log_probe() {
            probe.mark_upstream_send(if upstream_stream_requested {
                UpstreamTransport::HttpSse
            } else {
                UpstreamTransport::Http
            });
        }
        let initial_response = await_upstream(
            downstream,
            tokio::time::timeout(response_header_timeout, request_builder.send()),
        )
        .await?;
        let (response_result, effective_upstream_stream, effective_header_timeout) =
            match initial_response {
                Ok(Err(error))
                    if subagent_request && upstream_stream_requested && error.is_connect() =>
                {
                    if let Some(probe) = downstream.request_log_probe() {
                        probe.mark_fallback("stream_connect_to_non_stream_http");
                        probe.set_upstream_transport(UpstreamTransport::Http);
                    }
                    // A connect error happens before the request reaches the
                    // provider, so retrying once as a non-streaming HTTP
                    // request cannot duplicate model or tool side effects.
                    // Errors after the request was sent are never replayed.
                    let mut fallback_body = upstream_body.clone();
                    fallback_body
                        .as_object_mut()
                        .expect("validated Responses body must remain an object")
                        .insert("stream".to_string(), Value::Bool(false));
                    let fallback_request = self
                        .client
                        .post(upstream_url)
                        .headers(
                            fallback_headers
                                .expect("streaming subagent fallback headers must be prepared"),
                        )
                        .json(&fallback_body);
                    (
                        await_upstream(
                            downstream,
                            tokio::time::timeout(
                                UPSTREAM_NON_STREAM_RESPONSE_HEADER_TIMEOUT,
                                fallback_request.send(),
                            ),
                        )
                        .await?,
                        false,
                        UPSTREAM_NON_STREAM_RESPONSE_HEADER_TIMEOUT,
                    )
                }
                response => (response, upstream_stream_requested, response_header_timeout),
            };
        let response = match response_result {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                let timeout = error.is_timeout();
                let connect = error.is_connect();
                let sanitized_error = error.without_url().to_string();
                record_router_failure_nonblocking(
                    "local_router_upstream_failed",
                    "proxy_local_router_request",
                    sanitized_error,
                    serde_json::json!({
                        "routeId": resolved.provider_id.as_str(),
                        "routeName": resolved.route.route_name.as_str(),
                        "requestedModel": resolved.requested_model.as_str(),
                        "model": resolved.upstream_model.as_str(),
                        "timeout": timeout,
                        "connect": connect,
                        "upstream": resolved.route.upstream_authority.as_str(),
                        "upstreamProtocol": bridge.upstream_protocol().label(),
                        "protocolBridge": bridge.label(),
                        "requestKind": request_kind.label(),
                        "upstreamStream": effective_upstream_stream,
                        "responseHeaderTimeoutSeconds": effective_header_timeout.as_secs(),
                        "requestId": current_router_request_id(),
                    }),
                );
                let route_name = route_display_name(&resolved.route);
                let upstream = resolved.route.upstream_authority.as_str();
                let (status, code, message) = if timeout {
                    (
                        504,
                        "upstream_timeout",
                        format!(
                            "Codey 线路「{route_name}」请求上游 {upstream} 超时；请检查上游服务状态或网络连接"
                        ),
                    )
                } else {
                    (
                        424,
                        "upstream_unreachable",
                        format!(
                            "Codey 线路「{route_name}」无法连接上游 {upstream}；请确认上游服务已启动，并检查线路 URL、证书和网络设置"
                        ),
                    )
                };
                // Codex currently reduces JSON bodies from locally generated
                // gateway failures to "Unknown error". A concise text body is
                // preserved in its surfaced `unexpected status` message. A
                // transport setup failure uses non-retryable 424 so Codex does
                // not repeat the same deterministic failure four more times.
                downstream.write_text_error(status, code, message).await?;
                return Ok(());
            }
            Err(_) => {
                record_router_failure_nonblocking(
                    "local_router_upstream_failed",
                    "wait_for_local_router_upstream_headers",
                    "等待上游响应头超时",
                    serde_json::json!({
                        "routeId": resolved.provider_id.as_str(),
                        "routeName": resolved.route.route_name.as_str(),
                        "requestedModel": resolved.requested_model.as_str(),
                        "model": resolved.upstream_model.as_str(),
                        "timeout": true,
                        "stage": "response_headers",
                        "upstream": resolved.route.upstream_authority.as_str(),
                        "upstreamProtocol": bridge.upstream_protocol().label(),
                        "protocolBridge": bridge.label(),
                        "requestKind": request_kind.label(),
                        "upstreamStream": effective_upstream_stream,
                        "responseHeaderTimeoutSeconds": effective_header_timeout.as_secs(),
                        "requestId": current_router_request_id(),
                    }),
                );
                downstream
                    .write_text_error(
                        504,
                        "upstream_header_timeout",
                        format!(
                            "Codey 线路「{}」等待上游 {} 返回响应头超时",
                            route_display_name(&resolved.route),
                            resolved.route.upstream_authority
                        ),
                    )
                    .await?;
                return Ok(());
            }
        };
        if let Some(probe) = downstream.request_log_probe() {
            let upstream_request_id = upstream_request_id_from_headers(response.headers());
            probe.mark_upstream_headers(response.status().as_u16(), upstream_request_id.as_deref());
        }
        match bridge {
            ProtocolBridge::ResponsesToAnthropicMessages => {
                write_anthropic_messages_as_responses(
                    downstream,
                    response,
                    &resolved.upstream_model,
                    stream_requested,
                    &resolved.route,
                    &tool_bridge,
                )
                .await
            }
            _ if !response.status().is_success() => {
                write_upstream_http_error(downstream, response, &resolved, bridge, request_kind)
                    .await
            }
            ProtocolBridge::ResponsesToChatCompletions => {
                write_chat_completions_as_responses(
                    downstream,
                    response,
                    &resolved.upstream_model,
                    stream_requested,
                    &resolved.route,
                    &tool_bridge,
                )
                .await
            }
            _ => downstream.proxy_response(response).await,
        }
    }
}

fn should_force_upstream_streaming(
    bridge: ProtocolBridge,
    request_kind: ResponsesRequestKind,
    downstream_websocket: bool,
    stream_requested: bool,
) -> bool {
    request_kind == ResponsesRequestKind::Create
        && !downstream_websocket
        && !stream_requested
        && bridge.can_collect_streamed_response()
}

fn request_binding_keys(request: &HttpRequest) -> Vec<String> {
    ["thread-id", "session-id"]
        .into_iter()
        .filter_map(|header_name| {
            let value = incoming_header(request, header_name)?.trim();
            (!value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control))
                .then(|| format!("{header_name}:{value}"))
        })
        .collect()
}

fn request_log_codex_session(request: &HttpRequest) -> (Option<&str>, bool) {
    if let Some(parent_thread_id) = incoming_header(request, "x-codex-parent-thread-id") {
        return (valid_codex_session_id(parent_thread_id), true);
    }
    if incoming_header(request, "x-openai-subagent").is_some() {
        return (None, true);
    }
    (
        ["thread-id", "session-id"]
            .into_iter()
            .find_map(|header_name| incoming_header(request, header_name))
            .and_then(valid_codex_session_id),
        false,
    )
}

fn valid_codex_session_id(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control))
        .then_some(value)
}

fn take_codey_route_metadata(
    request: &mut HttpRequest,
    body: &mut Value,
) -> Result<(Option<String>, bool)> {
    let mut route_hint = None;
    let mut body_mutated = false;
    for (name, value) in &mut request.headers {
        if !name.eq_ignore_ascii_case(TURN_METADATA_HEADER) {
            continue;
        }
        let Ok(mut metadata) = serde_json::from_str::<Value>(value) else {
            continue;
        };
        let extracted = take_route_hint_from_metadata_value(&mut metadata)?;
        merge_route_hint(&mut route_hint, extracted)?;
        *value =
            serde_json::to_string(&metadata).context("序列化清理后的 Codex turn metadata 失败")?;
    }

    let Some(client_metadata) = body
        .as_object_mut()
        .and_then(|body| body.get_mut("client_metadata"))
        .and_then(Value::as_object_mut)
    else {
        return Ok((route_hint, false));
    };
    let direct = client_metadata.remove(ROUTE_METADATA_KEY);
    if direct.is_some() {
        body_mutated = true;
    }
    merge_route_hint(
        &mut route_hint,
        direct.map(validated_route_hint_value).transpose()?,
    )?;
    if let Some(metadata) = client_metadata.get_mut(TURN_METADATA_HEADER) {
        let extracted = match metadata {
            Value::String(serialized) => {
                let Ok(mut parsed) = serde_json::from_str::<Value>(serialized) else {
                    return Ok((route_hint, body_mutated));
                };
                let extracted = take_route_hint_from_metadata_value(&mut parsed)?;
                if extracted.is_some() {
                    *serialized = serde_json::to_string(&parsed)
                        .context("序列化清理后的 Responses client metadata 失败")?;
                    body_mutated = true;
                }
                extracted
            }
            Value::Object(_) => {
                let extracted = take_route_hint_from_metadata_value(metadata)?;
                if extracted.is_some() {
                    body_mutated = true;
                }
                extracted
            }
            _ => None,
        };
        merge_route_hint(&mut route_hint, extracted)?;
    }
    Ok((route_hint, body_mutated))
}

fn should_passthrough_native_responses(
    bridge: ProtocolBridge,
    requested_model: &str,
    upstream_model: &str,
    body_mutated: bool,
) -> bool {
    matches!(bridge, ProtocolBridge::NativeResponses)
        && requested_model == upstream_model
        && !body_mutated
}

fn remove_codey_synthetic_previous_response_id(body: &mut Value) -> bool {
    let Some(object) = body.as_object_mut() else {
        return false;
    };
    let Some(previous_response_id) = object
        .get("previous_response_id")
        .and_then(Value::as_str)
        .map(str::trim)
    else {
        return false;
    };
    if !is_codey_synthetic_response_id(previous_response_id) {
        return false;
    }
    remove_previous_response_id(body)
}

fn remove_previous_response_id(body: &mut Value) -> bool {
    body.as_object_mut()
        .and_then(|object| object.remove("previous_response_id"))
        .is_some()
}

fn is_codey_synthetic_response_id(response_id: &str) -> bool {
    response_id.trim().starts_with("resp_codey_")
}

struct RawTopLevelObject<'a>(Vec<(String, &'a RawValue)>);

struct RawTopLevelObjectVisitor;

impl<'de> Visitor<'de> for RawTopLevelObjectVisitor {
    type Value = RawTopLevelObject<'de>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut fields = Vec::with_capacity(map.size_hint().unwrap_or_default());
        while let Some(field) = map.next_entry::<String, &'de RawValue>()? {
            fields.push(field);
        }
        Ok(RawTopLevelObject(fields))
    }
}

impl<'de> Deserialize<'de> for RawTopLevelObject<'de> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(RawTopLevelObjectVisitor)
    }
}

fn begin_encoded_json_field(output: &mut Vec<u8>, first: &mut bool, name: &str) -> Result<()> {
    if !*first {
        output.push(b',');
    }
    *first = false;
    serde_json::to_writer(&mut *output, name).context("序列化 Responses 请求字段名失败")?;
    output.push(b':');
    Ok(())
}

fn rewrite_native_responses_encoded_body(original: &[u8], updated: &Value) -> Result<Vec<u8>> {
    let RawTopLevelObject(fields) = serde_json::from_slice::<RawTopLevelObject>(original)
        .context("解析 Responses 原始请求字段失败")?;
    let updated = updated
        .as_object()
        .context("Responses 上游请求必须是 JSON 对象")?;
    let mut output = Vec::with_capacity(original.len());
    output.push(b'{');
    let mut first = true;
    let mut saw_model = false;
    let mut saw_client_metadata = false;
    let mut saw_previous_response_id = false;
    for (name, raw) in fields {
        let replacement = match name.as_str() {
            "model" => {
                saw_model = true;
                Some(updated.get("model"))
            }
            "client_metadata" => {
                saw_client_metadata = true;
                Some(updated.get("client_metadata"))
            }
            "previous_response_id" => {
                saw_previous_response_id = true;
                Some(updated.get("previous_response_id"))
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            let Some(replacement) = replacement else {
                continue;
            };
            begin_encoded_json_field(&mut output, &mut first, &name)?;
            serde_json::to_writer(&mut output, replacement)
                .context("序列化 Responses 已更新请求字段失败")?;
        } else {
            begin_encoded_json_field(&mut output, &mut first, &name)?;
            output.extend_from_slice(raw.get().as_bytes());
        }
    }
    for (name, saw_field) in [
        ("model", saw_model),
        ("client_metadata", saw_client_metadata),
        ("previous_response_id", saw_previous_response_id),
    ] {
        if saw_field {
            continue;
        }
        let Some(value) = updated.get(name) else {
            continue;
        };
        begin_encoded_json_field(&mut output, &mut first, name)?;
        serde_json::to_writer(&mut output, value).context("序列化 Responses 新增请求字段失败")?;
    }
    output.push(b'}');
    Ok(output)
}

async fn rewrite_native_responses_encoded_body_offloaded(
    original: Vec<u8>,
    updated: &Value,
) -> Result<Vec<u8>> {
    if original.len() < REQUEST_JSON_OFFLOAD_BYTES {
        return rewrite_native_responses_encoded_body(&original, updated);
    }
    let updated = updated
        .as_object()
        .context("Responses 上游请求必须是 JSON 对象")?
        .iter()
        .filter(|(name, _)| {
            matches!(
                name.as_str(),
                "model" | "client_metadata" | "previous_response_id"
            )
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<serde_json::Map<_, _>>();
    tokio::task::spawn_blocking(move || {
        rewrite_native_responses_encoded_body(&original, &Value::Object(updated))
    })
    .await
    .context("等待大型 Responses 请求改写任务失败")?
}

fn take_route_hint_from_metadata_value(metadata: &mut Value) -> Result<Option<String>> {
    let Some(metadata) = metadata.as_object_mut() else {
        return Ok(None);
    };
    metadata
        .remove(ROUTE_METADATA_KEY)
        .map(validated_route_hint_value)
        .transpose()
}

fn validated_route_hint_value(value: Value) -> Result<String> {
    let route_hint = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("{ROUTE_METADATA_KEY} 必须是字符串"))?
        .trim();
    if route_hint.is_empty() || route_hint.len() > 256 || route_hint.chars().any(char::is_control) {
        anyhow::bail!("{ROUTE_METADATA_KEY} 不是有效的线路 ID");
    }
    Ok(route_hint.to_string())
}

fn merge_route_hint(current: &mut Option<String>, next: Option<String>) -> Result<()> {
    let Some(next) = next else {
        return Ok(());
    };
    if current.as_ref().is_some_and(|current| current != &next) {
        anyhow::bail!("请求头和请求体携带了冲突的 {ROUTE_METADATA_KEY}");
    }
    *current = Some(next);
    Ok(())
}

fn should_forward_incoming_header(name: &str, official_account: bool) -> bool {
    if name.eq_ignore_ascii_case("authorization")
        || name.eq_ignore_ascii_case(ROUTER_AUTH_HEADER)
        || name.eq_ignore_ascii_case(ROUTE_METADATA_KEY)
        || name.eq_ignore_ascii_case(CONTENT_ENCODING.as_str())
        || name.eq_ignore_ascii_case(CONTENT_TYPE.as_str())
        || is_hop_by_hop_header(name)
    {
        return false;
    }
    // ChatGPT-account headers are required by the official Codex endpoint but
    // must never cross into an API-key provider. Third-party routes receive
    // only content negotiation, Codex client identity, and saved route headers.
    official_account || name.eq_ignore_ascii_case("accept") || is_codex_client_identity_header(name)
}

fn ensure_native_prompt_cache_key(
    headers: &mut HeaderMap,
    body: &Value,
    route_id: &str,
    upstream_url: &str,
    upstream_model: &str,
) -> bool {
    if headers.contains_key(PROMPT_CACHE_KEY_HEADER)
        || headers.contains_key(PROMPT_CACHE_KEY_COMPAT_HEADER)
        || body
            .as_object()
            .is_some_and(|body| body.contains_key(PROMPT_CACHE_KEY_BODY_FIELD))
    {
        return false;
    }
    let key = stable_prompt_cache_key(route_id, upstream_url, upstream_model, headers);
    headers.insert(
        HeaderName::from_static(PROMPT_CACHE_KEY_HEADER),
        HeaderValue::from_str(&key)
            .expect("generated prompt cache key must be a valid header value"),
    );
    true
}

fn stable_prompt_cache_key(
    route_id: &str,
    upstream_url: &str,
    upstream_model: &str,
    headers: &HeaderMap,
) -> String {
    let mut hasher = Sha256::new();
    for component in [
        b"codey-prompt-cache-v1".as_slice(),
        route_id.as_bytes(),
        upstream_url.as_bytes(),
        upstream_model.as_bytes(),
    ] {
        update_length_prefixed_digest(&mut hasher, component);
    }
    let (identity_kind, identity) = headers
        .get(CHATGPT_ACCOUNT_ID_HEADER)
        .map(|value| (b"account".as_slice(), value.as_bytes()))
        .or_else(|| {
            headers
                .get(AUTHORIZATION)
                .map(|value| (b"authorization".as_slice(), value.as_bytes()))
        })
        .unwrap_or((b"anonymous".as_slice(), b"".as_slice()));
    update_length_prefixed_digest(&mut hasher, identity_kind);
    update_length_prefixed_digest(&mut hasher, &Sha256::digest(identity));
    let digest = format!("{:x}", hasher.finalize());
    format!("codey-{}", &digest[..48])
}

fn update_length_prefixed_digest(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn is_codex_client_identity_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "user-agent"
            | "originator"
            | "version"
            | "openai-beta"
            | "x-openai-originator"
            | "x-openai-client-user-agent"
            | "x-client-request-id"
            | "thread-id"
            | "thread_id"
            | "session-id"
            | "session_id"
            | "prompt-cache-key"
            | "prompt_cache_key"
            | "x-codex-installation-id"
            | "x-codex-window-id"
            | "x-codex-parent-thread-id"
            | "x-codex-beta-features"
            | "x-openai-subagent"
    ) || lower.starts_with("x-stainless-")
}

fn incoming_header<'a>(request: &'a HttpRequest, header_name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(header_name))
        .map(|(_, value)| value.as_str())
}

fn request_is_subagent(request: &HttpRequest) -> bool {
    incoming_header(request, "x-openai-subagent").is_some()
        || incoming_header(request, "x-codex-parent-thread-id").is_some()
}

fn incoming_openai_authorization<'a>(
    request: &'a HttpRequest,
    router_bearer_token: &str,
) -> Option<&'a str> {
    let authorization = incoming_header(request, "authorization")?;
    if constant_time_eq(
        authorization.trim().as_bytes(),
        router_bearer_token.as_bytes(),
    ) {
        return None;
    }
    Some(authorization)
}

struct OfficialUpstreamAuth {
    authorization: String,
    account_id: Option<String>,
}

fn incoming_chatgpt_account_id(request: &HttpRequest) -> Option<String> {
    incoming_header(request, CHATGPT_ACCOUNT_ID_HEADER)
        .map(str::trim)
        .filter(|account_id| !account_id.is_empty())
        .map(ToString::to_string)
}

async fn resolve_official_upstream_auth(
    request: &HttpRequest,
    router_bearer_token: &str,
    auth_path: &Path,
    auth_cache: &Mutex<crate::account_usage::OfficialAuthCache>,
) -> Option<OfficialUpstreamAuth> {
    if let Some(authorization) = incoming_openai_authorization(request, router_bearer_token) {
        let account_id = match incoming_chatgpt_account_id(request) {
            Some(account_id) => Some(account_id),
            None => read_cached_official_auth(auth_path, auth_cache)
                .await
                .and_then(|auth| auth.account_id),
        };
        return Some(OfficialUpstreamAuth {
            authorization: authorization.to_string(),
            account_id,
        });
    }

    let auth = read_cached_official_auth(auth_path, auth_cache).await?;
    Some(OfficialUpstreamAuth {
        authorization: format!("Bearer {}", auth.access_token),
        // The account ID stored with the selected OAuth token is authoritative.
        // An incoming value may have been captured by a long-lived downstream
        // WebSocket before the user switched accounts.
        account_id: auth
            .account_id
            .or_else(|| incoming_chatgpt_account_id(request)),
    })
}

async fn read_cached_official_auth(
    auth_path: &Path,
    auth_cache: &Mutex<crate::account_usage::OfficialAuthCache>,
) -> Option<crate::account_usage::OfficialAuth> {
    let now = Instant::now();
    if let Some(cached) = auth_cache
        .lock()
        .expect("official auth cache mutex poisoned")
        .get(now)
    {
        return cached.ok();
    }

    let auth_path = auth_path.to_path_buf();
    let result =
        tokio::task::spawn_blocking(move || crate::account_usage::read_official_auth(&auth_path))
            .await
            .ok()?;
    let now = Instant::now();
    let mut cache = auth_cache
        .lock()
        .expect("official auth cache mutex poisoned");
    if let Some(cached) = cache.get(now) {
        return cached.ok();
    }
    cache.store(result, now).ok()
}

fn route_display_name(route: &RouteTarget) -> &str {
    let route_name = route.route_name.trim();
    if route_name.is_empty() {
        route.provider_id.as_str()
    } else {
        route_name
    }
}

fn upstream_authority(base_url: &str) -> String {
    let Ok(url) = reqwest::Url::parse(base_url.trim()) else {
        return "已配置地址".to_string();
    };
    let Some(host) = url.host_str() else {
        return "已配置地址".to_string();
    };
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpstreamProtocol {
    OpenAiResponses,
    OpenAiChatCompletions,
    AnthropicMessages,
}

impl UpstreamProtocol {
    fn from_profile(official_account: bool, upstream_protocol: &str) -> Self {
        if official_account {
            return Self::OpenAiResponses;
        }
        match upstream_protocol {
            UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS => Self::OpenAiChatCompletions,
            UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES => Self::AnthropicMessages,
            UPSTREAM_PROTOCOL_OPENAI_RESPONSES => Self::OpenAiResponses,
            _ => Self::OpenAiResponses,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "OpenAI Responses",
            Self::OpenAiChatCompletions => "OpenAI Chat Completions",
            Self::AnthropicMessages => "Anthropic Messages",
        }
    }

    fn endpoint_label(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "API URL",
            Self::OpenAiChatCompletions => "Chat Completions API URL",
            Self::AnthropicMessages => "Anthropic Messages API URL",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProtocolBridge {
    NativeResponses,
    ResponsesToChatCompletions,
    ResponsesToAnthropicMessages,
}

impl ProtocolBridge {
    fn from_upstream_protocol(protocol: UpstreamProtocol) -> Self {
        match protocol {
            UpstreamProtocol::OpenAiResponses => Self::NativeResponses,
            UpstreamProtocol::OpenAiChatCompletions => Self::ResponsesToChatCompletions,
            UpstreamProtocol::AnthropicMessages => Self::ResponsesToAnthropicMessages,
        }
    }

    fn upstream_protocol(self) -> UpstreamProtocol {
        match self {
            Self::NativeResponses => UpstreamProtocol::OpenAiResponses,
            Self::ResponsesToChatCompletions => UpstreamProtocol::OpenAiChatCompletions,
            Self::ResponsesToAnthropicMessages => UpstreamProtocol::AnthropicMessages,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::NativeResponses => "Responses passthrough",
            Self::ResponsesToChatCompletions => "Responses -> Chat Completions",
            Self::ResponsesToAnthropicMessages => "Responses -> Anthropic Messages",
        }
    }

    fn convert_responses_body(self, body: &Value) -> Result<Option<ConvertedResponsesRequest>> {
        match self {
            Self::NativeResponses => Ok(None),
            Self::ResponsesToChatCompletions => {
                responses_to_chat_completions_request(body).map(Some)
            }
            Self::ResponsesToAnthropicMessages => {
                responses_to_anthropic_messages_request(body).map(Some)
            }
        }
    }

    fn can_collect_streamed_response(self) -> bool {
        matches!(
            self,
            Self::ResponsesToChatCompletions | Self::ResponsesToAnthropicMessages
        )
    }
}

fn is_hop_by_hop_header(name: &str) -> bool {
    [
        "host",
        "content-length",
        "connection",
        "proxy-connection",
        "keep-alive",
        "transfer-encoding",
        "te",
        "trailer",
        "upgrade",
        "accept-encoding",
    ]
    .iter()
    .any(|blocked| name.eq_ignore_ascii_case(blocked))
}

fn is_sse_content_type(value: &str) -> bool {
    const SSE_CONTENT_TYPE: &[u8] = b"text/event-stream";
    value
        .as_bytes()
        .windows(SSE_CONTENT_TYPE.len())
        .any(|candidate| candidate.eq_ignore_ascii_case(SSE_CONTENT_TYPE))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (*left ^ *right)
        })
        == 0
}

fn current_router_request_id() -> Option<String> {
    ROUTER_REQUEST_ID
        .try_with(|request_id| request_id.clone())
        .ok()
}

fn current_router_request_started_at() -> Option<Instant> {
    ROUTER_REQUEST_STARTED_AT
        .try_with(|started_at| *started_at)
        .ok()
}

fn router_request_id_header() -> String {
    current_router_request_id()
        .map(|request_id| format!("x-codey-request-id: {request_id}\r\n"))
        .unwrap_or_default()
}

fn normalized_endpoint_url(base_url: &str) -> Result<reqwest::Url> {
    let mut url = crate::config::validate_outbound_api_url(base_url.trim(), "线路 API URL")
        .map_err(anyhow::Error::msg)?;
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn strip_ascii_case_suffix<'a>(value: &'a str, suffix: &str) -> Option<&'a str> {
    let prefix_len = value.len().checked_sub(suffix.len())?;
    value[prefix_len..]
        .eq_ignore_ascii_case(suffix)
        .then_some(&value[..prefix_len])
}

fn responses_endpoint(base_url: &str) -> Result<String> {
    let url = normalized_endpoint_url(base_url)?;
    let base = url.as_str().trim_end_matches('/');
    if strip_ascii_case_suffix(base, "/responses").is_some() {
        Ok(base.to_string())
    } else {
        Ok(format!("{base}/responses"))
    }
}

fn image_generation_endpoint(base_url: &str) -> Result<String> {
    let url = normalized_endpoint_url(base_url)?;
    let base = url.as_str().trim_end_matches('/');
    if strip_ascii_case_suffix(base, "/images/generations").is_some() {
        return Ok(base.to_string());
    }
    for suffix in ["/chat/completions", "/responses"] {
        if let Some(prefix) = strip_ascii_case_suffix(base, suffix) {
            return Ok(format!(
                "{}/images/generations",
                prefix.trim_end_matches('/')
            ));
        }
    }
    let last_segment = url
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    if has_version_suffix(last_segment) {
        Ok(format!("{base}/images/generations"))
    } else {
        Ok(format!("{base}/v1/images/generations"))
    }
}

fn responses_compact_endpoint(base_url: &str) -> Result<String> {
    let endpoint = responses_endpoint(base_url)?;
    Ok(format!("{}/compact", endpoint.trim_end_matches('/')))
}

fn responses_websocket_endpoint(base_url: &str) -> Result<String> {
    let endpoint = responses_endpoint(base_url)?;
    let mut url = reqwest::Url::parse(&endpoint).context("解析 Responses WebSocket URL 失败")?;
    let websocket_scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        scheme => anyhow::bail!("Responses WebSocket 不支持 URL scheme {scheme}"),
    };
    url.set_scheme(websocket_scheme)
        .map_err(|_| anyhow::anyhow!("转换 Responses WebSocket URL scheme 失败"))?;
    Ok(url.to_string())
}

fn chat_completions_endpoint(base_url: &str) -> Result<String> {
    let url = normalized_endpoint_url(base_url)?;
    let base = url.as_str().trim_end_matches('/');
    if strip_ascii_case_suffix(base, "/chat/completions").is_some() {
        return Ok(base.to_string());
    }
    if let Some(prefix) = strip_ascii_case_suffix(base, "/responses") {
        return Ok(format!("{}/chat/completions", prefix.trim_end_matches('/')));
    }
    let last_segment = url
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    if has_version_suffix(last_segment) {
        Ok(format!("{base}/chat/completions"))
    } else {
        Ok(format!("{base}/v1/chat/completions"))
    }
}

fn anthropic_messages_endpoint(base_url: &str) -> Result<String> {
    let url = normalized_endpoint_url(base_url)?;
    let base = url.as_str().trim_end_matches('/');
    if strip_ascii_case_suffix(base, "/messages").is_some() {
        return Ok(base.to_string());
    }
    for suffix in ["/chat/completions", "/responses"] {
        if let Some(prefix) = strip_ascii_case_suffix(base, suffix) {
            return Ok(format!("{}/messages", prefix.trim_end_matches('/')));
        }
    }
    let last_segment = url
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    if has_version_suffix(last_segment) {
        Ok(format!("{base}/messages"))
    } else {
        Ok(format!("{base}/v1/messages"))
    }
}

fn prepare_upstream_url(
    protocol: UpstreamProtocol,
    base_url: &str,
) -> std::result::Result<String, String> {
    let result = match protocol {
        UpstreamProtocol::OpenAiResponses => responses_endpoint(base_url),
        UpstreamProtocol::OpenAiChatCompletions => chat_completions_endpoint(base_url),
        UpstreamProtocol::AnthropicMessages => anthropic_messages_endpoint(base_url),
    };
    result.map_err(|error| {
        let endpoint = protocol.endpoint_label();
        format!("{endpoint} 无效：{error:#}")
    })
}

fn prepare_upstream_compact_url(
    protocol: UpstreamProtocol,
    base_url: &str,
) -> std::result::Result<String, String> {
    if protocol == UpstreamProtocol::OpenAiResponses {
        responses_compact_endpoint(base_url)
            .map_err(|error| format!("Responses Compact API URL 无效：{error:#}"))
    } else {
        // CC Switch sends legacy compact requests through the same conversion
        // and upstream endpoint as a normal Responses request for adapted
        // Chat Completions and Anthropic routes.
        prepare_upstream_url(protocol, base_url)
    }
}

fn prepare_upstream_websocket_url(
    protocol: UpstreamProtocol,
    base_url: &str,
) -> std::result::Result<String, String> {
    if protocol != UpstreamProtocol::OpenAiResponses {
        return Err(format!("{} 不支持 Responses WebSocket", protocol.label()));
    }
    responses_websocket_endpoint(base_url)
        .map_err(|error| format!("Responses WebSocket API URL 无效：{error:#}"))
}

fn prepare_upstream_headers(
    profile: &crate::config::ProviderProfile,
    protocol: UpstreamProtocol,
) -> std::result::Result<HeaderMap, String> {
    let route_name = profile.name.trim();
    let mut headers = HeaderMap::with_capacity(profile.model_request_headers.len() + 2);
    for (name, value) in &profile.model_request_headers {
        if value.trim().is_empty() {
            continue;
        }
        if is_hop_by_hop_header(name) {
            return Err(format!("线路「{route_name}」包含不允许覆盖的请求头 {name}"));
        }
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("线路「{route_name}」包含非法请求头名称"))?;
        let value = HeaderValue::from_str(value)
            .map_err(|_| format!("线路「{route_name}」包含非法请求头值"))?;
        headers.insert(name, value);
    }

    let has_custom_authorization = headers.contains_key(AUTHORIZATION);
    if protocol == UpstreamProtocol::AnthropicMessages && has_custom_authorization {
        return Err(format!(
            "线路「{route_name}」使用 Anthropic Messages 时不允许配置 Authorization；请使用该线路的 Key 字段或 x-api-key"
        ));
    }
    if !profile.official_account && !profile.api_key.trim().is_empty() {
        let header_name = if protocol == UpstreamProtocol::AnthropicMessages {
            HeaderName::from_static("x-api-key")
        } else {
            AUTHORIZATION
        };
        if !headers.contains_key(&header_name) {
            let header_value = if protocol == UpstreamProtocol::AnthropicMessages {
                profile.api_key.trim().to_string()
            } else {
                format!("Bearer {}", profile.api_key.trim())
            };
            let value = HeaderValue::from_str(&header_value)
                .map_err(|_| format!("线路「{route_name}」的 API Key 格式无效"))?;
            headers.insert(header_name, value);
        }
    }
    if protocol == UpstreamProtocol::AnthropicMessages
        && !headers.contains_key(HeaderName::from_static("anthropic-version"))
    {
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );
    }
    Ok(headers)
}

fn has_version_suffix(segment: &str) -> bool {
    segment
        .strip_prefix('v')
        .or_else(|| segment.strip_prefix('V'))
        .is_some_and(|version| version.chars().next().is_some_and(|ch| ch.is_ascii_digit()))
}

#[derive(Clone, Debug)]
struct ConvertedResponsesRequest {
    body: Value,
    tool_bridge: ResponsesToolBridge,
}

#[derive(Clone, Debug, Default)]
struct ResponsesToolBridge {
    upstream_to_response: HashMap<String, ResponsesToolName>,
    response_to_upstream: HashMap<ResponsesToolName, String>,
    has_namespace_tools: bool,
    has_custom_tools: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ResponsesToolKind {
    Function,
    Custom,
    ToolSearch,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ResponsesToolName {
    kind: ResponsesToolKind,
    namespace: Vec<String>,
    name: String,
}

impl ResponsesToolName {
    fn plain(name: &str) -> Self {
        Self {
            kind: ResponsesToolKind::Function,
            namespace: Vec::new(),
            name: name.to_string(),
        }
    }

    fn custom_in_namespace(namespace: &[String], name: &str) -> Self {
        Self {
            kind: ResponsesToolKind::Custom,
            namespace: namespace.to_vec(),
            name: name.to_string(),
        }
    }

    fn tool_search() -> Self {
        Self {
            kind: ResponsesToolKind::ToolSearch,
            namespace: Vec::new(),
            name: "tool_search".to_string(),
        }
    }

    fn is_custom(&self) -> bool {
        self.kind == ResponsesToolKind::Custom
    }

    fn is_function(&self) -> bool {
        self.kind == ResponsesToolKind::Function
    }

    fn is_tool_search(&self) -> bool {
        self.kind == ResponsesToolKind::ToolSearch
    }

    fn namespace_string(&self) -> Option<String> {
        (!self.namespace.is_empty()).then(|| self.namespace.join("."))
    }

    fn insert_response_fields(&self, object: &mut serde_json::Map<String, Value>) {
        if self.is_tool_search() {
            object.remove("name");
            object.remove("namespace");
            object.insert("execution".to_string(), Value::String("client".to_string()));
            return;
        }
        object.insert("name".to_string(), Value::String(self.name.clone()));
        if let Some(namespace) = self.namespace_string() {
            object.insert("namespace".to_string(), Value::String(namespace));
        } else {
            object.remove("namespace");
        }
    }
}

impl ResponsesToolBridge {
    fn upstream_name_for_call(&self, tool_name: &ResponsesToolName) -> Result<String> {
        if let Some(upstream_name) = self.response_to_upstream.get(tool_name) {
            return Ok(upstream_name.clone());
        }
        if tool_name.is_function() && tool_name.namespace.is_empty() {
            return Ok(tool_name.name.clone());
        }
        if tool_name.is_custom() {
            anyhow::bail!(
                "custom_tool_call 指向未声明的 custom 工具 {}",
                tool_name.name
            )
        }
        if tool_name.is_tool_search() {
            anyhow::bail!("tool_search_call 指向未声明的 execution=client tool_search 工具")
        }
        anyhow::bail!(
            "function_call 指向未声明的 namespace 工具 {}.{}",
            tool_name.namespace.join("."),
            tool_name.name
        )
    }

    fn restore_upstream_name(&self, upstream_name: &str) -> Result<ResponsesToolName> {
        if let Some(tool_name) = self.upstream_to_response.get(upstream_name) {
            return Ok(tool_name.clone());
        }
        if self.has_namespace_tools && looks_like_namespace_upstream_name(upstream_name) {
            anyhow::bail!("上游返回了未知的 namespace function 名称 {upstream_name}");
        }
        if self.has_custom_tools && looks_like_custom_upstream_name(upstream_name) {
            anyhow::bail!("上游返回了未知的 custom function 名称 {upstream_name}");
        }
        Ok(ResponsesToolName::plain(upstream_name))
    }

    fn restore_stream_upstream_name(
        &self,
        upstream_name: &str,
        final_name: bool,
    ) -> Result<Option<ResponsesToolName>> {
        if let Some(tool_name) = self.upstream_to_response.get(upstream_name) {
            if !final_name
                && self.upstream_to_response.keys().any(|known| {
                    known.len() > upstream_name.len() && known.starts_with(upstream_name)
                })
            {
                return Ok(None);
            }
            return Ok(Some(tool_name.clone()));
        }
        if !final_name
            && self
                .upstream_to_response
                .keys()
                .any(|known| known.starts_with(upstream_name))
        {
            return Ok(None);
        }
        if self.has_namespace_tools && could_be_namespace_upstream_name(upstream_name) {
            if final_name {
                anyhow::bail!("上游返回了未知的 namespace function 名称 {upstream_name}");
            }
            return Ok(None);
        }
        if self.has_custom_tools && could_be_custom_upstream_name(upstream_name) {
            if final_name {
                anyhow::bail!("上游返回了未知的 custom function 名称 {upstream_name}");
            }
            return Ok(None);
        }
        Ok(Some(ResponsesToolName::plain(upstream_name)))
    }
}

#[cfg(test)]
fn responses_to_chat_completions_body(body: &Value) -> Result<Value> {
    Ok(responses_to_chat_completions_request(body)?.body)
}

fn responses_to_chat_completions_request(body: &Value) -> Result<ConvertedResponsesRequest> {
    let object = body
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Responses 请求体必须是 JSON 对象"))?;
    let model = object
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| anyhow::anyhow!("缺少 model 字段"))?;
    let mut messages = Vec::new();
    if let Some(instructions) = object.get("instructions").and_then(Value::as_str)
        && !instructions.is_empty()
    {
        messages.push(json!({"role":"system","content":instructions}));
    }
    // Responses can add tools at a specific point in `input`. Chat and
    // Anthropic only accept request-level tool declarations, so retain the
    // message order while promoting those declarations for the current model
    // generation. Historical assistant items are not re-executed.
    let (normalized_input, additional_tools) =
        responses_input_without_additional_tools(object.get("input"))?;
    let merged_tools = merge_responses_tools(object.get("tools"), additional_tools)?;
    let mut tool_bridge = ResponsesToolBridge::default();
    let chat_tools = merged_tools
        .as_ref()
        .map(|tools| responses_tools_to_chat_tools_with_bridge(tools, &mut tool_bridge))
        .transpose()?;
    append_chat_messages_from_responses_input(
        normalized_input.as_ref(),
        &mut messages,
        &tool_bridge,
    )?;
    if messages.is_empty() {
        anyhow::bail!("缺少可转换为 Chat Completions messages 的 input");
    }
    if object
        .get("previous_response_id")
        .is_some_and(|value| !value.is_null())
    {
        anyhow::bail!(
            "previous_response_id 依赖 Responses 服务端状态，不能无损转换为 Chat Completions"
        );
    }
    let stream = object
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut chat = serde_json::Map::from_iter([
        ("model".to_string(), Value::String(model.to_string())),
        ("messages".to_string(), Value::Array(messages)),
        ("stream".to_string(), Value::Bool(stream)),
    ]);
    copy_number_or_string_field(object, &mut chat, "temperature", "temperature");
    copy_number_or_string_field(object, &mut chat, "top_p", "top_p");
    copy_number_or_string_field(object, &mut chat, "presence_penalty", "presence_penalty");
    copy_number_or_string_field(object, &mut chat, "frequency_penalty", "frequency_penalty");
    copy_number_or_string_field(object, &mut chat, "reasoning_effort", "reasoning_effort");
    if !chat.contains_key("reasoning_effort")
        && let Some(reasoning) = object.get("reasoning").and_then(Value::as_object)
    {
        copy_number_or_string_field(reasoning, &mut chat, "effort", "reasoning_effort");
    }
    copy_number_or_string_field(object, &mut chat, "user", "user");
    copy_json_field(object, &mut chat, "stop", "stop");
    copy_json_field(object, &mut chat, "seed", "seed");
    copy_json_field(object, &mut chat, "logit_bias", "logit_bias");
    copy_json_field(object, &mut chat, "logprobs", "logprobs");
    copy_json_field(object, &mut chat, "top_logprobs", "top_logprobs");
    if stream {
        let mut stream_options = object
            .get("stream_options")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        // Chat Completions only reports usage for streams when explicitly
        // requested. The final Responses event needs that usage shape.
        stream_options.insert("include_usage".to_string(), Value::Bool(true));
        chat.insert("stream_options".to_string(), Value::Object(stream_options));
    }
    if let Some(text) = object.get("text").and_then(Value::as_object)
        && let Some(format) = text.get("format")
    {
        chat.insert(
            "response_format".to_string(),
            responses_text_format_to_chat_response_format(format)?,
        );
    }
    if let Some(max_tokens) = object
        .get("max_output_tokens")
        .or_else(|| object.get("max_tokens"))
        .cloned()
    {
        chat.insert("max_tokens".to_string(), max_tokens);
    }
    let web_search_tool_seen = chat_tools
        .as_ref()
        .is_some_and(|tools| tools.web_search_tool_seen);
    let has_chat_tools = chat_tools
        .as_ref()
        .and_then(|tools| tools.tools.as_ref())
        .is_some();
    if let Some(web_search_options) = chat_web_search_options_for_tool_choice(
        chat_tools
            .as_ref()
            .and_then(|tools| tools.web_search_options.as_ref()),
        object.get("tool_choice"),
        has_chat_tools,
    )? {
        chat.insert("web_search_options".to_string(), web_search_options);
    }
    let web_search_enabled = chat.contains_key("web_search_options");
    if web_search_enabled {
        reject_unportable_chat_web_search_include(object.get("include"))?;
    }
    if let Some(tools) = chat_tools.and_then(|tools| tools.tools) {
        chat.insert("tools".to_string(), tools);
    }
    if let Some(tool_choice) = object.get("tool_choice")
        && !should_omit_chat_tool_choice_for_web_search(
            tool_choice,
            has_chat_tools,
            web_search_tool_seen,
            web_search_enabled,
        )?
    {
        chat.insert(
            "tool_choice".to_string(),
            responses_tool_choice_to_chat_tool_choice(tool_choice, &tool_bridge)?,
        );
    }
    if let Some(parallel_tool_calls) = object.get("parallel_tool_calls") {
        if !parallel_tool_calls.is_boolean() {
            anyhow::bail!("parallel_tool_calls 必须是布尔值");
        }
        chat.insert(
            "parallel_tool_calls".to_string(),
            parallel_tool_calls.clone(),
        );
    }
    if let Some(function_call) = object.get("function_call") {
        chat.insert(
            "function_call".to_string(),
            responses_function_call_choice_to_chat(function_call, &tool_bridge)?,
        );
    }
    Ok(ConvertedResponsesRequest {
        body: Value::Object(chat),
        tool_bridge,
    })
}

fn responses_input_without_additional_tools(
    input: Option<&Value>,
) -> Result<(Option<Value>, Vec<Value>)> {
    let Some(input) = input else {
        return Ok((None, Vec::new()));
    };
    let mut additional_tools = Vec::new();
    match input {
        Value::Array(items) => {
            let mut normalized = Vec::with_capacity(items.len());
            for item in items {
                if append_responses_additional_tools(item, &mut additional_tools)? {
                    continue;
                }
                append_responses_tool_search_output_tools(item, &mut additional_tools)?;
                if item
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(is_opaque_responses_input_item_type)
                {
                    continue;
                }
                normalized.push(item.clone());
            }
            Ok((Some(Value::Array(normalized)), additional_tools))
        }
        Value::Object(_) if append_responses_additional_tools(input, &mut additional_tools)? => {
            Ok((None, additional_tools))
        }
        Value::Object(_)
            if input
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(is_opaque_responses_input_item_type) =>
        {
            Ok((None, additional_tools))
        }
        Value::Object(_) => {
            append_responses_tool_search_output_tools(input, &mut additional_tools)?;
            Ok((Some(input.clone()), additional_tools))
        }
        _ => Ok((Some(input.clone()), additional_tools)),
    }
}

fn append_responses_tool_search_output_tools(item: &Value, tools: &mut Vec<Value>) -> Result<()> {
    let Some(object) = item.as_object() else {
        return Ok(());
    };
    if object.get("type").and_then(Value::as_str) != Some("tool_search_output") {
        return Ok(());
    }
    validate_client_tool_search_execution(object, "tool_search_output")?;
    let loaded = object
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("tool_search_output.tools 必须是数组"))?;
    tools.extend(loaded.iter().cloned());
    Ok(())
}

fn append_responses_additional_tools(item: &Value, tools: &mut Vec<Value>) -> Result<bool> {
    let Some(object) = item.as_object() else {
        return Ok(false);
    };
    if object.get("type").and_then(Value::as_str) != Some("additional_tools") {
        return Ok(false);
    }
    if object.get("role").and_then(Value::as_str) != Some("developer") {
        anyhow::bail!("additional_tools.role 必须是 developer");
    }
    let additional = object
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("additional_tools.tools 必须是数组"))?;
    tools.extend(additional.iter().cloned());
    Ok(true)
}

fn merge_responses_tools(
    configured: Option<&Value>,
    additional: Vec<Value>,
) -> Result<Option<Value>> {
    if configured.is_none() && additional.is_empty() {
        return Ok(None);
    }
    let configured = match configured {
        Some(Value::Array(tools)) => tools.as_slice(),
        Some(_) => anyhow::bail!("tools 必须是数组"),
        None => &[],
    };
    let mut merged = Vec::with_capacity(configured.len() + additional.len());
    let mut identities = HashMap::with_capacity(configured.len() + additional.len());
    for tool in configured {
        if let Some(identity) = responses_tool_identity(tool) {
            if identity.starts_with("function/")
                && identities
                    .get(&identity)
                    .and_then(|index| merged.get(*index))
                    == Some(tool)
            {
                continue;
            }
            identities.entry(identity).or_insert(merged.len());
        }
        merged.push(tool.clone());
    }
    for tool in additional {
        if let Some(identity) = responses_tool_identity(&tool) {
            if let Some(index) = identities.get(&identity).copied() {
                if merged[index] == tool {
                    continue;
                }
                anyhow::bail!("additional_tools 包含定义冲突的工具 {identity}");
            }
            identities.insert(identity, merged.len());
        }
        merged.push(tool);
    }
    Ok(Some(Value::Array(merged)))
}

fn responses_tool_identity(tool: &Value) -> Option<String> {
    let object = tool.as_object()?;
    let tool_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("function");
    if matches!(tool_type, "web_search" | "web_search_preview") {
        return Some("web_search".to_string());
    }
    if tool_type == "tool_search" {
        let execution = object
            .get("execution")
            .and_then(Value::as_str)
            .unwrap_or("server");
        return Some(format!("tool_search/{execution}"));
    }
    let name = object
        .get("name")
        .or_else(|| {
            object
                .get("function")
                .and_then(|function| function.get("name"))
        })
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())?;
    Some(format!("{tool_type}/{name}"))
}

fn chat_web_search_options_for_tool_choice(
    options: Option<&Value>,
    tool_choice: Option<&Value>,
    has_chat_tools: bool,
) -> Result<Option<Value>> {
    let Some(tool_choice) = tool_choice else {
        if options.is_some() {
            anyhow::bail!(
                "Responses 可选 web_search 不能无损转换为 Chat Completions；请明确选择 web_search、required 或 none"
            );
        }
        return Ok(None);
    };
    match tool_choice {
        Value::String(choice) if choice == "auto" => {
            // Codex app-server currently attaches an ambient optional
            // `web_search` tool even when the selected model descriptor does
            // not advertise search. Adapted Chat/Anthropic routes cannot
            // execute that hosted Responses tool, but `auto` also proves the
            // caller did not require it. Remove only this ambient declaration;
            // explicit and required search choices remain fail-closed or use
            // the dedicated Chat search mapping below.
            Ok(None)
        }
        Value::String(choice) if choice == "none" => Ok(None),
        Value::String(choice) if choice == "required" => {
            let Some(options) = options else {
                return Ok(None);
            };
            if has_chat_tools {
                anyhow::bail!(
                    "Responses tool_choice=required 同时包含 web_search 和其他工具，Chat Completions 无法无损表达"
                );
            }
            Ok(Some(options.clone()))
        }
        Value::Object(object) => match object.get("type").and_then(Value::as_str) {
            Some("web_search" | "web_search_preview") => {
                let options = options.ok_or_else(|| {
                    anyhow::anyhow!("Responses tool_choice 选择了未在 tools 中声明的 web_search")
                })?;
                if has_chat_tools {
                    anyhow::bail!(
                        "Responses tool_choice 明确选择 web_search 时仍包含其他工具，Chat Completions 无法无损表达"
                    );
                }
                Ok(Some(options.clone()))
            }
            Some("function" | "custom" | "tool_search") => Ok(None),
            Some(tool_type) => anyhow::bail!(
                "Responses tool_choice 类型 {tool_type} 不能转换为 Chat Completions tool_choice"
            ),
            None => anyhow::bail!("Responses tool_choice 缺少 type"),
        },
        _ => anyhow::bail!("tool_choice 必须是 auto/none/required 或对象"),
    }
}

fn reject_unportable_chat_web_search_include(include: Option<&Value>) -> Result<()> {
    let Some(include) = include else {
        return Ok(());
    };
    let entries = include
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Responses include 必须是数组"))?;
    if entries
        .iter()
        .any(|entry| entry.as_str() == Some("web_search_call.action.sources"))
    {
        anyhow::bail!(
            "Responses include=web_search_call.action.sources 不能无损转换为 Chat Completions"
        );
    }
    Ok(())
}

fn responses_tool_choice_targets_web_search(tool_choice: &Value) -> Result<bool> {
    match tool_choice {
        Value::Object(object) => match object.get("type").and_then(Value::as_str) {
            Some("web_search" | "web_search_preview") => Ok(true),
            Some("function" | "custom" | "tool_search") => Ok(false),
            Some(tool_type) => anyhow::bail!(
                "Responses tool_choice 类型 {tool_type} 不能转换为 Chat Completions tool_choice"
            ),
            None => anyhow::bail!("Responses tool_choice 缺少 type"),
        },
        _ => Ok(false),
    }
}

fn should_omit_chat_tool_choice_for_web_search(
    tool_choice: &Value,
    has_chat_tools: bool,
    web_search_tool_seen: bool,
    web_search_enabled: bool,
) -> Result<bool> {
    if !web_search_tool_seen {
        return responses_tool_choice_targets_web_search(tool_choice);
    }
    if !has_chat_tools {
        return match tool_choice {
            Value::String(choice) if matches!(choice.as_str(), "auto" | "none" | "required") => {
                Ok(true)
            }
            Value::Object(object)
                if matches!(
                    object.get("type").and_then(Value::as_str),
                    Some("web_search" | "web_search_preview")
                ) =>
            {
                Ok(true)
            }
            _ => responses_tool_choice_targets_web_search(tool_choice),
        };
    }
    if web_search_enabled {
        responses_tool_choice_targets_web_search(tool_choice)
    } else {
        Ok(false)
    }
}

const DEFAULT_ANTHROPIC_MAX_TOKENS: u64 = 8192;

#[cfg(test)]
fn responses_to_anthropic_messages_body(body: &Value) -> Result<Value> {
    Ok(responses_to_anthropic_messages_request(body)?.body)
}

fn responses_to_anthropic_messages_request(body: &Value) -> Result<ConvertedResponsesRequest> {
    // Reuse the normalized Chat message representation so Responses message,
    // image, function-call and function-result variants have one parser. The
    // second stage below changes only the Anthropic-specific wire semantics.
    let ConvertedResponsesRequest {
        body: chat,
        tool_bridge,
    } = responses_to_chat_completions_request(body)?;
    let mut chat = match chat {
        Value::Object(chat) => chat,
        _ => anyhow::bail!("Responses 请求无法归一化为消息对象"),
    };
    for unsupported in [
        "presence_penalty",
        "frequency_penalty",
        "seed",
        "logit_bias",
        "logprobs",
        "top_logprobs",
        "web_search_options",
    ] {
        if chat.contains_key(unsupported) {
            anyhow::bail!("Anthropic Messages 不支持 Responses 字段 {unsupported}");
        }
    }
    if let Some(response_format) = chat.get("response_format")
        && response_format.get("type").and_then(Value::as_str) != Some("text")
    {
        anyhow::bail!("Anthropic Messages 暂不支持当前 Responses 结构化输出格式");
    }

    let model = chat
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("缺少 model 字段"))?
        .to_string();
    let mut system_parts = Vec::new();
    let mut messages = Vec::new();
    let chat_messages = match chat.remove("messages") {
        Some(Value::Array(messages)) => messages,
        _ => Vec::new(),
    };
    for message in chat_messages {
        append_anthropic_message_from_chat(&message, &mut system_parts, &mut messages)?;
    }
    if messages.is_empty() {
        anyhow::bail!("缺少可转换为 Anthropic Messages messages 的 input");
    }

    let max_tokens = chat
        .get("max_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_ANTHROPIC_MAX_TOKENS);
    if max_tokens == 0 {
        anyhow::bail!("max_output_tokens 必须大于 0");
    }
    let mut anthropic = serde_json::Map::from_iter([
        ("model".to_string(), Value::String(model)),
        ("messages".to_string(), Value::Array(messages)),
        ("max_tokens".to_string(), Value::Number(max_tokens.into())),
        (
            "stream".to_string(),
            Value::Bool(chat.get("stream").and_then(Value::as_bool).unwrap_or(false)),
        ),
    ]);
    if !system_parts.is_empty() {
        anthropic.insert(
            "system".to_string(),
            Value::String(system_parts.join("\n\n")),
        );
    }
    copy_number_or_string_field(&chat, &mut anthropic, "temperature", "temperature");
    copy_number_or_string_field(&chat, &mut anthropic, "top_p", "top_p");
    if let Some(top_k) = body.get("top_k") {
        if !top_k.is_number() {
            anyhow::bail!("top_k 必须是数字");
        }
        anthropic.insert("top_k".to_string(), top_k.clone());
    }
    if let Some(stop) = chat.get("stop") {
        anthropic.insert(
            "stop_sequences".to_string(),
            anthropic_stop_sequences(stop)?,
        );
    }
    if let Some(user_id) = chat
        .get("user")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        anthropic.insert("metadata".to_string(), json!({"user_id":user_id}));
    }
    if let Some(effort) = chat
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .map(normalize_anthropic_effort)
    {
        anthropic.insert("output_config".to_string(), json!({"effort":effort}));
    }

    let parallel_tool_calls = chat
        .get("parallel_tool_calls")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if let Some(tools) = chat.remove("tools") {
        let converted_tools = chat_tools_to_anthropic_tools(&tools)?;
        let mut tool_choice = chat
            .get("tool_choice")
            .map(chat_tool_choice_to_anthropic_tool_choice)
            .transpose()?;
        if tool_choice.is_none()
            && let Some(function_call) = chat.get("function_call")
        {
            tool_choice = Some(chat_function_call_to_anthropic_tool_choice(function_call)?);
        }
        if tool_choice
            .as_ref()
            .is_some_and(|choice| choice.get("type").and_then(Value::as_str) == Some("none"))
        {
            tool_choice = None;
        } else {
            anthropic.insert("tools".to_string(), converted_tools);
            if !parallel_tool_calls {
                let choice = tool_choice.get_or_insert_with(|| json!({"type":"auto"}));
                choice
                    .as_object_mut()
                    .expect("Anthropic tool choice must be an object")
                    .insert("disable_parallel_tool_use".to_string(), Value::Bool(true));
            }
        }
        if let Some(tool_choice) = tool_choice {
            anthropic.insert("tool_choice".to_string(), tool_choice);
        }
    }
    Ok(ConvertedResponsesRequest {
        body: Value::Object(anthropic),
        tool_bridge,
    })
}

fn append_anthropic_message_from_chat(
    message: &Value,
    system_parts: &mut Vec<String>,
    messages: &mut Vec<Value>,
) -> Result<()> {
    let message = message
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("归一化消息必须是对象"))?;
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("归一化消息缺少 role"))?;
    if role == "system" {
        let text = message
            .get("content")
            .map(chat_message_content_text)
            .unwrap_or_default();
        if !text.is_empty() {
            system_parts.push(text);
        }
        return Ok(());
    }
    if role == "tool" {
        let call_id = message
            .get("tool_call_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("tool 消息缺少 tool_call_id"))?;
        let content = json_value_as_chat_string(message.get("content")).unwrap_or_default();
        return push_anthropic_message(
            messages,
            "user",
            vec![json!({
                "type":"tool_result",
                "tool_use_id":call_id,
                "content":content,
            })],
        );
    }
    if !matches!(role, "user" | "assistant") {
        anyhow::bail!("Anthropic Messages 不支持消息角色 {role}");
    }
    let mut blocks = message
        .get("content")
        .map(|content| chat_content_to_anthropic_blocks(content, role == "user"))
        .transpose()?
        .unwrap_or_default();
    if let Some(tool_calls) = message.get("tool_calls") {
        blocks.extend(chat_tool_calls_to_anthropic_blocks(tool_calls)?);
    }
    if let Some(function_call) = message.get("function_call") {
        blocks.extend(chat_legacy_function_call_to_anthropic_blocks(
            function_call,
        )?);
    }
    if blocks.is_empty() {
        return Ok(());
    }
    push_anthropic_message(messages, role, blocks)
}

fn push_anthropic_message(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) -> Result<()> {
    if let Some(last) = messages.last_mut().and_then(Value::as_object_mut)
        && last.get("role").and_then(Value::as_str) == Some(role)
    {
        let content = last
            .get_mut("content")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| anyhow::anyhow!("Anthropic message.content 必须是数组"))?;
        content.extend(blocks);
        return Ok(());
    }
    messages.push(json!({"role":role,"content":blocks}));
    Ok(())
}

fn chat_content_to_anthropic_blocks(content: &Value, allow_images: bool) -> Result<Vec<Value>> {
    match content {
        Value::Null => Ok(Vec::new()),
        Value::String(text) => Ok(if text.is_empty() {
            Vec::new()
        } else {
            vec![json!({"type":"text","text":text})]
        }),
        Value::Array(parts) => {
            let mut blocks = Vec::new();
            for part in parts {
                let part = part
                    .as_object()
                    .ok_or_else(|| anyhow::anyhow!("消息 content 条目必须是对象"))?;
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let text = part
                            .get("text")
                            .and_then(Value::as_str)
                            .ok_or_else(|| anyhow::anyhow!("text content 缺少 text"))?;
                        blocks.push(json!({"type":"text","text":text}));
                    }
                    Some("image_url") if allow_images => {
                        let image_url = part
                            .get("image_url")
                            .ok_or_else(|| anyhow::anyhow!("image_url content 缺少 image_url"))?;
                        blocks.push(json!({
                            "type":"image",
                            "source":chat_image_url_to_anthropic_source(image_url)?,
                        }));
                    }
                    Some("image_url") => anyhow::bail!("Anthropic Messages 只允许用户消息包含图片"),
                    Some(part_type) => {
                        anyhow::bail!("消息 content 类型 {part_type} 不能转换为 Anthropic Messages")
                    }
                    None => anyhow::bail!("消息 content 条目缺少 type"),
                }
            }
            Ok(blocks)
        }
        _ => anyhow::bail!("消息 content 必须是字符串或数组"),
    }
}

fn chat_image_url_to_anthropic_source(image_url: &Value) -> Result<Value> {
    let url = match image_url {
        Value::String(url) => url.as_str(),
        Value::Object(object) => object
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("image_url 缺少 url"))?,
        _ => anyhow::bail!("image_url 必须是字符串或对象"),
    };
    if let Some(data) = url.strip_prefix("data:") {
        let (media_type, data) = data
            .split_once(";base64,")
            .ok_or_else(|| anyhow::anyhow!("Anthropic 图片 data URL 必须使用 base64"))?;
        if !matches!(
            media_type,
            "image/jpeg" | "image/png" | "image/gif" | "image/webp"
        ) {
            anyhow::bail!("Anthropic Messages 不支持图片类型 {media_type}");
        }
        if data.is_empty() {
            anyhow::bail!("Anthropic 图片 data URL 缺少数据");
        }
        return Ok(json!({"type":"base64","media_type":media_type,"data":data}));
    }
    let parsed = reqwest::Url::parse(url).context("图片 URL 格式无效")?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        anyhow::bail!("Anthropic 图片 URL 必须是 HTTP(S) 或 base64 data URL");
    }
    Ok(json!({"type":"url","url":url}))
}

fn chat_tool_calls_to_anthropic_blocks(tool_calls: &Value) -> Result<Vec<Value>> {
    let tool_calls = tool_calls
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("tool_calls 必须是数组"))?;
    tool_calls
        .iter()
        .map(|tool_call| {
            let tool_call = tool_call
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("tool_call 必须是对象"))?;
            let id = tool_call
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow::anyhow!("tool_call 缺少 id"))?;
            let function = tool_call
                .get("function")
                .and_then(Value::as_object)
                .ok_or_else(|| anyhow::anyhow!("tool_call 缺少 function"))?;
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow::anyhow!("tool_call.function 缺少 name"))?;
            let input = parse_anthropic_tool_input(function.get("arguments"))?;
            Ok(json!({"type":"tool_use","id":id,"name":name,"input":input}))
        })
        .collect()
}

fn chat_legacy_function_call_to_anthropic_blocks(function_call: &Value) -> Result<Vec<Value>> {
    let function_call = function_call
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("function_call 必须是对象"))?;
    let name = function_call
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("function_call 缺少 name"))?;
    Ok(vec![json!({
        "type":"tool_use",
        "id":format!("call_codey_{}", Uuid::new_v4()),
        "name":name,
        "input":parse_anthropic_tool_input(function_call.get("arguments"))?,
    })])
}

fn parse_anthropic_tool_input(arguments: Option<&Value>) -> Result<Value> {
    let input = match arguments {
        None | Some(Value::Null) => json!({}),
        Some(Value::Object(object)) => Value::Object(object.clone()),
        Some(Value::String(arguments)) if arguments.trim().is_empty() => json!({}),
        Some(Value::String(arguments)) => {
            serde_json::from_str::<Value>(arguments).context("工具调用 arguments 不是有效 JSON")?
        }
        Some(value) => value.clone(),
    };
    if !input.is_object() {
        anyhow::bail!("Anthropic tool_use.input 必须是 JSON 对象");
    }
    Ok(input)
}

fn chat_tools_to_anthropic_tools(tools: &Value) -> Result<Value> {
    let tools = tools
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("tools 必须是数组"))?;
    let mut converted = Vec::new();
    for tool in tools {
        let tool = tool
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("tool 条目必须是对象"))?;
        if tool.get("type").and_then(Value::as_str) != Some("function") {
            anyhow::bail!("Anthropic Messages 只支持 Responses function 工具");
        }
        let function = tool
            .get("function")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("function tool 缺少 function"))?;
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("function tool 缺少 name"))?;
        let input_schema = function
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({"type":"object","properties":{}}));
        if !input_schema.is_object() {
            anyhow::bail!("function tool.parameters 必须是 JSON Schema 对象");
        }
        let mut converted_tool = serde_json::Map::from_iter([
            ("name".to_string(), Value::String(name.to_string())),
            ("input_schema".to_string(), input_schema),
        ]);
        if let Some(description) = function.get("description").and_then(Value::as_str) {
            converted_tool.insert(
                "description".to_string(),
                Value::String(description.to_string()),
            );
        }
        converted.push(Value::Object(converted_tool));
    }
    Ok(Value::Array(converted))
}

fn chat_tool_choice_to_anthropic_tool_choice(tool_choice: &Value) -> Result<Value> {
    match tool_choice {
        Value::String(choice) => match choice.as_str() {
            "auto" => Ok(json!({"type":"auto"})),
            "required" => Ok(json!({"type":"any"})),
            "none" => Ok(json!({"type":"none"})),
            _ => anyhow::bail!("Anthropic Messages 不支持 tool_choice={choice}"),
        },
        Value::Object(object) => {
            let name = object
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow::anyhow!("function tool_choice 缺少 name"))?;
            Ok(json!({"type":"tool","name":name}))
        }
        _ => anyhow::bail!("tool_choice 必须是字符串或 function 对象"),
    }
}

fn chat_function_call_to_anthropic_tool_choice(function_call: &Value) -> Result<Value> {
    match function_call {
        Value::String(choice) if choice == "auto" => Ok(json!({"type":"auto"})),
        Value::String(choice) if choice == "none" => Ok(json!({"type":"none"})),
        Value::Object(function) => {
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow::anyhow!("function_call 缺少 name"))?;
            Ok(json!({"type":"tool","name":name}))
        }
        _ => anyhow::bail!("function_call 不能转换为 Anthropic tool_choice"),
    }
}

fn anthropic_stop_sequences(stop: &Value) -> Result<Value> {
    match stop {
        Value::String(stop) => Ok(Value::Array(vec![Value::String(stop.clone())])),
        Value::Array(stops) if stops.iter().all(Value::is_string) => {
            Ok(Value::Array(stops.clone()))
        }
        _ => anyhow::bail!("stop 必须是字符串或字符串数组"),
    }
}

fn normalize_anthropic_effort(effort: &str) -> &'static str {
    let effort = effort.trim();
    if effort.eq_ignore_ascii_case("low") || effort.eq_ignore_ascii_case("minimal") {
        "low"
    } else if effort.eq_ignore_ascii_case("medium") {
        "medium"
    } else if effort.eq_ignore_ascii_case("max")
        || effort.eq_ignore_ascii_case("xhigh")
        || effort.eq_ignore_ascii_case("ultra")
    {
        "max"
    } else {
        "high"
    }
}

fn responses_text_format_to_chat_response_format(format: &Value) -> Result<Value> {
    let object = format
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("text.format 必须是对象"))?;
    match object.get("type").and_then(Value::as_str) {
        Some("text") => Ok(json!({"type":"text"})),
        Some("json_object") => Ok(json!({"type":"json_object"})),
        Some("json_schema") => {
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| anyhow::anyhow!("text.format.json_schema 缺少 name"))?;
            let schema = object
                .get("schema")
                .ok_or_else(|| anyhow::anyhow!("text.format.json_schema 缺少 schema"))?;
            let mut json_schema = serde_json::Map::from_iter([
                ("name".to_string(), Value::String(name.to_string())),
                ("schema".to_string(), schema.clone()),
            ]);
            for field in ["description", "strict"] {
                if let Some(value) = object.get(field) {
                    json_schema.insert(field.to_string(), value.clone());
                }
            }
            Ok(json!({
                "type": "json_schema",
                "json_schema": Value::Object(json_schema),
            }))
        }
        Some(format_type) => {
            anyhow::bail!("Responses text.format 类型 {format_type} 不能转换为 Chat Completions")
        }
        None => anyhow::bail!("text.format 缺少 type"),
    }
}

fn append_chat_messages_from_responses_input(
    input: Option<&Value>,
    messages: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    let Some(input) = input else {
        return Ok(());
    };
    match input {
        Value::String(text) => push_chat_text_message(messages, "user", text),
        Value::Array(items) => {
            for item in items {
                append_chat_message_item(item, messages, tool_bridge)?;
            }
            Ok(())
        }
        Value::Object(_) => append_chat_message_item(input, messages, tool_bridge),
        _ => anyhow::bail!("input 必须是字符串、对象或数组"),
    }
}

fn append_chat_message_item(
    item: &Value,
    messages: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    match item {
        Value::String(text) => push_chat_text_message(messages, "user", text),
        Value::Object(object) => match object.get("type").and_then(Value::as_str) {
            Some("message") => append_responses_message_object(object, messages, tool_bridge),
            Some("agent_message") => {
                append_responses_agent_message_object(object, messages, tool_bridge)
            }
            None if looks_like_message_object(object) => {
                append_responses_message_object(object, messages, tool_bridge)
            }
            Some("function_call") => {
                append_responses_function_call_item(object, messages, tool_bridge)
            }
            Some("function_call_output") => {
                append_responses_tool_call_output_item(object, messages, "function_call_output")
            }
            Some("custom_tool_call") => {
                append_responses_custom_tool_call_item(object, messages, tool_bridge)
            }
            Some("custom_tool_call_output") => {
                append_responses_tool_call_output_item(object, messages, "custom_tool_call_output")
            }
            Some("tool_search_call") => {
                append_responses_tool_search_call_item(object, messages, tool_bridge)
            }
            Some("tool_search_output") => {
                append_responses_tool_search_output_item(object, messages)
            }
            Some("web_search_call")
                if object.get("status").and_then(Value::as_str) == Some("completed") =>
            {
                Ok(())
            }
            Some(item_type) if is_opaque_responses_input_item_type(item_type) => Ok(()),
            Some("input_text" | "output_text" | "text" | "input_image" | "image_url") => {
                append_single_content_part_as_user_message(item, messages)
            }
            Some(item_type) => anyhow::bail!(
                "Responses input item 类型 {item_type} 不能无损转换为 Chat Completions message"
            ),
            None => anyhow::bail!("Responses input item 缺少可转换的 role/content/type 字段"),
        },
        _ => anyhow::bail!("Responses input 数组只能包含字符串或对象"),
    }
}

fn looks_like_message_object(object: &serde_json::Map<String, Value>) -> bool {
    object.contains_key("role")
        || object.contains_key("content")
        || object.contains_key("tool_calls")
        || object.contains_key("function_call")
}

fn append_responses_agent_message_object(
    object: &serde_json::Map<String, Value>,
    messages: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    let mut message = object.clone();
    message.insert("role".to_string(), Value::String("assistant".to_string()));
    if !message.contains_key("content")
        && let Some(text) = message.get("message").and_then(Value::as_str)
    {
        message.insert("content".to_string(), Value::String(text.to_string()));
    }
    append_responses_message_object(&message, messages, tool_bridge)
}

fn append_responses_message_object(
    object: &serde_json::Map<String, Value>,
    messages: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    let role = object
        .get("role")
        .and_then(Value::as_str)
        .map(normalize_chat_role)
        .transpose()?
        .unwrap_or("user");
    if role == "tool" {
        return append_responses_tool_call_output_item(object, messages, "tool message");
    }
    let mut message = serde_json::Map::new();
    message.insert("role".to_string(), Value::String(role.to_string()));
    let chat_content = object
        .get("content")
        .map(|content| responses_content_to_chat_content(content, role))
        .transpose()?
        .flatten();
    if let Some(chat_content) = chat_content {
        message.insert("content".to_string(), chat_content);
    } else if let Some(text) = first_text_field(object) {
        message.insert("content".to_string(), Value::String(text.to_string()));
    }
    if let Some(tool_calls) = object.get("tool_calls") {
        message.insert(
            "tool_calls".to_string(),
            normalize_chat_tool_calls(tool_calls, tool_bridge)?,
        );
        message.entry("content".to_string()).or_insert(Value::Null);
    }
    if let Some(function_call) = object.get("function_call") {
        message.insert(
            "function_call".to_string(),
            normalize_chat_legacy_function_call(function_call, tool_bridge)?,
        );
        message.entry("content".to_string()).or_insert(Value::Null);
    }
    if message.contains_key("content")
        || message.contains_key("tool_calls")
        || message.contains_key("function_call")
    {
        messages.push(Value::Object(message));
    }
    Ok(())
}

fn normalize_chat_role(role: &str) -> Result<&'static str> {
    match role.trim() {
        "user" => Ok("user"),
        "assistant" => Ok("assistant"),
        "system" | "developer" => Ok("system"),
        "tool" => Ok("tool"),
        other => anyhow::bail!("不支持的 Responses message role：{other}"),
    }
}

fn first_text_field(object: &serde_json::Map<String, Value>) -> Option<&str> {
    object
        .get("text")
        .or_else(|| object.get("input_text"))
        .or_else(|| object.get("output_text"))
        .or_else(|| object.get("message"))
        .and_then(Value::as_str)
}

fn first_visible_content_part_text(object: &serde_json::Map<String, Value>) -> Option<&str> {
    first_text_field(object).or_else(|| object.get("refusal").and_then(Value::as_str))
}

// These items are provider-maintained state from Responses history. They are not
// visible model-facing text, and Chat Completions has no field that can carry them.
fn is_opaque_responses_input_item_type(item_type: &str) -> bool {
    matches!(item_type, "encrypted_content" | "reasoning" | "compaction")
}

fn is_opaque_responses_content_part_type(part_type: &str) -> bool {
    matches!(part_type, "encrypted_content" | "reasoning" | "compaction")
}

fn responses_content_to_chat_content(content: &Value, role: &str) -> Result<Option<Value>> {
    match content {
        Value::String(text) => Ok((!text.is_empty()).then(|| Value::String(text.clone()))),
        Value::Array(parts) => {
            let mut chat_parts = Vec::new();
            for part in parts {
                if let Some(chat_part) = responses_content_part_to_chat_part(part)? {
                    chat_parts.push(chat_part);
                }
            }
            if chat_parts.is_empty() {
                return Ok(None);
            }
            let has_image = chat_parts
                .iter()
                .any(|part| part.get("type").and_then(Value::as_str) == Some("image_url"));
            if role != "user" {
                if has_image {
                    anyhow::bail!(
                        "Chat Completions 只支持把用户消息中的 Responses 图片内容无损转换"
                    );
                }
                let text = chat_parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Ok((!text.is_empty()).then_some(Value::String(text)));
            }
            Ok(Some(Value::Array(chat_parts)))
        }
        Value::Object(_) => responses_content_part_to_chat_part(content)
            .map(|part| part.map(|part| Value::Array(vec![part]))),
        _ => anyhow::bail!("message.content 必须是字符串、对象或数组"),
    }
}

fn responses_content_part_to_chat_part(part: &Value) -> Result<Option<Value>> {
    match part {
        Value::String(text) => Ok(Some(json!({"type":"text","text":text}))),
        Value::Object(object) => {
            let part_type = object.get("type").and_then(Value::as_str);
            match part_type {
                Some("input_text" | "output_text" | "text" | "refusal") => {
                    let text = first_visible_content_part_text(object)
                        .ok_or_else(|| anyhow::anyhow!("文本 content part 缺少 text"))?;
                    Ok(Some(json!({"type":"text","text":text})))
                }
                None if first_visible_content_part_text(object).is_some() => {
                    let text = first_visible_content_part_text(object).unwrap_or_default();
                    Ok(Some(json!({"type":"text","text":text})))
                }
                Some("input_image" | "image_url") => Ok(Some(json!({
                    "type": "image_url",
                    "image_url": responses_image_url_to_chat_image_url(object)?
                }))),
                None if object.contains_key("image_url") || object.contains_key("url") => {
                    Ok(Some(json!({
                        "type": "image_url",
                        "image_url": responses_image_url_to_chat_image_url(object)?
                    })))
                }
                Some(part_type) if is_opaque_responses_content_part_type(part_type) => Ok(None),
                None if object.contains_key("encrypted_content") => Ok(None),
                Some(part_type) => anyhow::bail!(
                    "Responses content part 类型 {part_type} 不能无损转换为 Chat Completions content"
                ),
                None => anyhow::bail!("Responses content part 缺少 text 或 image_url"),
            }
        }
        _ => anyhow::bail!("Responses content part 必须是字符串或对象"),
    }
}

fn responses_image_url_to_chat_image_url(object: &serde_json::Map<String, Value>) -> Result<Value> {
    if object.contains_key("file_id")
        && !object.contains_key("image_url")
        && !object.contains_key("url")
    {
        anyhow::bail!(
            "input_image.file_id 依赖 Responses 文件状态，不能无损转换为 Chat Completions"
        );
    }
    let mut image_url = match object.get("image_url").or_else(|| object.get("url")) {
        Some(Value::String(url)) if !url.is_empty() => json!({ "url": url }),
        Some(Value::Object(image_url)) => Value::Object(image_url.clone()),
        Some(_) => anyhow::bail!("input_image.image_url 必须是字符串或对象"),
        None => anyhow::bail!("input_image 缺少 image_url"),
    };
    if let Some(detail) = object.get("detail") {
        let detail = detail
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("input_image.detail 必须是字符串"))?;
        if !matches!(detail, "auto" | "low" | "high") {
            anyhow::bail!(
                "input_image.detail={detail} 不能无损转换为 Chat Completions image_url.detail"
            );
        }
        if let Some(image_url) = image_url.as_object_mut() {
            image_url
                .entry("detail".to_string())
                .or_insert_with(|| Value::String(detail.to_string()));
        }
    }
    Ok(image_url)
}

fn append_single_content_part_as_user_message(
    item: &Value,
    messages: &mut Vec<Value>,
) -> Result<()> {
    if let Some(chat_part) = responses_content_part_to_chat_part(item)? {
        messages.push(json!({"role":"user","content":[chat_part]}));
    }
    Ok(())
}

fn append_responses_function_call_item(
    object: &serde_json::Map<String, Value>,
    messages: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    let call_id = object
        .get("call_id")
        .or_else(|| object.get("id"))
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("function_call 缺少 call_id"))?;
    let tool_name = responses_tool_name_from_call_object(object, "function_call")?;
    let upstream_name = tool_bridge.upstream_name_for_call(&tool_name)?;
    let arguments =
        json_value_as_chat_string(object.get("arguments")).unwrap_or_else(|| "{}".to_string());
    append_chat_assistant_tool_call(messages, call_id, &upstream_name, &arguments)
}

fn append_responses_custom_tool_call_item(
    object: &serde_json::Map<String, Value>,
    messages: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    let call_id = object
        .get("call_id")
        .or_else(|| object.get("id"))
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("custom_tool_call 缺少 call_id"))?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("custom_tool_call 缺少 name"))?;
    let input = object
        .get("input")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("custom_tool_call.input 必须是字符串"))?;
    let namespace =
        responses_namespace_path(object.get("namespace"), "custom_tool_call.namespace")?;
    let tool_name = ResponsesToolName::custom_in_namespace(&namespace, name);
    let upstream_name = tool_bridge.upstream_name_for_call(&tool_name)?;
    let arguments = wrap_custom_tool_input(input)?;
    append_chat_assistant_tool_call(messages, call_id, &upstream_name, &arguments)
}

fn append_responses_tool_search_call_item(
    object: &serde_json::Map<String, Value>,
    messages: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    if object.get("execution").and_then(Value::as_str) != Some("client") {
        anyhow::bail!("tool_search_call 只允许 execution=client 的历史调用");
    }
    let call_id = object
        .get("call_id")
        .or_else(|| object.get("id"))
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("tool_search_call 缺少 call_id"))?;
    let upstream_name = tool_bridge.upstream_name_for_call(&ResponsesToolName::tool_search())?;
    let arguments = object
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        anyhow::bail!("tool_search_call.arguments 必须是 JSON 对象");
    }
    let arguments =
        serde_json::to_string(&arguments).context("序列化 tool_search_call.arguments 失败")?;
    append_chat_assistant_tool_call(messages, call_id, &upstream_name, &arguments)
}

fn append_responses_tool_search_output_item(
    object: &serde_json::Map<String, Value>,
    messages: &mut Vec<Value>,
) -> Result<()> {
    validate_client_tool_search_execution(object, "tool_search_output")?;
    let call_id = object
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("tool_search_output 缺少 call_id"))?;
    let tools = object
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("tool_search_output.tools 必须是数组"))?;
    let content = serde_json::to_string(&json!({"tools":tools}))
        .context("序列化 tool_search_output.tools 失败")?;
    messages.push(json!({
        "role":"tool",
        "tool_call_id":call_id,
        "content":content,
    }));
    Ok(())
}

fn validate_client_tool_search_execution(
    object: &serde_json::Map<String, Value>,
    context: &str,
) -> Result<()> {
    match object.get("execution").and_then(Value::as_str) {
        Some("client") => Ok(()),
        Some(execution) => anyhow::bail!("{context}.execution={execution} 不受支持；必须是 client"),
        None => anyhow::bail!("{context} 缺少 execution=client"),
    }
}

fn append_chat_assistant_tool_call(
    messages: &mut Vec<Value>,
    call_id: &str,
    upstream_name: &str,
    arguments: &str,
) -> Result<()> {
    let tool_call = json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": upstream_name,
            "arguments": arguments
        }
    });
    if let Some(last) = messages.last_mut().and_then(Value::as_object_mut)
        && last.get("role").and_then(Value::as_str) == Some("assistant")
        && last
            .get("content")
            .is_none_or(|content| content.is_null() || content.as_str() == Some(""))
        && !last.contains_key("function_call")
    {
        last.entry("content".to_string()).or_insert(Value::Null);
        match last.get_mut("tool_calls") {
            Some(Value::Array(tool_calls)) => {
                tool_calls.push(tool_call);
                return Ok(());
            }
            Some(_) => anyhow::bail!("assistant message.tool_calls 必须是数组"),
            None => {
                last.insert("tool_calls".to_string(), Value::Array(vec![tool_call]));
                return Ok(());
            }
        }
    }
    messages.push(json!({
        "role": "assistant",
        "content": Value::Null,
        "tool_calls": [tool_call]
    }));
    Ok(())
}

fn append_responses_tool_call_output_item(
    object: &serde_json::Map<String, Value>,
    messages: &mut Vec<Value>,
    context: &str,
) -> Result<()> {
    let call_id = object
        .get("call_id")
        .or_else(|| object.get("tool_call_id"))
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{context} 缺少 call_id"))?;
    let output = object
        .get("output")
        .or_else(|| object.get("content"))
        .ok_or_else(|| anyhow::anyhow!("{context} 缺少 output"))?;
    let (content, images) = responses_tool_output_content(output)?.unwrap_or_else(|| {
        (
            json_value_as_chat_string(Some(output)).unwrap_or_default(),
            Vec::new(),
        )
    });
    messages.push(json!({
        "role": "tool",
        "tool_call_id": call_id,
        "content": content,
    }));
    if !images.is_empty() {
        messages.push(json!({
            "role": "user",
            "content": images,
        }));
    }
    Ok(())
}

fn responses_tool_output_content(output: &Value) -> Result<Option<(String, Vec<Value>)>> {
    let parts = match output {
        Value::Array(parts) => parts.as_slice(),
        Value::Object(_) => std::slice::from_ref(output),
        _ => return Ok(None),
    };
    let mut text = Vec::new();
    let mut images = Vec::new();
    for part in parts {
        let Some(part) = part.as_object() else {
            return Ok(None);
        };
        match part.get("type").and_then(Value::as_str) {
            Some("input_text" | "output_text" | "text" | "refusal") => {
                text.push(
                    first_visible_content_part_text(part)
                        .ok_or_else(|| anyhow::anyhow!("工具文本输出缺少 text"))?
                        .to_string(),
                );
            }
            Some("input_image" | "image_url") => images.push(json!({
                "type": "image_url",
                "image_url": responses_image_url_to_chat_image_url(part)?,
            })),
            Some(part_type) if is_opaque_responses_content_part_type(part_type) => {}
            _ => return Ok(None),
        }
    }
    Ok(Some((text.join("\n"), images)))
}

fn normalize_chat_tool_calls(
    tool_calls: &Value,
    tool_bridge: &ResponsesToolBridge,
) -> Result<Value> {
    let tool_calls = tool_calls
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("tool_calls 必须是数组"))?;
    let mut normalized = Vec::new();
    for tool_call in tool_calls {
        let object = tool_call
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("tool_calls 条目必须是对象"))?;
        let call_type = object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("function");
        if call_type != "function" {
            anyhow::bail!("Chat tool_call 类型 {call_type} 不支持");
        }
        let function = object
            .get("function")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("tool_call 缺少 function"))?;
        let tool_name = responses_tool_name_from_function_object(
            function,
            object.get("namespace"),
            "tool_call.function",
        )?;
        let upstream_name = tool_bridge.upstream_name_for_call(&tool_name)?;
        let arguments = json_value_as_chat_string(function.get("arguments"))
            .unwrap_or_else(|| "{}".to_string());
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("call_codey_{}", Uuid::new_v4()));
        normalized.push(json!({
            "id": id,
            "type": "function",
            "function": {
                "name": upstream_name,
                "arguments": arguments,
            }
        }));
    }
    Ok(Value::Array(normalized))
}

fn normalize_chat_legacy_function_call(
    function_call: &Value,
    tool_bridge: &ResponsesToolBridge,
) -> Result<Value> {
    match function_call {
        Value::String(choice) if matches!(choice.as_str(), "auto" | "none") => {
            Ok(Value::String(choice.clone()))
        }
        Value::Object(object) => {
            let tool_name = responses_tool_name_from_call_object(object, "function_call")?;
            let upstream_name = tool_bridge.upstream_name_for_call(&tool_name)?;
            Ok(json!({"name": upstream_name}))
        }
        _ => anyhow::bail!("function_call 必须是 auto/none 或包含 name 的对象"),
    }
}

const NAMESPACE_UPSTREAM_TOOL_PREFIX: &str = "codey_ns__";
const CUSTOM_UPSTREAM_TOOL_PREFIX: &str = "codey_custom__";
const TOOL_SEARCH_UPSTREAM_TOOL_NAME: &str = "codey_tool_search__client__bridge_v1";
const UPSTREAM_FUNCTION_NAME_MAX_BYTES: usize = 64;
const MAX_RESPONSES_NAMESPACE_DEPTH: usize = 8;

#[derive(Default)]
struct ResponsesChatTools {
    tools: Option<Value>,
    web_search_options: Option<Value>,
    web_search_tool_seen: bool,
}

struct ResponsesChatToolsConversion<'a> {
    tool_bridge: &'a mut ResponsesToolBridge,
    upstream_names: HashMap<String, ResponsesToolName>,
    bridged_definitions: HashMap<ResponsesToolName, Value>,
    converted: Vec<Value>,
    web_search_options: Option<Value>,
    web_search_tool_seen: bool,
}

// Tool calls always carry object arguments, but some OpenAI-compatible
// providers reject a root union when any branch also permits a scalar or null.
fn normalize_responses_tool_parameter_roots(body: &mut Value) -> bool {
    let Some(body) = body.as_object_mut() else {
        return false;
    };
    let mut changed = normalize_responses_tool_list(body.get_mut("tools"));
    match body.get_mut("input") {
        Some(Value::Array(items)) => {
            for item in items {
                changed |= normalize_responses_input_tool_list(item);
            }
        }
        Some(item) => changed |= normalize_responses_input_tool_list(item),
        None => {}
    }
    changed
}

fn normalize_responses_input_tool_list(item: &mut Value) -> bool {
    let Some(item) = item.as_object_mut() else {
        return false;
    };
    if !matches!(
        item.get("type").and_then(Value::as_str),
        Some("additional_tools" | "tool_search_output")
    ) {
        return false;
    }
    normalize_responses_tool_list(item.get_mut("tools"))
}

fn normalize_responses_tool_list(tools: Option<&mut Value>) -> bool {
    let Some(tools) = tools.and_then(Value::as_array_mut) else {
        return false;
    };
    let mut changed = false;
    for tool in tools {
        let Some(tool) = tool.as_object_mut() else {
            continue;
        };
        if let Some(parameters) = tool.get_mut("parameters") {
            changed |= normalize_tool_parameter_root(parameters);
        }
        if let Some(parameters) = tool
            .get_mut("function")
            .and_then(Value::as_object_mut)
            .and_then(|function| function.get_mut("parameters"))
        {
            changed |= normalize_tool_parameter_root(parameters);
        }
        for field in ["tools", "children"] {
            changed |= normalize_responses_tool_list(tool.get_mut(field));
        }
    }
    changed
}

fn normalize_tool_parameter_root(schema: &mut Value) -> bool {
    match restrict_tool_parameter_schema_to_object(schema) {
        Some(changed) => changed,
        None => {
            *schema = json!({"type":"object","properties":{}});
            true
        }
    }
}

fn restrict_tool_parameter_schema_to_object(schema: &mut Value) -> Option<bool> {
    match schema {
        Value::Bool(true) => {
            *schema = json!({"type":"object"});
            Some(true)
        }
        Value::Object(schema) => {
            let mut changed = match schema.get("type") {
                Some(Value::String(schema_type)) if schema_type == "object" => false,
                Some(Value::Array(schema_types))
                    if schema_types
                        .iter()
                        .any(|schema_type| schema_type.as_str() == Some("object")) =>
                {
                    schema.insert("type".to_string(), Value::String("object".to_string()));
                    true
                }
                None => {
                    schema.insert("type".to_string(), Value::String("object".to_string()));
                    true
                }
                _ => return None,
            };
            for keyword in ["anyOf", "oneOf"] {
                let mut remove_keyword = false;
                if let Some(branches) = schema.get_mut(keyword).and_then(Value::as_array_mut) {
                    branches.retain_mut(|branch| {
                        let Some(branch_changed) = restrict_tool_parameter_schema_to_object(branch)
                        else {
                            changed = true;
                            return false;
                        };
                        changed |= branch_changed;
                        true
                    });
                    remove_keyword = branches.is_empty();
                }
                if remove_keyword {
                    schema.remove(keyword);
                    changed = true;
                }
            }
            Some(changed)
        }
        _ => None,
    }
}

fn responses_tools_to_chat_tools_with_bridge(
    tools: &Value,
    tool_bridge: &mut ResponsesToolBridge,
) -> Result<ResponsesChatTools> {
    let tools = tools
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("tools 必须是数组"))?;
    let mut conversion = ResponsesChatToolsConversion {
        tool_bridge,
        upstream_names: HashMap::new(),
        bridged_definitions: HashMap::new(),
        converted: Vec::new(),
        web_search_options: None,
        web_search_tool_seen: false,
    };
    for tool in tools {
        append_responses_tool_to_chat_tools(tool, &[], &mut conversion)?;
    }
    Ok(ResponsesChatTools {
        tools: (!conversion.converted.is_empty()).then_some(Value::Array(conversion.converted)),
        web_search_options: conversion.web_search_options,
        web_search_tool_seen: conversion.web_search_tool_seen,
    })
}

fn append_responses_tool_to_chat_tools(
    tool: &Value,
    namespace_path: &[String],
    conversion: &mut ResponsesChatToolsConversion<'_>,
) -> Result<()> {
    let object = tool
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("tools 条目必须是对象"))?;
    let tool_type = object.get("type").and_then(Value::as_str);
    if matches!(tool_type, Some("function"))
        || (tool_type.is_none() && object.contains_key("function"))
    {
        let mut function = responses_function_tool_map(object)?;
        let name = response_function_name(&function, "function tool")?.to_string();
        let tool_name = ResponsesToolName {
            kind: ResponsesToolKind::Function,
            namespace: namespace_path.to_vec(),
            name: name.clone(),
        };
        let should_push = if namespace_path.is_empty() {
            register_plain_tool_name(
                &name,
                conversion.tool_bridge,
                &mut conversion.upstream_names,
            )?;
            true
        } else {
            validate_namespaced_function_name(&name)?;
            let original_definition = Value::Object(function.clone());
            match register_namespaced_tool_name(
                tool_name,
                &original_definition,
                conversion.tool_bridge,
                &mut conversion.upstream_names,
                &mut conversion.bridged_definitions,
            )? {
                Some(upstream_name) => {
                    function.insert("name".to_string(), Value::String(upstream_name));
                    true
                }
                None => false,
            }
        };
        if should_push {
            if namespace_path.is_empty() {
                function.insert("name".to_string(), Value::String(name));
            }
            conversion
                .converted
                .push(json!({"type":"function","function":Value::Object(function)}));
        }
        return Ok(());
    }
    if tool_type == Some("namespace") {
        return append_responses_namespace_tools(object, namespace_path, conversion);
    }
    if tool_type == Some("custom") {
        let name = response_function_name(object, "custom tool")?.to_string();
        let tool_name = ResponsesToolName::custom_in_namespace(namespace_path, &name);
        let original_definition = Value::Object(object.clone());
        let Some(upstream_name) = register_custom_tool_name(
            tool_name,
            &original_definition,
            conversion.tool_bridge,
            &mut conversion.upstream_names,
            &mut conversion.bridged_definitions,
        )?
        else {
            return Ok(());
        };
        conversion.converted.push(json!({
            "type":"function",
            "function":{
                "name":upstream_name,
                "description":custom_tool_bridge_description(object)?,
                "parameters":{
                    "type":"object",
                    "properties":{
                        "input":{
                            "type":"string",
                            "description":"Raw free-form input for the original Responses custom tool."
                        }
                    },
                    "required":["input"],
                    "additionalProperties":false
                }
            }
        }));
        return Ok(());
    }
    if tool_type == Some("tool_search") {
        if !namespace_path.is_empty() {
            anyhow::bail!("namespace.tools 不支持工具类型 tool_search");
        }
        match object.get("execution").and_then(Value::as_str) {
            Some("client") => {}
            Some("server") | None => anyhow::bail!(
                "Responses 托管工具 tool_search 不能转换为 Chat/Anthropic function 工具；仅 execution=client 可桥接"
            ),
            Some(execution) => {
                anyhow::bail!("tool_search.execution={execution} 不受支持；仅 client 可桥接")
            }
        }
        let description = object
            .get("description")
            .and_then(Value::as_str)
            .filter(|description| !description.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("execution=client tool_search.description 必须是非空字符串")
            })?;
        let parameters = object
            .get("parameters")
            .filter(|parameters| parameters.is_object())
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("execution=client tool_search.parameters 必须是 JSON 对象")
            })?;
        let tool_name = ResponsesToolName::tool_search();
        if let Some(existing) = conversion
            .upstream_names
            .get(TOOL_SEARCH_UPSTREAM_TOOL_NAME)
            && existing != &tool_name
        {
            anyhow::bail!(
                "客户端 tool_search 保留函数名 {TOOL_SEARCH_UPSTREAM_TOOL_NAME} 与 function 工具冲突"
            );
        }
        let original_definition = Value::Object(object.clone());
        if let Some(existing_definition) = conversion.bridged_definitions.get(&tool_name) {
            if existing_definition == &original_definition {
                return Ok(());
            }
            anyhow::bail!("客户端 tool_search 存在定义冲突");
        }
        conversion
            .bridged_definitions
            .insert(tool_name.clone(), original_definition);
        conversion.upstream_names.insert(
            TOOL_SEARCH_UPSTREAM_TOOL_NAME.to_string(),
            tool_name.clone(),
        );
        conversion.tool_bridge.response_to_upstream.insert(
            tool_name.clone(),
            TOOL_SEARCH_UPSTREAM_TOOL_NAME.to_string(),
        );
        conversion
            .tool_bridge
            .upstream_to_response
            .insert(TOOL_SEARCH_UPSTREAM_TOOL_NAME.to_string(), tool_name);
        conversion.converted.push(json!({
            "type":"function",
            "function":{
                "name":TOOL_SEARCH_UPSTREAM_TOOL_NAME,
                "description":description,
                "parameters":parameters,
            }
        }));
        return Ok(());
    }
    if matches!(tool_type, Some("web_search" | "web_search_preview")) {
        conversion.web_search_tool_seen = true;
        if !namespace_path.is_empty() {
            let tool_name = tool_type.unwrap_or("unknown");
            anyhow::bail!("namespace.tools 不支持工具类型 {tool_name}");
        }
        let options = responses_web_search_tool_to_chat_options(object)?;
        if let Some(existing) = conversion.web_search_options.as_ref() {
            if existing == &options {
                return Ok(());
            }
            anyhow::bail!("Responses web_search 工具存在定义冲突");
        }
        conversion.web_search_options = Some(options);
        return Ok(());
    }
    if !namespace_path.is_empty() {
        let tool_name = tool_type.unwrap_or("unknown");
        anyhow::bail!("namespace.tools 不支持工具类型 {tool_name}");
    }
    let tool_name = tool_type.unwrap_or("unknown");
    anyhow::bail!(
        "Responses 内置工具 {tool_name} 不能转换为 Chat Completions tools，请改用支持 Responses 的线路"
    )
}

fn responses_web_search_tool_to_chat_options(
    object: &serde_json::Map<String, Value>,
) -> Result<Value> {
    if let Some(filters) = object.get("filters")
        && !filters.is_null()
    {
        anyhow::bail!("Chat Completions web_search_options 不支持 Responses web_search.filters");
    }
    if let Some(return_token_budget) = object.get("return_token_budget")
        && !return_token_budget.is_null()
    {
        anyhow::bail!(
            "Chat Completions web_search_options 不支持 Responses web_search.return_token_budget"
        );
    }
    if object.get("external_web_access").and_then(Value::as_bool) == Some(false) {
        anyhow::bail!("Chat Completions web_search_options 不能表达 external_web_access=false");
    }

    let mut options = serde_json::Map::new();
    if let Some(search_context_size) = object.get("search_context_size") {
        let size = search_context_size
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("web_search.search_context_size 必须是字符串"))?;
        if !matches!(size, "low" | "medium" | "high") {
            anyhow::bail!("web_search.search_context_size 必须是 low/medium/high");
        }
        options.insert(
            "search_context_size".to_string(),
            Value::String(size.to_string()),
        );
    }
    if let Some(user_location) = object.get("user_location")
        && !user_location.is_null()
    {
        options.insert(
            "user_location".to_string(),
            responses_web_search_user_location_to_chat(user_location)?,
        );
    }
    Ok(Value::Object(options))
}

fn responses_web_search_user_location_to_chat(user_location: &Value) -> Result<Value> {
    let object = user_location
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("web_search.user_location 必须是对象"))?;
    let location_type = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("web_search.user_location 缺少 type"))?;
    if location_type != "approximate" {
        anyhow::bail!("web_search.user_location.type 只能是 approximate");
    }
    let approximate = if let Some(approximate) = object.get("approximate") {
        approximate
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("web_search.user_location.approximate 必须是对象"))?
            .clone()
    } else {
        let mut approximate = serde_json::Map::new();
        for key in ["city", "country", "region", "timezone"] {
            if let Some(value) = object.get(key) {
                if !value.is_string() && !value.is_null() {
                    anyhow::bail!("web_search.user_location.{key} 必须是字符串");
                }
                if !value.is_null() {
                    approximate.insert(key.to_string(), value.clone());
                }
            }
        }
        approximate
    };
    Ok(json!({
        "type":"approximate",
        "approximate":Value::Object(approximate),
    }))
}

fn append_responses_namespace_tools(
    object: &serde_json::Map<String, Value>,
    parent_namespace: &[String],
    conversion: &mut ResponsesChatToolsConversion<'_>,
) -> Result<()> {
    let namespace = responses_namespace_name(object)?;
    let mut namespace_path = parent_namespace.to_vec();
    namespace_path.push(namespace);
    if namespace_path.len() > MAX_RESPONSES_NAMESPACE_DEPTH {
        anyhow::bail!("namespace 嵌套层级超过 {MAX_RESPONSES_NAMESPACE_DEPTH}");
    }
    let tools = object.get("tools");
    let children = object.get("children");
    if tools.is_none() && children.is_none() {
        anyhow::bail!("namespace 工具缺少 tools 或 children");
    }
    if let Some(tools) = tools {
        let tools = tools
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("namespace.tools 必须是数组"))?;
        for tool in tools {
            append_responses_tool_to_chat_tools(tool, &namespace_path, conversion)?;
        }
    }
    if let Some(children) = children {
        let children = children
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("namespace.children 必须是数组"))?;
        for child in children {
            append_responses_tool_to_chat_tools(child, &namespace_path, conversion)?;
        }
    }
    Ok(())
}

fn responses_function_tool_map(
    object: &serde_json::Map<String, Value>,
) -> Result<serde_json::Map<String, Value>> {
    let mut function = if let Some(function) = object.get("function").and_then(Value::as_object) {
        function.clone()
    } else {
        object
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "type" | "defer_loading"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<serde_json::Map<_, _>>()
    };
    function.remove("defer_loading");
    let name = response_function_name(&function, "function tool")?.to_string();
    function.insert("name".to_string(), Value::String(name));
    Ok(function)
}

fn register_plain_tool_name(
    name: &str,
    tool_bridge: &mut ResponsesToolBridge,
    upstream_names: &mut HashMap<String, ResponsesToolName>,
) -> Result<()> {
    let tool_name = ResponsesToolName::plain(name);
    if let Some(existing) = upstream_names.get(name) {
        if existing != &tool_name {
            anyhow::bail!("桥接工具展开名称 {name} 与 function 工具冲突");
        }
    } else {
        upstream_names.insert(name.to_string(), tool_name.clone());
    }
    tool_bridge
        .upstream_to_response
        .entry(name.to_string())
        .or_insert_with(|| tool_name.clone());
    tool_bridge
        .response_to_upstream
        .entry(tool_name)
        .or_insert_with(|| name.to_string());
    Ok(())
}

fn register_namespaced_tool_name(
    tool_name: ResponsesToolName,
    original_definition: &Value,
    tool_bridge: &mut ResponsesToolBridge,
    upstream_names: &mut HashMap<String, ResponsesToolName>,
    namespace_definitions: &mut HashMap<ResponsesToolName, Value>,
) -> Result<Option<String>> {
    if let Some(existing_definition) = namespace_definitions.get(&tool_name) {
        if existing_definition == original_definition {
            return Ok(None);
        }
        anyhow::bail!(
            "namespace 工具 {}.{} 存在定义冲突",
            tool_name.namespace.join("."),
            tool_name.name
        );
    }
    let upstream_name = namespaced_upstream_tool_name(&tool_name.namespace, &tool_name.name);
    if let Some(existing) = upstream_names.get(&upstream_name)
        && existing != &tool_name
    {
        anyhow::bail!("namespace 工具展开名称 {upstream_name} 发生冲突");
    }
    namespace_definitions.insert(tool_name.clone(), original_definition.clone());
    upstream_names.insert(upstream_name.clone(), tool_name.clone());
    tool_bridge.has_namespace_tools = true;
    tool_bridge
        .response_to_upstream
        .insert(tool_name.clone(), upstream_name.clone());
    tool_bridge
        .upstream_to_response
        .insert(upstream_name.clone(), tool_name);
    Ok(Some(upstream_name))
}

fn register_custom_tool_name(
    tool_name: ResponsesToolName,
    original_definition: &Value,
    tool_bridge: &mut ResponsesToolBridge,
    upstream_names: &mut HashMap<String, ResponsesToolName>,
    bridged_definitions: &mut HashMap<ResponsesToolName, Value>,
) -> Result<Option<String>> {
    if let Some(existing_definition) = bridged_definitions.get(&tool_name) {
        if existing_definition == original_definition {
            return Ok(None);
        }
        anyhow::bail!(
            "custom 工具 {}{} 存在定义冲突",
            tool_name
                .namespace_string()
                .map(|namespace| format!("{namespace}."))
                .unwrap_or_default(),
            tool_name.name,
        );
    }
    let upstream_name = custom_upstream_tool_name(&tool_name.namespace, &tool_name.name);
    if let Some(existing) = upstream_names.get(&upstream_name)
        && existing != &tool_name
    {
        anyhow::bail!("custom 工具展开名称 {upstream_name} 发生冲突");
    }
    bridged_definitions.insert(tool_name.clone(), original_definition.clone());
    upstream_names.insert(upstream_name.clone(), tool_name.clone());
    tool_bridge.has_custom_tools = true;
    tool_bridge
        .response_to_upstream
        .insert(tool_name.clone(), upstream_name.clone());
    tool_bridge
        .upstream_to_response
        .insert(upstream_name.clone(), tool_name);
    Ok(Some(upstream_name))
}

fn custom_tool_bridge_description(object: &serde_json::Map<String, Value>) -> Result<String> {
    let mut description = object
        .get("description")
        .and_then(Value::as_str)
        .filter(|description| !description.is_empty())
        .map(|description| {
            format!(
                "{}\n\n",
                utf8_prefix(description, MAX_CUSTOM_TOOL_SOURCE_DESCRIPTION_BYTES)
            )
        })
        .unwrap_or_default();
    description.push_str(
        "[Codey compatibility bridge] This was an OpenAI Responses custom free-form tool. \
Call this function with exactly one `input` string containing the complete raw tool input. \
Do not JSON-encode the string again and do not add wrapper text.",
    );
    if let Some(format) = object.get("format") {
        description.push_str(" Format: ");
        description.push_str(
            &serde_json::to_string(format).context("序列化 Responses custom 工具格式提示失败")?,
        );
    }
    if description.len() > MAX_CUSTOM_TOOL_BRIDGE_DESCRIPTION_BYTES {
        let mut end = MAX_CUSTOM_TOOL_BRIDGE_DESCRIPTION_BYTES - '…'.len_utf8();
        while !description.is_char_boundary(end) {
            end -= 1;
        }
        description.truncate(end);
        description.push('…');
    }
    Ok(description)
}

fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn response_function_name<'a>(
    object: &'a serde_json::Map<String, Value>,
    context: &str,
) -> Result<&'a str> {
    object
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{context} 缺少 name"))
}

fn responses_namespace_name(object: &serde_json::Map<String, Value>) -> Result<String> {
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("namespace 工具缺少 name"))?;
    validate_namespace_segment(name, "namespace.name")
}

fn validate_namespace_segment(segment: &str, field: &str) -> Result<String> {
    if segment.is_empty()
        || segment.trim() != segment
        || segment.contains('.')
        || segment.chars().any(char::is_control)
    {
        anyhow::bail!("{field} 必须是非空、无控制字符且不包含点号的字符串");
    }
    Ok(segment.to_string())
}

fn validate_namespaced_function_name(name: &str) -> Result<()> {
    if name.is_empty() || name.trim() != name || name.chars().any(char::is_control) {
        anyhow::bail!("namespace function tool.name 必须是非空且无控制字符的字符串");
    }
    Ok(())
}

fn namespaced_upstream_tool_name(namespace: &[String], name: &str) -> String {
    let canonical = format!("{}\u{1e}{name}", namespace.join("\u{1f}"));
    let hash = stable_tool_hash_hex(&canonical);
    let stem_source = format!("{}__{name}", namespace.join("__"));
    let stem = sanitize_upstream_tool_stem(&stem_source);
    let suffix = format!("__{hash}");
    let max_stem_len = UPSTREAM_FUNCTION_NAME_MAX_BYTES
        .saturating_sub(NAMESPACE_UPSTREAM_TOOL_PREFIX.len())
        .saturating_sub(suffix.len());
    let stem = stem.chars().take(max_stem_len).collect::<String>();
    format!("{NAMESPACE_UPSTREAM_TOOL_PREFIX}{stem}{suffix}")
}

fn custom_upstream_tool_name(namespace: &[String], name: &str) -> String {
    let canonical = format!("custom\u{1e}{}\u{1e}{name}", namespace.join("\u{1f}"));
    let hash = stable_tool_hash_hex(&canonical);
    let stem_source = if namespace.is_empty() {
        name.to_string()
    } else {
        format!("{}__{name}", namespace.join("__"))
    };
    let stem = sanitize_upstream_tool_stem(&stem_source);
    let suffix = format!("__{hash}");
    let max_stem_len = UPSTREAM_FUNCTION_NAME_MAX_BYTES
        .saturating_sub(CUSTOM_UPSTREAM_TOOL_PREFIX.len())
        .saturating_sub(suffix.len());
    let stem = stem.chars().take(max_stem_len).collect::<String>();
    format!("{CUSTOM_UPSTREAM_TOOL_PREFIX}{stem}{suffix}")
}

fn stable_tool_hash_hex(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn sanitize_upstream_tool_stem(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    let mut last_was_separator = false;
    for ch in value.chars() {
        let next = if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
            ch
        } else {
            '_'
        };
        if next == '_' && last_was_separator {
            continue;
        }
        last_was_separator = next == '_';
        sanitized.push(next);
    }
    let sanitized = sanitized.trim_matches('_');
    if sanitized.is_empty() {
        "tool".to_string()
    } else {
        sanitized.to_string()
    }
}

fn looks_like_namespace_upstream_name(name: &str) -> bool {
    name.starts_with(NAMESPACE_UPSTREAM_TOOL_PREFIX)
}

fn looks_like_custom_upstream_name(name: &str) -> bool {
    name.starts_with(CUSTOM_UPSTREAM_TOOL_PREFIX)
}

fn could_be_namespace_upstream_name(name: &str) -> bool {
    !name.is_empty()
        && (NAMESPACE_UPSTREAM_TOOL_PREFIX.starts_with(name)
            || name.starts_with(NAMESPACE_UPSTREAM_TOOL_PREFIX))
}

fn could_be_custom_upstream_name(name: &str) -> bool {
    !name.is_empty()
        && (CUSTOM_UPSTREAM_TOOL_PREFIX.starts_with(name)
            || name.starts_with(CUSTOM_UPSTREAM_TOOL_PREFIX))
}

fn responses_namespace_path(value: Option<&Value>, field: &str) -> Result<Vec<String>> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(namespace)) => namespace
            .split('.')
            .map(|segment| validate_namespace_segment(segment, field))
            .collect(),
        Some(Value::Array(segments)) => segments
            .iter()
            .map(|segment| {
                segment
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("{field} 数组只能包含字符串"))
                    .and_then(|segment| validate_namespace_segment(segment, field))
            })
            .collect(),
        Some(_) => anyhow::bail!("{field} 必须是字符串或字符串数组"),
    }
}

fn merge_namespace_paths(
    outer: Vec<String>,
    inner: Vec<String>,
    field: &str,
) -> Result<Vec<String>> {
    if outer.is_empty() {
        return Ok(inner);
    }
    if inner.is_empty() || inner == outer {
        return Ok(outer);
    }
    anyhow::bail!("{field} 同时包含冲突的 namespace")
}

fn responses_tool_name_from_function_object(
    function: &serde_json::Map<String, Value>,
    outer_namespace: Option<&Value>,
    context: &str,
) -> Result<ResponsesToolName> {
    let name = response_function_name(function, context)?;
    let namespace = merge_namespace_paths(
        responses_namespace_path(outer_namespace, "namespace")?,
        responses_namespace_path(function.get("namespace"), "function.namespace")?,
        context,
    )?;
    Ok(ResponsesToolName {
        kind: ResponsesToolKind::Function,
        namespace,
        name: name.to_string(),
    })
}

fn responses_tool_name_from_call_object(
    object: &serde_json::Map<String, Value>,
    context: &str,
) -> Result<ResponsesToolName> {
    if let Some(function) = object.get("function").and_then(Value::as_object) {
        return responses_tool_name_from_function_object(
            function,
            object.get("namespace"),
            context,
        );
    }
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{context} 缺少 name"))?;
    Ok(ResponsesToolName {
        kind: ResponsesToolKind::Function,
        namespace: responses_namespace_path(object.get("namespace"), "namespace")?,
        name: name.to_string(),
    })
}

fn responses_tool_choice_to_chat_tool_choice(
    tool_choice: &Value,
    tool_bridge: &ResponsesToolBridge,
) -> Result<Value> {
    match tool_choice {
        Value::String(choice) if matches!(choice.as_str(), "auto" | "none" | "required") => {
            Ok(Value::String(choice.clone()))
        }
        Value::Object(object) => {
            let choice_type = object.get("type").and_then(Value::as_str);
            let tool_name = match choice_type {
                Some("function") => {
                    responses_tool_name_from_call_object(object, "function tool_choice")?
                }
                Some("custom") => {
                    let name = response_function_name(object, "custom tool_choice")?;
                    let namespace = responses_namespace_path(
                        object.get("namespace"),
                        "custom tool_choice.namespace",
                    )?;
                    ResponsesToolName::custom_in_namespace(&namespace, name)
                }
                Some("tool_search") => ResponsesToolName::tool_search(),
                _ => {
                    let tool_name = choice_type.unwrap_or("unknown");
                    anyhow::bail!(
                        "Responses tool_choice 类型 {tool_name} 不能转换为 Chat Completions tool_choice"
                    );
                }
            };
            let upstream_name = tool_bridge.upstream_name_for_call(&tool_name)?;
            Ok(json!({"type":"function","function":{"name":upstream_name}}))
        }
        _ => anyhow::bail!("tool_choice 必须是 auto/none/required 或 function 对象"),
    }
}

fn responses_function_call_choice_to_chat(
    function_call: &Value,
    tool_bridge: &ResponsesToolBridge,
) -> Result<Value> {
    match function_call {
        Value::String(choice) if matches!(choice.as_str(), "auto" | "none") => {
            Ok(Value::String(choice.clone()))
        }
        Value::Object(object) => {
            let tool_name = responses_tool_name_from_call_object(object, "function_call")?;
            let upstream_name = tool_bridge.upstream_name_for_call(&tool_name)?;
            Ok(json!({"name":upstream_name}))
        }
        _ => anyhow::bail!("function_call 必须是 auto/none 或 function name 对象"),
    }
}

fn json_value_as_chat_string(value: Option<&Value>) -> Option<String> {
    value.map(|value| match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    })
}

fn wrap_custom_tool_input(input: &str) -> Result<String> {
    serde_json::to_string(&json!({"input":input})).context("序列化 custom 工具 input 包装失败")
}

fn custom_tool_input_from_value(value: &Value, context: &str) -> Result<String> {
    value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{context} 必须是 JSON 对象"))?
        .get("input")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("{context}.input 必须是字符串"))
}

fn custom_tool_input_from_arguments(arguments: &str, context: &str) -> Result<String> {
    let value = serde_json::from_str::<Value>(arguments)
        .with_context(|| format!("{context} 不是有效 JSON"))?;
    custom_tool_input_from_value(&value, context)
}

fn responses_tool_call_item_from_upstream_arguments(
    tool_name: &ResponsesToolName,
    call_id: String,
    arguments: String,
    status: &str,
    context: &str,
) -> Result<Value> {
    let payload = if tool_name.is_custom() {
        Value::String(custom_tool_input_from_arguments(&arguments, context)?)
    } else if tool_name.is_tool_search() {
        let arguments = serde_json::from_str::<Value>(&arguments)
            .with_context(|| format!("{context} 不是有效 JSON"))?;
        if !arguments.is_object() {
            anyhow::bail!("{context} 必须是 JSON 对象");
        }
        arguments
    } else {
        Value::String(arguments)
    };
    Ok(responses_tool_call_item_from_payload(
        tool_name, call_id, payload, status,
    ))
}

fn responses_tool_call_item_from_payload(
    tool_name: &ResponsesToolName,
    call_id: String,
    payload: Value,
    status: &str,
) -> Value {
    let item_id = responses_tool_call_item_id(tool_name);
    responses_tool_call_item_with_id(tool_name, item_id, call_id, payload, status)
}

fn responses_tool_call_item_id(tool_name: &ResponsesToolName) -> String {
    let prefix = if tool_name.is_custom() {
        "ctc_codey_"
    } else if tool_name.is_tool_search() {
        "tsc_codey_"
    } else {
        "fc_codey_"
    };
    format!("{prefix}{}", Uuid::new_v4())
}

fn responses_tool_call_item_with_id(
    tool_name: &ResponsesToolName,
    item_id: String,
    call_id: String,
    payload: Value,
    status: &str,
) -> Value {
    let (item_type, payload_field) = match tool_name.kind {
        ResponsesToolKind::Function => ("function_call", "arguments"),
        ResponsesToolKind::Custom => ("custom_tool_call", "input"),
        ResponsesToolKind::ToolSearch => ("tool_search_call", "arguments"),
    };
    let mut item = serde_json::Map::from_iter([
        ("id".to_string(), Value::String(item_id)),
        ("type".to_string(), Value::String(item_type.to_string())),
        ("status".to_string(), Value::String(status.to_string())),
        ("call_id".to_string(), Value::String(call_id)),
        (payload_field.to_string(), payload),
    ]);
    tool_name.insert_response_fields(&mut item);
    Value::Object(item)
}

fn push_chat_text_message(messages: &mut Vec<Value>, role: &str, content: &str) -> Result<()> {
    if content.is_empty() {
        return Ok(());
    }
    messages.push(json!({"role":role,"content":content}));
    Ok(())
}

fn copy_number_or_string_field(
    from: &serde_json::Map<String, Value>,
    to: &mut serde_json::Map<String, Value>,
    source: &str,
    target: &str,
) {
    if let Some(value) = from.get(source)
        && (value.is_number() || value.is_string() || value.is_boolean())
    {
        to.insert(target.to_string(), value.clone());
    }
}

fn copy_json_field(
    from: &serde_json::Map<String, Value>,
    to: &mut serde_json::Map<String, Value>,
    source: &str,
    target: &str,
) {
    if let Some(value) = from.get(source) {
        to.insert(target.to_string(), value.clone());
    }
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    _body_budget_permit: Option<OwnedSemaphorePermit>,
}

#[derive(Debug)]
struct PendingHttpRequest {
    request: HttpRequest,
    content_length: usize,
}

#[cfg(test)]
async fn read_http_request<R>(stream: &mut R) -> Result<HttpRequest>
where
    R: tokio::io::AsyncRead + Unpin,
{
    read_http_request_with_budget(stream, None).await
}

#[derive(Debug)]
struct RequestBodyBudgetUnavailable;

impl std::fmt::Display for RequestBodyBudgetUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Codey 本地路由请求缓冲预算不足")
    }
}

impl std::error::Error for RequestBodyBudgetUnavailable {}

#[derive(Debug)]
struct UnsupportedRequestContentEncoding;

impl std::fmt::Display for UnsupportedRequestContentEncoding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Responses 请求体仅支持 identity 或 zstd Content-Encoding")
    }
}

impl std::error::Error for UnsupportedRequestContentEncoding {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResponsesRequestBodyEncoding {
    Identity,
    Zstd,
}

#[cfg(test)]
async fn read_http_request_with_budget<R>(
    stream: &mut R,
    body_budget: Option<&Arc<Semaphore>>,
) -> Result<HttpRequest>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let pending = read_http_request_head(stream).await?;
    read_http_request_body_with_budget(stream, pending, body_budget).await
}

async fn read_http_request_head<R>(stream: &mut R) -> Result<PendingHttpRequest>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let read = stream
            .read(&mut chunk)
            .await
            .context("读取 Codey 本地路由请求失败")?;
        if read == 0 {
            anyhow::bail!("请求在 HTTP 头读取完成前断开");
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = find_header_end(&buffer) {
            if index > MAX_HEADER_BYTES {
                anyhow::bail!("HTTP 请求头超过 Codey 本地路由安全上限");
            }
            break index;
        }
        // Keep at most the three bytes that may be the beginning of the
        // terminating CRLFCRLF sequence beyond the header byte limit. A read
        // may also contain body bytes, which must not count as header bytes.
        if buffer.len() > MAX_HEADER_BYTES.saturating_add(3) {
            anyhow::bail!("HTTP 请求头超过 Codey 本地路由安全上限");
        }
    };
    let header_text = std::str::from_utf8(&buffer[..header_end]).context("HTTP 头不是 UTF-8")?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("HTTP 请求缺少方法"))?
        .to_string();
    let raw_path = request_parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("HTTP 请求缺少路径"))?;
    let path = raw_path.split('?').next().unwrap_or(raw_path).to_string();
    let mut headers = Vec::new();
    let mut content_length = 0_usize;
    let mut saw_content_length = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_string();
        let value = value.trim().to_string();
        if name.eq_ignore_ascii_case("content-length") {
            let parsed = value.parse::<usize>().context("HTTP Content-Length 无效")?;
            if saw_content_length && parsed != content_length {
                anyhow::bail!("HTTP 请求包含冲突的 Content-Length");
            }
            saw_content_length = true;
            content_length = parsed;
        }
        if name.eq_ignore_ascii_case("transfer-encoding") && !value.eq_ignore_ascii_case("identity")
        {
            anyhow::bail!("Codey 本地路由不接受分块请求体");
        }
        headers.push((name, value));
    }
    if content_length > MAX_REQUEST_BYTES {
        anyhow::bail!("请求体超过 Codey 本地路由安全上限");
    }
    let body_start = header_end + 4;
    let buffered_body = buffer.get(body_start..).unwrap_or_default();
    let mut body = Vec::with_capacity(buffered_body.len().min(content_length));
    body.extend_from_slice(&buffered_body[..buffered_body.len().min(content_length)]);
    Ok(PendingHttpRequest {
        request: HttpRequest {
            method,
            path,
            headers,
            body,
            _body_budget_permit: None,
        },
        content_length,
    })
}

async fn read_http_request_body_with_budget<R>(
    stream: &mut R,
    mut pending: PendingHttpRequest,
    body_budget: Option<&Arc<Semaphore>>,
) -> Result<HttpRequest>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let body_budget_permit = body_budget
        .map(|body_budget| acquire_request_body_budget(body_budget, pending.content_length))
        .transpose()?
        .flatten();
    pending.request._body_budget_permit = body_budget_permit;
    let mut chunk = [0_u8; 8192];
    while pending.request.body.len() < pending.content_length {
        let remaining = pending.content_length - pending.request.body.len();
        let read_length = remaining.min(chunk.len());
        let read = stream
            .read(&mut chunk[..read_length])
            .await
            .context("读取 Codey 本地路由请求体失败")?;
        if read == 0 {
            anyhow::bail!("请求体读取完成前连接断开");
        }
        pending.request.body.extend_from_slice(&chunk[..read]);
    }
    pending.request.body.truncate(pending.content_length);
    Ok(pending.request)
}

fn responses_request_body_encoding(request: &HttpRequest) -> Result<ResponsesRequestBodyEncoding> {
    let mut encoding = None;
    for (name, value) in &request.headers {
        if !name.eq_ignore_ascii_case(CONTENT_ENCODING.as_str()) {
            continue;
        }
        for token in value.split(',') {
            let token = token.trim();
            if token.is_empty() || encoding.replace(token).is_some() {
                return Err(anyhow::Error::new(UnsupportedRequestContentEncoding));
            }
        }
    }
    match encoding {
        None => Ok(ResponsesRequestBodyEncoding::Identity),
        Some(encoding) if encoding.eq_ignore_ascii_case("identity") => {
            Ok(ResponsesRequestBodyEncoding::Identity)
        }
        Some(encoding) if encoding.eq_ignore_ascii_case("zstd") => {
            Ok(ResponsesRequestBodyEncoding::Zstd)
        }
        Some(_) => Err(anyhow::Error::new(UnsupportedRequestContentEncoding)),
    }
}

async fn decode_responses_request_body(
    request: &mut HttpRequest,
    body_budget: &Arc<Semaphore>,
) -> Result<Vec<u8>> {
    let encoding = responses_request_body_encoding(request)?;
    request
        .headers
        .retain(|(name, _)| !name.eq_ignore_ascii_case(CONTENT_ENCODING.as_str()));
    let encoded = std::mem::take(&mut request.body);
    if encoding == ResponsesRequestBodyEncoding::Identity {
        return Ok(encoded);
    }

    reserve_request_body_budget_for_decompression(request, body_budget)?;
    let decoded = tokio::task::spawn_blocking(move || decode_zstd_request_body(encoded))
        .await
        .context("等待 Responses zstd 请求体解压任务失败")??;
    shrink_request_body_budget(request, decoded.len())?;
    Ok(decoded)
}

async fn parse_responses_request_body(
    encoded: Vec<u8>,
) -> Result<(Vec<u8>, serde_json::Result<Value>)> {
    if encoded.len() < REQUEST_JSON_OFFLOAD_BYTES {
        let parsed = serde_json::from_slice(&encoded);
        return Ok((encoded, parsed));
    }
    tokio::task::spawn_blocking(move || {
        let parsed = serde_json::from_slice(&encoded);
        (encoded, parsed)
    })
    .await
    .context("等待大型 Responses JSON 解析任务失败")
}

fn decode_zstd_request_body(encoded: Vec<u8>) -> Result<Vec<u8>> {
    let mut decoder = zstd::stream::read::Decoder::new(Cursor::new(encoded))
        .context("初始化 Responses zstd 请求体解码器失败")?;
    decoder
        .window_log_max(25)
        .context("限制 Responses zstd 请求体解压窗口失败")?;
    let mut decoded = Vec::new();
    decoder
        .take((MAX_REQUEST_BYTES as u64).saturating_add(1))
        .read_to_end(&mut decoded)
        .context("解压 Responses zstd 请求体失败")?;
    if decoded.len() > MAX_REQUEST_BYTES {
        anyhow::bail!("解压后的 Responses 请求体超过 Codey 本地路由安全上限");
    }
    Ok(decoded)
}

fn request_body_budget_permit_count(wire_bytes: usize) -> Result<usize> {
    if wire_bytes > MAX_REQUEST_BYTES {
        anyhow::bail!("请求体超过 Codey 本地路由安全上限");
    }
    let estimated_memory = wire_bytes.saturating_mul(REQUEST_MEMORY_BUDGET_MULTIPLIER);
    Ok(estimated_memory.div_ceil(REQUEST_BODY_BUDGET_UNIT_BYTES))
}

fn reserve_request_body_budget_for_decompression(
    request: &mut HttpRequest,
    body_budget: &Arc<Semaphore>,
) -> Result<()> {
    let required = request_body_budget_permit_count(MAX_REQUEST_BYTES)?;
    let held = request
        ._body_budget_permit
        .as_ref()
        .map(OwnedSemaphorePermit::num_permits)
        .unwrap_or(0);
    let additional = required.saturating_sub(held);
    if additional == 0 {
        return Ok(());
    }
    let additional = u32::try_from(additional).context("请求体解压预算超出内部上限")?;
    let permit = Arc::clone(body_budget)
        .try_acquire_many_owned(additional)
        .map_err(|_| anyhow::Error::new(RequestBodyBudgetUnavailable))?;
    if let Some(held) = request._body_budget_permit.as_mut() {
        held.merge(permit);
    } else {
        request._body_budget_permit = Some(permit);
    }
    Ok(())
}

fn shrink_request_body_budget(request: &mut HttpRequest, decoded_bytes: usize) -> Result<()> {
    let desired = request_body_budget_permit_count(decoded_bytes)?;
    let Some(mut held) = request._body_budget_permit.take() else {
        return Ok(());
    };
    if desired == 0 {
        return Ok(());
    }
    if desired > held.num_permits() {
        anyhow::bail!("Responses 请求体解压预算不足");
    }
    if desired == held.num_permits() {
        request._body_budget_permit = Some(held);
        return Ok(());
    }
    let retained = held
        .split(desired)
        .context("缩减 Responses 请求体解压预算失败")?;
    drop(held);
    request._body_budget_permit = Some(retained);
    Ok(())
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn acquire_request_body_budget(
    body_budget: &Arc<Semaphore>,
    wire_bytes: usize,
) -> Result<Option<OwnedSemaphorePermit>> {
    let permits = request_body_budget_permit_count(wire_bytes)?;
    if permits == 0 {
        return Ok(None);
    }
    let permits = u32::try_from(permits).context("请求体缓冲预算超出内部上限")?;
    Arc::clone(body_budget)
        .try_acquire_many_owned(permits)
        .map(Some)
        .map_err(|_| anyhow::Error::new(RequestBodyBudgetUnavailable))
}

#[derive(Debug)]
struct WebSocketRequestContext {
    headers: Vec<(String, String)>,
}

async fn request_looks_like_responses_websocket(stream: &TcpStream) -> Result<bool> {
    tokio::time::timeout(REQUEST_READ_TIMEOUT, async {
        let mut peek = vec![0_u8; 4096];
        loop {
            let read = stream
                .peek(&mut peek)
                .await
                .context("探测 Codey Responses WebSocket 请求失败")?;
            if read == 0 {
                return Ok(false);
            }
            let bytes = &peek[..read];
            let Some(request_line_end) = bytes.windows(2).position(|window| window == b"\r\n")
            else {
                tokio::time::sleep(Duration::from_millis(1)).await;
                continue;
            };
            let request_line = std::str::from_utf8(&bytes[..request_line_end])
                .context("WebSocket HTTP 请求行不是 UTF-8")?;
            let mut parts = request_line.split_whitespace();
            let method = parts.next().unwrap_or_default();
            let raw_path = parts.next().unwrap_or_default();
            let path = raw_path.split('?').next().unwrap_or(raw_path);
            if method != "GET" || !RESPONSES_WEBSOCKET_PATHS.contains(&path) {
                return Ok(false);
            }
            if let Some(header_end) = find_header_end(bytes) {
                let headers = String::from_utf8_lossy(&bytes[request_line_end + 2..header_end]);
                let mut connection_upgrade = false;
                let mut websocket_upgrade = false;
                for line in headers.split("\r\n") {
                    let Some((name, value)) = line.split_once(':') else {
                        continue;
                    };
                    if name.trim().eq_ignore_ascii_case("connection") {
                        connection_upgrade = value
                            .split([',', ' '])
                            .any(|part| part.trim().eq_ignore_ascii_case("upgrade"));
                    } else if name.trim().eq_ignore_ascii_case("upgrade") {
                        websocket_upgrade = value.trim().eq_ignore_ascii_case("websocket");
                    }
                }
                return Ok(connection_upgrade && websocket_upgrade);
            }
            if read == peek.len() {
                if peek.len() >= MAX_HEADER_BYTES.saturating_add(4) {
                    return Ok(false);
                }
                peek.resize(
                    peek.len()
                        .saturating_mul(2)
                        .min(MAX_HEADER_BYTES.saturating_add(4)),
                    0,
                );
            } else {
                // `peek` leaves the current bytes readable, so wait briefly
                // for another packet instead of spinning on the same prefix.
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
    })
    .await
    .context("探测 Codey Responses WebSocket 请求超时")?
}

fn websocket_request_authorized(
    request: &WebSocketRequest,
    token: &str,
    bearer_token: &str,
) -> bool {
    request.headers().iter().any(|(name, value)| {
        let Ok(value) = value.to_str() else {
            return false;
        };
        (name.as_str().eq_ignore_ascii_case(ROUTER_AUTH_HEADER)
            && constant_time_eq(value.trim().as_bytes(), token.as_bytes()))
            || (name.as_str().eq_ignore_ascii_case("authorization")
                && constant_time_eq(value.trim().as_bytes(), bearer_token.as_bytes()))
    })
}

fn websocket_forward_headers(request: &WebSocketRequest) -> Vec<(String, String)> {
    request
        .headers()
        .iter()
        .filter(|(name, _)| {
            let name = name.as_str();
            !is_hop_by_hop_header(name) && !name.to_ascii_lowercase().starts_with("sec-websocket-")
        })
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect()
}

fn websocket_handshake_error(status: WebSocketStatusCode, message: &str) -> WebSocketErrorResponse {
    WebSocketResponse::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .header("content-length", message.len())
        .body(Some(message.to_string()))
        .expect("static WebSocket handshake error response must be valid")
}

fn responses_websocket_stream_id(body: &Value) -> Result<Option<String>> {
    let Some(stream_id) = body.get("stream_id") else {
        return Ok(None);
    };
    let stream_id = stream_id.as_str().context("stream_id 必须是字符串")?;
    if stream_id.is_empty()
        || stream_id.len() > 256
        || !stream_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        anyhow::bail!("必须为 1–256 个字母、数字、下划线、连字符或句点");
    }
    Ok(Some(stream_id.to_string()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpstreamWebSocketAttempt {
    UseHttp,
    Completed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpstreamWebSocketMaintenanceAction {
    None,
    SendPing,
    Drop,
}

#[derive(Clone, Copy, Debug)]
struct UpstreamWebSocketLiveness {
    connected_at: Instant,
    last_activity_at: Instant,
    heartbeat_sent_at: Option<Instant>,
}

impl UpstreamWebSocketLiveness {
    fn new(now: Instant) -> Self {
        Self {
            connected_at: now,
            last_activity_at: now,
            heartbeat_sent_at: None,
        }
    }

    fn record_activity(&mut self, now: Instant) {
        self.last_activity_at = now;
    }

    fn record_pong(&mut self, now: Instant) {
        self.last_activity_at = now;
        self.heartbeat_sent_at = None;
    }

    fn record_heartbeat_sent(&mut self, now: Instant) {
        self.heartbeat_sent_at = Some(now);
    }

    fn maintenance_deadline(&self) -> Instant {
        let liveness_deadline = self
            .heartbeat_sent_at
            .map(|sent_at| sent_at + UPSTREAM_WEBSOCKET_PONG_TIMEOUT)
            .unwrap_or(self.last_activity_at + UPSTREAM_WEBSOCKET_HEARTBEAT_INTERVAL);
        std::cmp::min(
            self.connected_at + UPSTREAM_WEBSOCKET_MAX_REUSE_AGE,
            liveness_deadline,
        )
    }

    fn maintenance_action(&self, now: Instant) -> UpstreamWebSocketMaintenanceAction {
        if now >= self.connected_at + UPSTREAM_WEBSOCKET_MAX_REUSE_AGE {
            return UpstreamWebSocketMaintenanceAction::Drop;
        }
        if let Some(sent_at) = self.heartbeat_sent_at {
            return if now >= sent_at + UPSTREAM_WEBSOCKET_PONG_TIMEOUT {
                UpstreamWebSocketMaintenanceAction::Drop
            } else {
                UpstreamWebSocketMaintenanceAction::None
            };
        }
        if now >= self.last_activity_at + UPSTREAM_WEBSOCKET_HEARTBEAT_INTERVAL {
            UpstreamWebSocketMaintenanceAction::SendPing
        } else {
            UpstreamWebSocketMaintenanceAction::None
        }
    }
}

struct CachedUpstreamWebSocket {
    route_id: String,
    url: String,
    auth_identity: UpstreamWebSocketAuthIdentity,
    response_ids: HashSet<String>,
    liveness: UpstreamWebSocketLiveness,
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct UpstreamWebSocketAuthIdentity {
    authorization: Option<[u8; 32]>,
    account_id: Option<[u8; 32]>,
}

impl UpstreamWebSocketAuthIdentity {
    fn from_headers(headers: &HeaderMap) -> Self {
        Self {
            authorization: websocket_header_fingerprint(headers, AUTHORIZATION.as_str()),
            account_id: websocket_header_fingerprint(headers, CHATGPT_ACCOUNT_ID_HEADER),
        }
    }
}

fn websocket_header_fingerprint(headers: &HeaderMap, name: &str) -> Option<[u8; 32]> {
    headers
        .get(name)
        .map(|value| Sha256::digest(value.as_bytes()).into())
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct UpstreamWebSocketBackoffKey {
    route_id: String,
    url: String,
    auth_identity: UpstreamWebSocketAuthIdentity,
}

impl UpstreamWebSocketBackoffKey {
    fn new(route_id: &str, url: &str, auth_identity: UpstreamWebSocketAuthIdentity) -> Self {
        Self {
            route_id: route_id.to_string(),
            url: url.to_string(),
            auth_identity,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct UpstreamWebSocketBackoff {
    failure_count: u32,
    until: Instant,
    permanent: bool,
    generation: u64,
}

#[derive(Debug, Default)]
struct UpstreamWebSocketBackoffs {
    entries: HashMap<UpstreamWebSocketBackoffKey, UpstreamWebSocketBackoff>,
    order: VecDeque<(UpstreamWebSocketBackoffKey, u64)>,
    next_generation: u64,
}

impl UpstreamWebSocketBackoffs {
    fn is_backing_off(&self, key: &UpstreamWebSocketBackoffKey, now: Instant) -> bool {
        self.entries
            .get(key)
            .is_some_and(|backoff| backoff.permanent || backoff.until > now)
    }

    fn record_failure(
        &mut self,
        key: UpstreamWebSocketBackoffKey,
        now: Instant,
    ) -> (u32, Duration) {
        let reset_after = *UPSTREAM_WEBSOCKET_BACKOFF_STEPS
            .last()
            .expect("WebSocket backoff steps must not be empty");
        let failure_count = self
            .entries
            .get(&key)
            .filter(|backoff| !backoff.permanent && now <= backoff.until + reset_after)
            .map(|backoff| backoff.failure_count.saturating_add(1))
            .unwrap_or(1);
        let duration = upstream_websocket_backoff_duration(failure_count);
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        self.entries.insert(
            key.clone(),
            UpstreamWebSocketBackoff {
                failure_count,
                until: now + duration,
                permanent: false,
                generation,
            },
        );
        self.order.push_back((key, generation));
        self.enforce_limit();
        (failure_count, duration)
    }

    fn record_unsupported(&mut self, key: UpstreamWebSocketBackoffKey, now: Instant) {
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        self.entries.insert(
            key.clone(),
            UpstreamWebSocketBackoff {
                failure_count: 0,
                until: now,
                permanent: true,
                generation,
            },
        );
        self.order.push_back((key, generation));
        self.enforce_limit();
    }

    fn record_success(&mut self, key: &UpstreamWebSocketBackoffKey) {
        self.entries.remove(key);
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    fn enforce_limit(&mut self) {
        while self.entries.len() > MAX_UPSTREAM_WEBSOCKET_BACKOFFS {
            let Some((key, generation)) = self.order.pop_front() else {
                break;
            };
            if self
                .entries
                .get(&key)
                .is_some_and(|entry| entry.generation == generation)
            {
                self.entries.remove(&key);
            }
        }
        if self.order.len() > MAX_UPSTREAM_WEBSOCKET_BACKOFFS * 4 {
            self.order.retain(|(key, generation)| {
                self.entries
                    .get(key)
                    .is_some_and(|entry| entry.generation == *generation)
            });
        }
    }
}

fn upstream_websocket_backoff_duration(failure_count: u32) -> Duration {
    let index = failure_count.saturating_sub(1) as usize;
    UPSTREAM_WEBSOCKET_BACKOFF_STEPS[index.min(UPSTREAM_WEBSOCKET_BACKOFF_STEPS.len() - 1)]
}

fn record_upstream_websocket_failure(
    backoffs: &Arc<Mutex<UpstreamWebSocketBackoffs>>,
    key: &UpstreamWebSocketBackoffKey,
) {
    backoffs
        .lock()
        .expect("upstream WebSocket backoff mutex poisoned")
        .record_failure(key.clone(), Instant::now());
}

#[async_trait]
trait ResponsesDownstream: Send {
    async fn wait_for_upstream<T, F>(&mut self, future: F) -> Result<T>
    where
        T: Send,
        F: std::future::Future<Output = T> + Send,
    {
        Ok(future.await)
    }
    fn is_websocket(&self) -> bool {
        false
    }

    fn request_log_probe(&self) -> Option<&RouteRequestLogProbe> {
        None
    }

    fn prepare_adapted_response_context(&mut self, _body: &mut Value) -> bool {
        false
    }

    fn remember_adapted_response(&mut self, _response_id: &str, _output: &[Value]) {}

    async fn write_error(
        &mut self,
        status: u16,
        code: &str,
        message: String,
        route: Option<&RouteTarget>,
    ) -> Result<()>;

    async fn write_text_error(&mut self, status: u16, code: &str, message: String) -> Result<()> {
        self.write_error(status, code, message, None).await
    }

    async fn write_json(&mut self, status: u16, value: &Value) -> Result<()>;
    async fn start_event_stream(&mut self) -> Result<()>;
    async fn write_event(&mut self, event: &Value) -> Result<()>;
    async fn finish_event_stream(&mut self) -> Result<()>;
    async fn proxy_response(&mut self, response: reqwest::Response) -> Result<()>;

    async fn proxy_response_with_probe(
        &mut self,
        response: reqwest::Response,
        _probe: Option<&RouteRequestLogProbe>,
    ) -> Result<()> {
        self.proxy_response(response).await
    }

    async fn try_proxy_upstream_websocket(
        &mut self,
        _route: &RouteTarget,
        _headers: &HeaderMap,
        _body: &mut Value,
    ) -> Result<UpstreamWebSocketAttempt> {
        Ok(UpstreamWebSocketAttempt::UseHttp)
    }

    async fn try_proxy_upstream_websocket_with_probe(
        &mut self,
        route: &RouteTarget,
        headers: &HeaderMap,
        body: &mut Value,
        _probe: Option<&RouteRequestLogProbe>,
    ) -> Result<UpstreamWebSocketAttempt> {
        self.try_proxy_upstream_websocket(route, headers, body)
            .await
    }
}

// HTTP FIN cannot distinguish a legal write-half shutdown from cancellation.
// HTTP also avoids an extra async-trait allocation on every response chunk.
async fn await_upstream<D, T, F>(downstream: &mut D, future: F) -> Result<T>
where
    D: ResponsesDownstream + ?Sized,
    T: Send,
    F: std::future::Future<Output = T> + Send,
{
    if downstream.is_websocket() {
        downstream.wait_for_upstream(future).await
    } else {
        Ok(future.await)
    }
}

#[derive(Debug)]
struct DownstreamClosed;

impl std::fmt::Display for DownstreamClosed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("下游 WebSocket 已关闭")
    }
}

impl std::error::Error for DownstreamClosed {}

struct ObservedResponsesDownstream<'a, D>
where
    D: ResponsesDownstream + ?Sized,
{
    inner: &'a mut D,
    probe: Option<RouteRequestLogProbe>,
}

impl<'a, D> ObservedResponsesDownstream<'a, D>
where
    D: ResponsesDownstream + ?Sized,
{
    fn new(inner: &'a mut D, probe: Option<RouteRequestLogProbe>) -> Self {
        Self { inner, probe }
    }

    fn finish_result(&self, result: &Result<()>, operation: &str) {
        let Some(probe) = self.probe.as_ref() else {
            return;
        };
        if result.is_ok() {
            probe.finish_success();
        } else {
            probe.mark_cancelled(operation);
            probe.finish_cancelled();
        }
    }
}

#[async_trait]
impl<D> ResponsesDownstream for ObservedResponsesDownstream<'_, D>
where
    D: ResponsesDownstream + ?Sized,
{
    async fn wait_for_upstream<T, F>(&mut self, future: F) -> Result<T>
    where
        T: Send,
        F: std::future::Future<Output = T> + Send,
    {
        let result = self.inner.wait_for_upstream(future).await;
        if let Err(error) = &result
            && error.is::<DownstreamClosed>()
            && let Some(probe) = &self.probe
        {
            probe.mark_cancelled("downstream_websocket_closed");
            probe.finish_cancelled();
        }
        result
    }
    fn is_websocket(&self) -> bool {
        self.inner.is_websocket()
    }

    fn request_log_probe(&self) -> Option<&RouteRequestLogProbe> {
        self.probe.as_ref()
    }

    fn prepare_adapted_response_context(&mut self, body: &mut Value) -> bool {
        self.inner.prepare_adapted_response_context(body)
    }

    fn remember_adapted_response(&mut self, response_id: &str, output: &[Value]) {
        self.inner.remember_adapted_response(response_id, output);
    }

    async fn write_error(
        &mut self,
        status: u16,
        code: &str,
        message: String,
        route: Option<&RouteTarget>,
    ) -> Result<()> {
        if let Some(probe) = self.probe.as_ref() {
            probe.mark_error(status, code);
        }
        let result = self.inner.write_error(status, code, message, route).await;
        self.finish_result(&result, "downstream_error_write_failed");
        result
    }

    async fn write_text_error(&mut self, status: u16, code: &str, message: String) -> Result<()> {
        if let Some(probe) = self.probe.as_ref() {
            probe.mark_error(status, code);
        }
        let result = self.inner.write_text_error(status, code, message).await;
        self.finish_result(&result, "downstream_error_write_failed");
        result
    }

    async fn write_json(&mut self, status: u16, value: &Value) -> Result<()> {
        if let Some(probe) = self.probe.as_ref() {
            probe.observe_response(status, value);
        }
        let result = self.inner.write_json(status, value).await;
        if result.is_ok()
            && (200..300).contains(&status)
            && let Some(probe) = self.probe.as_ref()
        {
            probe.mark_first_downstream_content();
        }
        self.finish_result(&result, "downstream_json_write_failed");
        result
    }

    async fn start_event_stream(&mut self) -> Result<()> {
        let result = self.inner.start_event_stream().await;
        if let Some(probe) = self.probe.as_ref() {
            if result.is_ok() {
                probe.mark_response_started(200);
            } else {
                probe.mark_cancelled("downstream_stream_header_write_failed");
                probe.finish_cancelled();
            }
        }
        result
    }

    async fn write_event(&mut self, event: &Value) -> Result<()> {
        if let Some(probe) = self.probe.as_ref() {
            probe.observe_event(event);
        }
        let result = self.inner.write_event(event).await;
        if let Some(probe) = self.probe.as_ref() {
            if result.is_ok() {
                probe.mark_response_started(200);
                if responses_event_has_user_content(event) {
                    probe.mark_first_downstream_content();
                }
            } else {
                probe.mark_cancelled("downstream_event_write_failed");
                probe.finish_cancelled();
            }
        }
        result
    }

    async fn finish_event_stream(&mut self) -> Result<()> {
        let result = self.inner.finish_event_stream().await;
        self.finish_result(&result, "downstream_stream_finish_failed");
        result
    }

    async fn proxy_response(&mut self, response: reqwest::Response) -> Result<()> {
        let result = self
            .inner
            .proxy_response_with_probe(response, self.probe.as_ref())
            .await;
        self.finish_result(&result, "downstream_proxy_write_failed");
        result
    }

    async fn try_proxy_upstream_websocket(
        &mut self,
        route: &RouteTarget,
        headers: &HeaderMap,
        body: &mut Value,
    ) -> Result<UpstreamWebSocketAttempt> {
        let result = self
            .inner
            .try_proxy_upstream_websocket_with_probe(route, headers, body, self.probe.as_ref())
            .await;
        match &result {
            Ok(UpstreamWebSocketAttempt::Completed) => {
                if let Some(probe) = self.probe.as_ref() {
                    probe.finish_success();
                }
            }
            Ok(UpstreamWebSocketAttempt::UseHttp) => {}
            Err(error) => {
                if let Some(probe) = self.probe.as_ref() {
                    if error.is::<DownstreamClosed>() {
                        probe.mark_cancelled("downstream_websocket_closed");
                    } else {
                        probe.mark_error(502, "upstream_websocket_proxy_failed");
                    }
                }
            }
        }
        result
    }
}

struct HttpResponsesDownstream {
    stream: TcpStream,
}

impl HttpResponsesDownstream {
    fn new(stream: TcpStream) -> Self {
        Self { stream }
    }
}

#[async_trait]
impl ResponsesDownstream for HttpResponsesDownstream {
    async fn write_error(
        &mut self,
        status: u16,
        code: &str,
        message: String,
        route: Option<&RouteTarget>,
    ) -> Result<()> {
        write_error_response(&mut self.stream, status, code, message, route).await
    }

    async fn write_text_error(&mut self, status: u16, code: &str, message: String) -> Result<()> {
        write_text_error_response(&mut self.stream, status, code, message).await
    }

    async fn write_json(&mut self, status: u16, value: &Value) -> Result<()> {
        write_json_response(&mut self.stream, status, value).await
    }

    async fn start_event_stream(&mut self) -> Result<()> {
        let header = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream; charset=utf-8\r\ncache-control: no-cache\r\ntransfer-encoding: chunked\r\n{}connection: close\r\n\r\n",
            router_request_id_header()
        );
        write_all_with_timeout(
            &mut self.stream,
            header.as_bytes(),
            "写入 Responses SSE 响应头失败",
        )
        .await
    }

    async fn write_event(&mut self, event: &Value) -> Result<()> {
        write_responses_sse_event(&mut self.stream, event).await
    }

    async fn finish_event_stream(&mut self) -> Result<()> {
        finish_chunked_response(&mut self.stream).await
    }

    async fn proxy_response(&mut self, response: reqwest::Response) -> Result<()> {
        write_proxy_response(&mut self.stream, response, None).await
    }

    async fn proxy_response_with_probe(
        &mut self,
        response: reqwest::Response,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<()> {
        write_proxy_response(&mut self.stream, response, probe).await
    }
}

struct WebSocketResponsesDownstream {
    socket: WebSocketStream<TcpStream>,
    upstream: Option<CachedUpstreamWebSocket>,
    websocket_backoffs: Arc<Mutex<UpstreamWebSocketBackoffs>>,
    stream_id: Option<String>,
    adapted_history: AdaptedResponsesHistory,
    terminal_started: bool,
    pending_messages: VecDeque<(WebSocketMessage, Option<OwnedSemaphorePermit>)>,
    pending_budget_blocked: bool,
    request_body_budget: Arc<Semaphore>,
}

#[derive(Debug, Default)]
struct AdaptedResponsesHistory {
    last: Option<(String, Vec<Value>)>,
    pending_input: Option<Vec<Value>>,
}

impl AdaptedResponsesHistory {
    fn prepare(&mut self, body: &mut Value) -> bool {
        self.pending_input = None;
        let Some(object) = body.as_object_mut() else {
            return false;
        };
        let input = match object.get("input") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(input)) => input.clone(),
            Some(input @ (Value::String(_) | Value::Object(_))) => vec![input.clone()],
            Some(_) => return false,
        };
        let previous_response_id = object
            .get("previous_response_id")
            .filter(|value| !value.is_null())
            .and_then(Value::as_str)
            .map(str::trim);
        let mut context = if let Some(previous_response_id) = previous_response_id {
            if !is_codey_synthetic_response_id(previous_response_id) {
                return false;
            }
            let Some((_, context)) = self
                .last
                .as_ref()
                .filter(|(response_id, _)| response_id == previous_response_id)
            else {
                return false;
            };
            context.clone()
        } else {
            Vec::new()
        };
        context.extend(input);
        self.pending_input = Some(context.clone());
        if previous_response_id.is_none() {
            return false;
        }
        object.remove("previous_response_id");
        object.insert("input".to_string(), Value::Array(context));
        true
    }

    fn remember(&mut self, response_id: &str, output: &[Value]) {
        let Some(mut context) = self.pending_input.take() else {
            return;
        };
        context.extend(output.iter().cloned());
        // ponytail: Codex continuations are linear; retain branches only if a client needs them.
        self.last = Some((response_id.to_string(), context));
    }
}

enum IdleWebSocketEvent {
    Downstream(
        Option<std::result::Result<WebSocketMessage, tokio_tungstenite::tungstenite::Error>>,
    ),
    Upstream(Option<std::result::Result<WebSocketMessage, tokio_tungstenite::tungstenite::Error>>),
    MaintainUpstream,
}

impl WebSocketResponsesDownstream {
    #[cfg(test)]
    fn new(socket: WebSocketStream<TcpStream>) -> Self {
        Self::with_shared_backoffs(
            socket,
            Arc::new(Mutex::new(UpstreamWebSocketBackoffs::default())),
            Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS)),
        )
    }

    fn with_shared_backoffs(
        socket: WebSocketStream<TcpStream>,
        websocket_backoffs: Arc<Mutex<UpstreamWebSocketBackoffs>>,
        request_body_budget: Arc<Semaphore>,
    ) -> Self {
        Self {
            socket,
            upstream: None,
            websocket_backoffs,
            stream_id: None,
            adapted_history: AdaptedResponsesHistory::default(),
            terminal_started: false,
            pending_messages: VecDeque::new(),
            pending_budget_blocked: false,
            request_body_budget,
        }
    }

    fn set_stream_id(&mut self, stream_id: Option<String>) {
        self.stream_id = stream_id;
    }

    fn clear_stream_id(&mut self) {
        self.stream_id = None;
        self.terminal_started = false;
    }

    async fn next_message(&mut self) -> Result<Option<WebSocketMessage>> {
        loop {
            if !self.pending_messages.is_empty()
                && self.upstream.as_ref().is_none_or(|upstream| {
                    upstream.liveness.heartbeat_sent_at.is_none()
                        && upstream.liveness.maintenance_deadline() > Instant::now()
                })
            {
                let (message, _permit) = self.pending_messages.pop_front().unwrap();
                if self.pending_messages.is_empty() {
                    self.pending_budget_blocked = false;
                }
                return Ok(Some(message));
            }
            let Some(upstream) = self.upstream.as_ref() else {
                return self
                    .socket
                    .next()
                    .await
                    .transpose()
                    .context("读取 Codey Responses WebSocket 消息失败");
            };
            let maintenance_deadline = upstream.liveness.maintenance_deadline();
            let event = {
                let downstream_socket = &mut self.socket;
                let upstream_socket = &mut self
                    .upstream
                    .as_mut()
                    .expect("cached upstream must exist while it is polled")
                    .socket;
                let maintenance =
                    tokio::time::sleep_until(tokio::time::Instant::from_std(maintenance_deadline));
                tokio::pin!(maintenance);
                tokio::select! {
                    // Prefer already-ready liveness work before accepting a
                    // new request, so a stale socket is never used merely
                    // because the request and heartbeat deadline raced.
                    biased;
                    message = upstream_socket.next() => IdleWebSocketEvent::Upstream(message),
                    _ = &mut maintenance => IdleWebSocketEvent::MaintainUpstream,
                    message = downstream_socket.next(), if self.pending_messages.is_empty() => IdleWebSocketEvent::Downstream(message),
                }
            };

            match event {
                IdleWebSocketEvent::Downstream(message) => {
                    let message = message
                        .transpose()
                        .context("读取 Codey Responses WebSocket 消息失败")?;
                    if matches!(&message, Some(WebSocketMessage::Text(_)))
                        && self
                            .upstream
                            .as_ref()
                            .is_some_and(|upstream| upstream.liveness.heartbeat_sent_at.is_some())
                    {
                        // A user request must not be committed while a liveness
                        // probe is unresolved. Reconnect before attempting it;
                        // the deterministic HTTP fallback remains available if
                        // that fresh handshake fails.
                        self.upstream.take();
                    }
                    return Ok(message);
                }
                IdleWebSocketEvent::Upstream(Some(Ok(WebSocketMessage::Ping(payload)))) => {
                    let pong = self
                        .upstream
                        .as_mut()
                        .expect("cached upstream must exist while replying to Ping")
                        .socket
                        .send(WebSocketMessage::Pong(payload));
                    match tokio::time::timeout(UPSTREAM_WEBSOCKET_PONG_TIMEOUT, pong).await {
                        Ok(Ok(())) => self
                            .upstream
                            .as_mut()
                            .expect("cached upstream must exist after Pong write")
                            .liveness
                            .record_activity(Instant::now()),
                        Ok(Err(_)) | Err(_) => {
                            self.upstream.take();
                        }
                    }
                }
                IdleWebSocketEvent::Upstream(Some(Ok(WebSocketMessage::Pong(_)))) => {
                    self.upstream
                        .as_mut()
                        .expect("cached upstream must exist after Pong read")
                        .liveness
                        .record_pong(Instant::now());
                }
                IdleWebSocketEvent::Upstream(Some(Ok(_)))
                | IdleWebSocketEvent::Upstream(Some(Err(_)))
                | IdleWebSocketEvent::Upstream(None) => {
                    // No application events are valid between responses. A
                    // Close, EOF, read failure, or unexpected data frame makes
                    // the cache ineligible without affecting the downstream.
                    self.upstream.take();
                }
                IdleWebSocketEvent::MaintainUpstream => {
                    let now = Instant::now();
                    let action = self
                        .upstream
                        .as_ref()
                        .expect("cached upstream must exist during maintenance")
                        .liveness
                        .maintenance_action(now);
                    match action {
                        UpstreamWebSocketMaintenanceAction::None => {}
                        UpstreamWebSocketMaintenanceAction::Drop => {
                            self.upstream.take();
                        }
                        UpstreamWebSocketMaintenanceAction::SendPing => {
                            let ping = self
                                .upstream
                                .as_mut()
                                .expect("cached upstream must exist while sending Ping")
                                .socket
                                .send(WebSocketMessage::Ping(Default::default()));
                            match tokio::time::timeout(UPSTREAM_WEBSOCKET_PONG_TIMEOUT, ping).await
                            {
                                Ok(Ok(())) => self
                                    .upstream
                                    .as_mut()
                                    .expect("cached upstream must exist after Ping write")
                                    .liveness
                                    .record_heartbeat_sent(now),
                                Ok(Err(_)) | Err(_) => {
                                    self.upstream.take();
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    async fn write_text(&mut self, text: impl Into<WebSocketText>) -> Result<()> {
        tokio::time::timeout(
            DOWNSTREAM_WRITE_TIMEOUT,
            self.socket.send(WebSocketMessage::Text(text.into())),
        )
        .await
        .context("写入 Codey Responses WebSocket 消息超时")
        .and_then(|result| result.context("写入 Codey Responses WebSocket 消息失败"))
        .context(DownstreamClosed)
    }

    async fn write_pong(&mut self, payload: tokio_tungstenite::tungstenite::Bytes) -> Result<()> {
        tokio::time::timeout(
            DOWNSTREAM_WRITE_TIMEOUT,
            self.socket.send(WebSocketMessage::Pong(payload)),
        )
        .await
        .context("写入 Codey Responses WebSocket Pong 超时")?
        .context("写入 Codey Responses WebSocket Pong 失败")
    }

    async fn proxy_upstream_websocket(
        &mut self,
        route: &RouteTarget,
        headers: &HeaderMap,
        body: &mut Value,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<UpstreamWebSocketAttempt> {
        if !route.supports_websockets {
            return Ok(UpstreamWebSocketAttempt::UseHttp);
        }
        let upstream_url = route
            .upstream_websocket_url
            .as_ref()
            .map_err(|error| anyhow::anyhow!(error.clone()))?;
        let now = Instant::now();
        let auth_identity = UpstreamWebSocketAuthIdentity::from_headers(headers);
        let backoff_key =
            UpstreamWebSocketBackoffKey::new(&route.provider_id, upstream_url, auth_identity);
        let previous_response_id = responses_previous_response_id(body);
        let cached_available = self.upstream.as_ref().is_some_and(|cached| {
            cached.route_id == route.provider_id
                && cached.url == *upstream_url
                && cached.liveness.heartbeat_sent_at.is_none()
                && cached.liveness.maintenance_action(now)
                    != UpstreamWebSocketMaintenanceAction::Drop
        });
        if !cached_available {
            self.upstream.take();
        }
        let cached_matches =
            self.upstream
                .as_ref()
                .is_some_and(|cached| match previous_response_id {
                    Some(response_id) => cached.response_ids.contains(response_id),
                    None => cached.auth_identity == auth_identity,
                });
        if !cached_matches {
            if previous_response_id.is_some() {
                return Ok(UpstreamWebSocketAttempt::UseHttp);
            }
            self.upstream.take();
        }
        if self.upstream.is_none()
            && self
                .websocket_backoffs
                .lock()
                .expect("upstream WebSocket backoff mutex poisoned")
                .is_backing_off(&backoff_key, now)
        {
            return Ok(UpstreamWebSocketAttempt::UseHttp);
        }
        let mut upstream = if let Some(cached) = self.upstream.take() {
            cached
        } else {
            match self
                .wait_for_upstream(connect_upstream_responses_websocket(upstream_url, headers))
                .await?
            {
                Ok(socket) => {
                    self.websocket_backoffs
                        .lock()
                        .expect("upstream WebSocket backoff mutex poisoned")
                        .record_success(&backoff_key);
                    CachedUpstreamWebSocket {
                        route_id: route.provider_id.clone(),
                        url: upstream_url.clone(),
                        auth_identity,
                        response_ids: HashSet::new(),
                        liveness: UpstreamWebSocketLiveness::new(Instant::now()),
                        socket,
                    }
                }
                Err(error) => {
                    if upstream_websocket_endpoint_is_unsupported(&error) {
                        self.websocket_backoffs
                            .lock()
                            .expect("upstream WebSocket backoff mutex poisoned")
                            .record_unsupported(backoff_key, Instant::now());
                        record_router_failure_nonblocking(
                            "local_router_upstream_websocket_degraded",
                            "connect_responses_websocket",
                            format!("{error:#}"),
                            serde_json::json!({
                                "routeId": route.provider_id.as_str(),
                                "routeName": route.route_name.as_str(),
                                "upstream": route.upstream_authority.as_str(),
                                "fallback": "http_sse",
                                "unsupportedEndpoint": true,
                                "requestId": current_router_request_id(),
                            }),
                        );
                        return Ok(UpstreamWebSocketAttempt::UseHttp);
                    }
                    let (failure_count, backoff_duration) = self
                        .websocket_backoffs
                        .lock()
                        .expect("upstream WebSocket backoff mutex poisoned")
                        .record_failure(backoff_key.clone(), Instant::now());
                    record_router_failure_nonblocking(
                        "local_router_upstream_websocket_degraded",
                        "connect_responses_websocket",
                        format!("{error:#}"),
                        serde_json::json!({
                            "routeId": route.provider_id.as_str(),
                            "routeName": route.route_name.as_str(),
                            "upstream": route.upstream_authority.as_str(),
                            "fallback": "http_sse",
                            "failureCount": failure_count,
                            "backoffSeconds": backoff_duration.as_secs(),
                            "requestId": current_router_request_id(),
                        }),
                    );
                    return Ok(UpstreamWebSocketAttempt::UseHttp);
                }
            }
        };

        let upstream_model = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let message_body = body
            .as_object_mut()
            .context("Responses WebSocket 上游请求必须是 JSON 对象")?;
        message_body.remove("stream");
        message_body.remove("background");
        message_body.insert(
            "type".to_string(),
            Value::String("response.create".to_string()),
        );
        let message =
            serde_json::to_string(body).context("序列化 Responses WebSocket 上游请求失败")?;

        // Once send is attempted the request may have reached the upstream.
        // Any later failure is surfaced to the caller and is never replayed
        // over HTTP, avoiding duplicate tool calls and other side effects.
        if let Some(probe) = probe {
            probe.mark_upstream_send(UpstreamTransport::WebSocket);
        }
        match self
            .wait_for_upstream(tokio::time::timeout(
                DOWNSTREAM_WRITE_TIMEOUT,
                upstream.socket.send(WebSocketMessage::Text(message.into())),
            ))
            .await?
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                record_upstream_websocket_failure(&self.websocket_backoffs, &backoff_key);
                return Err(error).context("发送 Responses WebSocket 上游请求失败");
            }
            Err(_) => {
                record_upstream_websocket_failure(&self.websocket_backoffs, &backoff_key);
                anyhow::bail!("发送 Responses WebSocket 上游请求超时");
            }
        }

        let mut produced_response_id = None;
        loop {
            let next = match self
                .wait_for_upstream(tokio::time::timeout(
                    UPSTREAM_READ_IDLE_TIMEOUT,
                    upstream.socket.next(),
                ))
                .await?
            {
                Ok(next) => next,
                Err(_) => {
                    record_upstream_websocket_failure(&self.websocket_backoffs, &backoff_key);
                    anyhow::bail!("读取 Responses WebSocket 上游事件超时");
                }
            };
            let Some(message) = next else {
                record_upstream_websocket_failure(&self.websocket_backoffs, &backoff_key);
                anyhow::bail!("Responses WebSocket 上游在终态事件前断开");
            };
            let message = match message {
                Ok(message) => message,
                Err(error) => {
                    record_upstream_websocket_failure(&self.websocket_backoffs, &backoff_key);
                    return Err(error).context("读取 Responses WebSocket 上游事件失败");
                }
            };
            match message {
                WebSocketMessage::Text(text) => {
                    if !text.is_empty()
                        && let Some(probe) = probe
                    {
                        probe.mark_first_upstream_data(FirstByteSource::UpstreamWebSocketEvent);
                    }
                    upstream.liveness.record_activity(Instant::now());
                    let (events, raw_json_text) =
                        match serde_json::from_str::<Value>(text.as_str()) {
                            Ok(event) => (vec![event], Some(text)),
                            Err(json_error) => (
                                parse_responses_websocket_sse_events(text.as_str()).with_context(
                                    || {
                                        format!(
                                            "Responses WebSocket 上游响应既不是有效 JSON，也不是有效 SSE（JSON 错误：{json_error}）"
                                        )
                                    },
                                )?,
                                None,
                            ),
                        };
                    let mut raw_json_text = raw_json_text;
                    for mut event in events {
                        if responses_event_is_failure(&event) {
                            let error_summary = annotate_upstream_websocket_failure(
                                &mut event,
                                route,
                                &upstream_model,
                                upstream_url,
                            );
                            if let (Some(probe), Some(error_summary)) =
                                (probe, error_summary.as_deref())
                            {
                                probe.mark_upstream_error_summary(error_summary);
                            }
                            raw_json_text = None;
                        }
                        if let Some(probe) = probe {
                            probe.observe_event(&event);
                        }
                        let terminal = responses_event_is_terminal(&event);
                        if let Some(response_id) = responses_event_response_id(&event) {
                            produced_response_id = Some(response_id.to_string());
                        }
                        if self.event_needs_stream_id(&event) {
                            self.write_event(&event).await?;
                        } else if let Some(text) = raw_json_text.take() {
                            // Preserve an already-validated bare JSON frame on
                            // the latency-sensitive first-event path. SSE-wrapped
                            // WebSocket frames must be normalized to bare JSON
                            // before they are sent to Codex.
                            self.terminal_started |= terminal;
                            self.write_text(text).await?;
                        } else {
                            self.write_event(&event).await?;
                        }
                        if responses_event_has_user_content(&event)
                            && let Some(probe) = probe
                        {
                            probe.mark_first_downstream_content();
                        }
                        if terminal {
                            let successful_backoff_key = UpstreamWebSocketBackoffKey::new(
                                &route.provider_id,
                                upstream_url,
                                upstream.auth_identity,
                            );
                            if let Some(response_id) = produced_response_id {
                                upstream.response_ids.insert(response_id);
                            }
                            if responses_websocket_connection_is_reusable(&event) {
                                self.upstream = Some(upstream);
                            }
                            self.websocket_backoffs
                                .lock()
                                .expect("upstream WebSocket backoff mutex poisoned")
                                .record_success(&successful_backoff_key);
                            return Ok(UpstreamWebSocketAttempt::Completed);
                        }
                    }
                }
                WebSocketMessage::Ping(payload) => {
                    match self
                        .wait_for_upstream(tokio::time::timeout(
                            DOWNSTREAM_WRITE_TIMEOUT,
                            upstream.socket.send(WebSocketMessage::Pong(payload)),
                        ))
                        .await?
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            record_upstream_websocket_failure(
                                &self.websocket_backoffs,
                                &backoff_key,
                            );
                            return Err(error).context("回复 Responses WebSocket 上游 Ping 失败");
                        }
                        Err(_) => {
                            record_upstream_websocket_failure(
                                &self.websocket_backoffs,
                                &backoff_key,
                            );
                            anyhow::bail!("回复 Responses WebSocket 上游 Ping 超时");
                        }
                    }
                    upstream.liveness.record_activity(Instant::now());
                }
                WebSocketMessage::Pong(_) => upstream.liveness.record_pong(Instant::now()),
                WebSocketMessage::Close(_) => {
                    record_upstream_websocket_failure(&self.websocket_backoffs, &backoff_key);
                    anyhow::bail!("Responses WebSocket 上游在终态事件前关闭连接");
                }
                WebSocketMessage::Binary(_) | WebSocketMessage::Frame(_) => {
                    record_upstream_websocket_failure(&self.websocket_backoffs, &backoff_key);
                    anyhow::bail!("Responses WebSocket 上游返回了不支持的二进制消息");
                }
            }
        }
    }

    fn event_needs_stream_id(&self, event: &Value) -> bool {
        self.stream_id.is_some()
            && event.get("stream_id").is_none()
            && responses_websocket_connection_is_reusable(event)
    }

    async fn close(
        &mut self,
        frame: Option<tokio_tungstenite::tungstenite::protocol::CloseFrame>,
    ) -> Result<()> {
        // Dropping the upstream socket is enough to reclaim it immediately;
        // do not delay the client close handshake on a remote peer.
        self.upstream.take();
        tokio::time::timeout(DOWNSTREAM_WRITE_TIMEOUT, self.socket.close(frame))
            .await
            .context("关闭 Codey Responses WebSocket 超时")?
            .context("关闭 Codey Responses WebSocket 失败")
    }
}

fn responses_event_is_terminal(event: &Value) -> bool {
    matches!(
        event.get("type").and_then(Value::as_str),
        Some("response.completed" | "response.failed" | "response.incomplete" | "error")
    )
}

fn responses_event_type_has_user_content(event_type: &str) -> bool {
    matches!(
        event_type,
        "response.output_text.delta"
            | "response.refusal.delta"
            | "response.function_call_arguments.delta"
            | "response.custom_tool_call_input.delta"
            | "response.reasoning_summary_text.delta"
    )
}

fn responses_event_has_user_content(event: &Value) -> bool {
    let Some(event_type) = event.get("type").and_then(Value::as_str) else {
        return false;
    };
    if responses_event_type_has_user_content(event_type) {
        return event.get("delta").is_some_and(|delta| match delta {
            Value::Null => false,
            Value::String(value) => !value.is_empty(),
            Value::Array(value) => !value.is_empty(),
            Value::Object(value) => !value.is_empty(),
            Value::Bool(_) | Value::Number(_) => true,
        });
    }
    matches!(event_type, "response.completed" | "response.incomplete")
        && event
            .pointer("/response/output")
            .and_then(Value::as_array)
            .is_some_and(|output| !output.is_empty())
}

fn responses_event_is_failure(event: &Value) -> bool {
    matches!(
        event.get("type").and_then(Value::as_str),
        Some("response.failed" | "error")
    )
}

fn parse_responses_websocket_sse_events(text: &str) -> Result<Vec<Value>> {
    let bytes = text.as_bytes();
    let mut cursor = SseCursor::default();
    let mut events = Vec::new();
    while let Some(frame) = take_next_sse_frame(bytes, &mut cursor) {
        append_responses_websocket_sse_event(&mut events, frame)?;
    }
    if !bytes[cursor.consumed..].iter().all(u8::is_ascii_whitespace) {
        append_responses_websocket_sse_event(&mut events, &bytes[cursor.consumed..])?;
    }
    if events.is_empty() {
        anyhow::bail!("Responses WebSocket SSE 帧不包含 JSON data 事件");
    }
    Ok(events)
}

fn append_responses_websocket_sse_event(events: &mut Vec<Value>, frame: &[u8]) -> Result<()> {
    let Some(data) = sse_frame_data(frame)? else {
        return Ok(());
    };
    if data.trim() == "[DONE]" {
        return Ok(());
    }
    events.push(
        serde_json::from_str::<Value>(&data)
            .context("Responses WebSocket 上游 SSE data 不是有效 JSON")?,
    );
    Ok(())
}

fn responses_previous_response_id(body: &Value) -> Option<&str> {
    body.get("previous_response_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|response_id| !response_id.is_empty())
}

fn responses_event_response_id(event: &Value) -> Option<&str> {
    event
        .get("response")
        .and_then(|response| response.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|response_id| !response_id.is_empty())
}

fn responses_websocket_connection_is_reusable(event: &Value) -> bool {
    event
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        != Some("websocket_connection_limit_reached")
}

fn upstream_websocket_request(url: &str, headers: &HeaderMap) -> Result<WebSocketRequest> {
    let mut request = url
        .into_client_request()
        .context("创建 Responses WebSocket 上游握手请求失败")?;
    for (name, value) in headers {
        if is_hop_by_hop_header(name.as_str())
            || name
                .as_str()
                .to_ascii_lowercase()
                .starts_with("sec-websocket-")
        {
            continue;
        }
        request.headers_mut().insert(name.clone(), value.clone());
    }
    let beta_name = HeaderName::from_static("openai-beta");
    let current_beta = request
        .headers()
        .get(&beta_name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !current_beta
        .split(',')
        .any(|token| token.trim() == RESPONSES_WEBSOCKET_BETA)
    {
        let beta = if current_beta.trim().is_empty() {
            RESPONSES_WEBSOCKET_BETA.to_string()
        } else {
            format!("{current_beta}, {RESPONSES_WEBSOCKET_BETA}")
        };
        request.headers_mut().insert(
            beta_name,
            HeaderValue::from_str(&beta).context("构造 Responses WebSocket Beta 请求头失败")?,
        );
    }
    Ok(request)
}

async fn connect_upstream_responses_websocket(
    url: &str,
    headers: &HeaderMap,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
    let request = upstream_websocket_request(url, headers)?;
    let config = WebSocketConfig::default()
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_REQUEST_BYTES)
        .max_message_size(Some(MAX_UPSTREAM_RESPONSE_BYTES))
        .max_frame_size(Some(MAX_UPSTREAM_RESPONSE_BYTES));
    let (socket, _) = tokio::time::timeout(
        UPSTREAM_WEBSOCKET_CONNECT_TIMEOUT,
        // Responses events are small and latency-sensitive. Disable Nagle on
        // the underlying upstream TCP socket before the TLS/WS handshake.
        connect_async_with_config(request, Some(config), true),
    )
    .await
    .context("连接 Responses WebSocket 上游超时")?
    .context("连接 Responses WebSocket 上游失败")?;
    Ok(socket)
}

fn upstream_websocket_endpoint_is_unsupported(error: &anyhow::Error) -> bool {
    error.chain().any(|source| {
        source
            .downcast_ref::<WebSocketError>()
            .is_some_and(|error| {
                let WebSocketError::Http(response) = error else {
                    return false;
                };
                matches!(
                    response.status(),
                    WebSocketStatusCode::NOT_FOUND
                        | WebSocketStatusCode::METHOD_NOT_ALLOWED
                        | WebSocketStatusCode::GONE
                        | WebSocketStatusCode::NOT_IMPLEMENTED
                )
            })
    })
}

#[async_trait]
impl ResponsesDownstream for WebSocketResponsesDownstream {
    async fn wait_for_upstream<T, F>(&mut self, future: F) -> Result<T>
    where
        T: Send,
        F: std::future::Future<Output = T> + Send,
    {
        tokio::pin!(future);
        loop {
            tokio::select! {
                // Keep the same pinned deadline when Ping or queued requests arrive.
                result = &mut future => return Ok(result),
                message = self.socket.next(), if self.pending_messages.len() < 8 && !self.pending_budget_blocked => {
                    match message {
                        Some(Ok(WebSocketMessage::Ping(payload))) => {
                            self.write_pong(payload).await.map_err(|error| error.context(DownstreamClosed))?;
                        }
                        Some(Ok(WebSocketMessage::Pong(_))) => {}
                        Some(Ok(WebSocketMessage::Close(_))) | None => {
                            self.upstream.take();
                            self.pending_messages.clear();
                            return Err(DownstreamClosed.into());
                        }
                        Some(Err(error)) => return Err(anyhow::Error::new(error).context(DownstreamClosed)),
                        Some(Ok(message)) => {
                            // ponytail: full queues delay control frames; use an explicit
                            // cancellation channel if cancellation must bypass queued requests.
                            // At most one bounded frame may wait for the shared body budget.
                            let permit = match acquire_request_body_budget(&self.request_body_budget, message.len()) {
                                Ok(permit) => permit,
                                Err(_) => { self.pending_budget_blocked = true; None }
                            };
                            self.pending_messages.push_back((message, permit));
                        }
                    }
                }
            }
        }
    }
    fn is_websocket(&self) -> bool {
        true
    }

    fn prepare_adapted_response_context(&mut self, body: &mut Value) -> bool {
        self.adapted_history.prepare(body)
    }

    fn remember_adapted_response(&mut self, response_id: &str, output: &[Value]) {
        self.adapted_history.remember(response_id, output);
    }

    async fn write_error(
        &mut self,
        status: u16,
        code: &str,
        message: String,
        route: Option<&RouteTarget>,
    ) -> Result<()> {
        self.write_event(&websocket_response_failed_event(
            status, code, message, route,
        ))
        .await
    }

    async fn write_json(&mut self, status: u16, value: &Value) -> Result<()> {
        if !(200..300).contains(&status) {
            let message = value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("Codey 本地路由返回错误")
                .to_string();
            return self
                .write_error(status, "local_router_error", message, None)
                .await;
        }
        self.start_event_stream().await?;
        for event in responses_event_sequence(value)? {
            self.write_event(&event).await?;
        }
        self.finish_event_stream().await
    }

    async fn start_event_stream(&mut self) -> Result<()> {
        Ok(())
    }

    async fn write_event(&mut self, event: &Value) -> Result<()> {
        if responses_event_is_terminal(event) {
            if self.terminal_started {
                return Ok(());
            }
            self.terminal_started = true;
        }
        let encoded = if self.event_needs_stream_id(event) {
            let mut event = event.clone();
            event
                .as_object_mut()
                .context("Responses WebSocket 事件必须是 JSON 对象")?
                .insert(
                    "stream_id".to_string(),
                    Value::String(
                        self.stream_id
                            .as_deref()
                            .expect("stream id must exist when insertion is required")
                            .to_string(),
                    ),
                );
            serde_json::to_string(&event).context("序列化 Responses WebSocket 事件失败")?
        } else {
            serde_json::to_string(event).context("序列化 Responses WebSocket 事件失败")?
        };
        self.write_text(encoded).await
    }

    async fn finish_event_stream(&mut self) -> Result<()> {
        Ok(())
    }

    async fn proxy_response(&mut self, response: reqwest::Response) -> Result<()> {
        proxy_native_response_to_websocket(self, response, None).await
    }

    async fn proxy_response_with_probe(
        &mut self,
        response: reqwest::Response,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<()> {
        proxy_native_response_to_websocket(self, response, probe).await
    }

    async fn try_proxy_upstream_websocket(
        &mut self,
        route: &RouteTarget,
        headers: &HeaderMap,
        body: &mut Value,
    ) -> Result<UpstreamWebSocketAttempt> {
        self.proxy_upstream_websocket(route, headers, body, None)
            .await
    }

    async fn try_proxy_upstream_websocket_with_probe(
        &mut self,
        route: &RouteTarget,
        headers: &HeaderMap,
        body: &mut Value,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<UpstreamWebSocketAttempt> {
        self.proxy_upstream_websocket(route, headers, body, probe)
            .await
    }
}

fn websocket_response_failed_event(
    status: u16,
    code: &str,
    message: String,
    route: Option<&RouteTarget>,
) -> Value {
    let mut codey =
        serde_json::Map::from_iter([("httpStatus".to_string(), Value::Number(status.into()))]);
    if let Some(route) = route {
        codey.insert(
            "routeId".to_string(),
            Value::String(route.provider_id.clone()),
        );
        codey.insert(
            "routeName".to_string(),
            Value::String(route.route_name.clone()),
        );
    }
    if let Some(request_id) = current_router_request_id() {
        codey.insert("requestId".to_string(), Value::String(request_id));
    }
    json!({
        "type":"response.failed",
        "response":{
            "id":format!("resp_codey_{}", Uuid::new_v4()),
            "object":"response",
            "created_at":current_unix_timestamp(),
            "status":"failed",
            "output":[],
            "error":{
                "type":"codey_route_error",
                "code":code,
                "message":message,
                "codey":codey,
            },
            "incomplete_details":Value::Null,
        }
    })
}

async fn proxy_native_response_to_websocket(
    downstream: &mut WebSocketResponsesDownstream,
    response: reqwest::Response,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<()> {
    let status = response.status().as_u16();
    let mut prepared = await_upstream(
        downstream,
        prepare_upstream_response(response, "读取 Responses WebSocket 上游响应失败", probe),
    )
    .await??;
    if prepared.is_sse {
        downstream.start_event_stream().await?;
        let mut buffer = Vec::new();
        let mut cursor = SseCursor::default();
        let mut done = false;
        let mut terminal = false;
        while let Some(chunk) = await_upstream(
            downstream,
            read_prepared_upstream_chunk(
                &mut prepared,
                "读取 Responses WebSocket 上游 SSE 流失败",
                probe,
            ),
        )
        .await??
        {
            compact_sse_buffer(&mut buffer, &mut cursor);
            buffer.extend_from_slice(&chunk);
            ensure_sse_buffer_within_limit(&buffer, cursor.consumed)?;
            while let Some(frame) = take_next_sse_frame(&buffer, &mut cursor) {
                let Some(data) = sse_frame_data(frame)? else {
                    continue;
                };
                if data.trim() == "[DONE]" {
                    done = true;
                    break;
                }
                let event = serde_json::from_str::<Value>(&data)
                    .context("Responses WebSocket 上游 SSE data 不是有效 JSON")?;
                terminal |= responses_event_is_terminal(&event);
                if let Some(probe) = probe {
                    probe.observe_event(&event);
                }
                downstream.write_event(&event).await?;
                if responses_event_has_user_content(&event)
                    && let Some(probe) = probe
                {
                    probe.mark_first_downstream_content();
                }
            }
            if done {
                break;
            }
        }
        if !done
            && !buffer[cursor.consumed..]
                .iter()
                .all(u8::is_ascii_whitespace)
            && let Some(data) = sse_frame_data(&buffer[cursor.consumed..])?
            && data.trim() != "[DONE]"
        {
            let event = serde_json::from_str::<Value>(&data)
                .context("Responses WebSocket 上游 SSE 末尾 data 不是有效 JSON")?;
            terminal |= responses_event_is_terminal(&event);
            if let Some(probe) = probe {
                probe.observe_event(&event);
            }
            downstream.write_event(&event).await?;
            if responses_event_has_user_content(&event)
                && let Some(probe) = probe
            {
                probe.mark_first_downstream_content();
            }
        }
        if !terminal {
            anyhow::bail!("Responses HTTP/SSE 降级流在终态事件前断开");
        }
        return downstream.finish_event_stream().await;
    }

    let limit = if (200..300).contains(&status) {
        MAX_UPSTREAM_RESPONSE_BYTES
    } else {
        MAX_UPSTREAM_ERROR_BYTES
    };
    let body = await_upstream(
        downstream,
        read_bounded_prepared_upstream_body(
            prepared,
            limit,
            "读取 Responses WebSocket 上游响应失败",
            probe,
        ),
    )
    .await??;
    if (200..300).contains(&status)
        && let Ok(text) = std::str::from_utf8(&body)
        && responses_body_looks_like_sse(text)
    {
        let events = parse_responses_websocket_sse_events(text)
            .context("解析 Responses WebSocket 上游未标记的 SSE 响应失败")?;
        if !events.iter().any(responses_event_is_terminal) {
            anyhow::bail!("Responses HTTP/SSE 降级响应缺少终态事件");
        }
        downstream.start_event_stream().await?;
        for event in events {
            if let Some(probe) = probe {
                probe.observe_event(&event);
            }
            downstream.write_event(&event).await?;
            if responses_event_has_user_content(&event)
                && let Some(probe) = probe
            {
                probe.mark_first_downstream_content();
            }
        }
        return downstream.finish_event_stream().await;
    }
    match serde_json::from_slice::<Value>(&body) {
        Ok(value) => {
            if let Some(probe) = probe {
                probe.observe_response(status, &value);
            }
            let result = downstream.write_json(status, &value).await;
            if result.is_ok()
                && (200..300).contains(&status)
                && let Some(probe) = probe
            {
                probe.mark_first_downstream_content();
            }
            result
        }
        Err(error) => {
            let detail = String::from_utf8_lossy(&body);
            downstream
                .write_error(
                    if (200..300).contains(&status) {
                        502
                    } else {
                        status
                    },
                    "upstream_protocol_error",
                    if detail.trim().is_empty() {
                        format!("Responses WebSocket 上游响应不是有效 JSON：{error}")
                    } else {
                        format!(
                            "Responses WebSocket 上游返回无法解析的响应：{}",
                            detail.trim().chars().take(512).collect::<String>()
                        )
                    },
                    None,
                )
                .await
        }
    }
}

fn responses_body_looks_like_sse(text: &str) -> bool {
    text.lines()
        .any(|line| line.trim_end_matches('\r').starts_with("data:"))
}

async fn write_error_response(
    stream: &mut TcpStream,
    status: u16,
    code: &str,
    message: impl Into<String>,
    route: Option<&RouteTarget>,
) -> Result<()> {
    let mut codey = serde_json::Map::new();
    if let Some(route) = route {
        codey.insert("routeId".into(), Value::String(route.provider_id.clone()));
        codey.insert("routeName".into(), Value::String(route.route_name.clone()));
    }
    if let Some(request_id) = current_router_request_id() {
        codey.insert("requestId".into(), Value::String(request_id));
    }
    let mut error = serde_json::Map::from_iter([
        ("message".into(), Value::String(message.into())),
        ("type".into(), Value::String("codey_route_error".into())),
        ("code".into(), Value::String(code.to_string())),
    ]);
    if !codey.is_empty() {
        error.insert("codey".into(), Value::Object(codey));
    }
    write_json_response(stream, status, &json!({ "error": error })).await
}

async fn write_text_error_response<W>(
    stream: &mut W,
    status: u16,
    code: &str,
    message: impl AsRef<str>,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let body = if let Some(request_id) = current_router_request_id() {
        format!(
            "{}（错误码：{code}；请求 ID：{request_id}）\n",
            message.as_ref()
        )
    } else {
        format!("{}（错误码：{code}）\n", message.as_ref())
    };
    let reason = reason_phrase(status);
    let request_id_header = router_request_id_header();
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\n{request_id_header}connection: close\r\n\r\n",
        body.len()
    );
    write_all_with_timeout(stream, header.as_bytes(), "写入本地路由错误响应头失败").await?;
    write_all_with_timeout(stream, body.as_bytes(), "写入本地路由错误响应失败").await?;
    Ok(())
}

async fn write_json_response(stream: &mut TcpStream, status: u16, value: &Value) -> Result<()> {
    let mut body = serde_json::to_vec(value).context("序列化 Codey 本地路由响应失败")?;
    body.push(b'\n');
    let reason = reason_phrase(status);
    let request_id_header = router_request_id_header();
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n{request_id_header}connection: close\r\n\r\n",
        body.len()
    );
    write_all_with_timeout(stream, header.as_bytes(), "写入本地路由 JSON 响应头失败").await?;
    write_all_with_timeout(stream, &body, "写入本地路由 JSON 响应失败").await?;
    Ok(())
}

async fn write_static_response(
    stream: &mut TcpStream,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let header = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\ncache-control: no-store\r\nx-content-type-options: nosniff\r\nconnection: close\r\n\r\n",
        body.len()
    );
    write_all_with_timeout(stream, header.as_bytes(), "写入请求日志页面响应头失败").await?;
    write_all_with_timeout(stream, body, "写入请求日志页面失败").await?;
    Ok(())
}

#[derive(Debug)]
struct UpstreamReadIdleTimeout {
    operation: &'static str,
}

impl std::fmt::Display for UpstreamReadIdleTimeout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}超过读取空闲期限", self.operation)
    }
}

impl std::error::Error for UpstreamReadIdleTimeout {}

struct PreparedUpstreamResponse {
    response: reqwest::Response,
    prefix: VecDeque<Bytes>,
    is_sse: bool,
}

async fn prepare_upstream_response(
    mut response: reqwest::Response,
    operation: &'static str,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<PreparedUpstreamResponse> {
    if response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(is_sse_content_type)
    {
        return Ok(PreparedUpstreamResponse {
            response,
            prefix: VecDeque::new(),
            is_sse: true,
        });
    }

    let mut prefix = VecDeque::new();
    let mut sniff = Vec::with_capacity(UPSTREAM_SSE_SNIFF_BYTES);
    loop {
        let Some(chunk) = read_upstream_chunk(&mut response, operation, probe).await? else {
            return Ok(PreparedUpstreamResponse {
                response,
                prefix,
                is_sse: false,
            });
        };
        if chunk.is_empty() {
            continue;
        }
        let remaining = UPSTREAM_SSE_SNIFF_BYTES.saturating_sub(sniff.len());
        sniff.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        prefix.push_back(chunk);
        if let Some(is_sse) = classify_upstream_sse_prefix(&sniff) {
            return Ok(PreparedUpstreamResponse {
                response,
                prefix,
                is_sse,
            });
        }
        if sniff.len() == UPSTREAM_SSE_SNIFF_BYTES {
            return Ok(PreparedUpstreamResponse {
                response,
                prefix,
                is_sse: false,
            });
        }
    }
}

fn classify_upstream_sse_prefix(prefix: &[u8]) -> Option<bool> {
    const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";
    const SSE_PREFIXES: [&[u8]; 5] = [b"data:", b"event:", b"id:", b"retry:", b":"];
    let prefix = if prefix.starts_with(UTF8_BOM) {
        &prefix[UTF8_BOM.len()..]
    } else if UTF8_BOM.starts_with(prefix) {
        return None;
    } else {
        prefix
    };
    let prefix = &prefix[prefix
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(prefix.len())..];
    if prefix.is_empty() {
        return None;
    }
    if SSE_PREFIXES.iter().any(|marker| prefix.starts_with(marker)) {
        return Some(true);
    }
    if SSE_PREFIXES.iter().any(|marker| marker.starts_with(prefix)) {
        return None;
    }
    Some(false)
}

async fn read_prepared_upstream_chunk(
    prepared: &mut PreparedUpstreamResponse,
    operation: &'static str,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<Option<Bytes>> {
    if let Some(chunk) = prepared.prefix.pop_front() {
        return Ok(Some(chunk));
    }
    read_upstream_chunk(&mut prepared.response, operation, probe).await
}

async fn read_bounded_prepared_upstream_body(
    mut prepared: PreparedUpstreamResponse,
    limit: usize,
    operation: &'static str,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = read_prepared_upstream_chunk(&mut prepared, operation, probe).await? {
        if body.len().saturating_add(chunk.len()) > limit {
            anyhow::bail!("{operation}超过 Codey 安全上限");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn read_upstream_chunk(
    response: &mut reqwest::Response,
    operation: &'static str,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<Option<Bytes>> {
    let chunk = tokio::time::timeout(UPSTREAM_READ_IDLE_TIMEOUT, response.chunk())
        .await
        .map_err(|_| anyhow::Error::new(UpstreamReadIdleTimeout { operation }))?
        .with_context(|| operation)?;
    if chunk.as_ref().is_some_and(|chunk| !chunk.is_empty())
        && let Some(probe) = probe
    {
        probe.mark_first_upstream_data(FirstByteSource::UpstreamHttpBody);
    }
    Ok(chunk)
}

async fn write_all_with_timeout<W>(
    stream: &mut W,
    bytes: &[u8],
    operation: &'static str,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(DOWNSTREAM_WRITE_TIMEOUT, stream.write_all(bytes))
        .await
        .with_context(|| format!("{operation}超过写入期限"))?
        .with_context(|| operation)
}

/// Writes one `transfer-encoding: chunked` frame with a single `write_all`.
/// Emitting the size line, payload and trailing CRLF as three separate writes
/// produced three small TCP segments for every streamed event.
async fn write_chunked_frame<W>(
    stream: &mut W,
    payload: &[u8],
    operation: &'static str,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut frame = Vec::with_capacity(payload.len() + 16);
    frame.extend_from_slice(format!("{:x}\r\n", payload.len()).as_bytes());
    frame.extend_from_slice(payload);
    frame.extend_from_slice(b"\r\n");
    write_all_with_timeout(stream, &frame, operation).await
}

async fn write_proxy_response(
    stream: &mut TcpStream,
    response: reqwest::Response,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<()> {
    let status = response.status().as_u16();
    let reason = response.status().canonical_reason().unwrap_or("OK");
    let original_content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let mut prepared = prepare_upstream_response(response, "读取上游响应失败", probe).await?;
    let upstream_is_sse = prepared.is_sse;
    let content_type = if upstream_is_sse {
        "text/event-stream"
    } else {
        original_content_type.as_str()
    };
    let mut log_tap = probe.map(|probe| RequestLogResponseTap::new(probe.clone()));
    write_all_with_timeout(
        stream,
        format!(
            "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ntransfer-encoding: chunked\r\n{}connection: close\r\n\r\n",
            router_request_id_header()
        )
        .as_bytes(),
        "写入上游响应头失败",
    )
    .await?;
    if let Some(probe) = probe {
        probe.mark_response_started(status);
    }
    while let Some(chunk) =
        read_prepared_upstream_chunk(&mut prepared, "读取上游响应失败", probe).await?
    {
        if chunk.is_empty() {
            continue;
        }
        write_chunked_frame(stream, &chunk, "写入上游响应块失败").await?;
        if let Some(tap) = log_tap.as_mut() {
            tap.observe(&chunk);
        }
        if !upstream_is_sse && let Some(probe) = probe {
            probe.mark_first_downstream_content();
        }
    }
    if let Some(tap) = log_tap.as_mut() {
        tap.finish();
    }
    write_all_with_timeout(stream, b"0\r\n\r\n", "结束上游响应流失败").await?;
    Ok(())
}

const REQUEST_LOG_TAP_CHUNK_BYTES: usize = 8 * 1024;
const REQUEST_LOG_TAP_QUEUE_CHUNKS: usize = 64;
const REQUEST_LOG_USAGE_KEY_BYTES: usize = 64;
const REQUEST_LOG_USAGE_SCALAR_BYTES: usize = 64;
const REQUEST_LOG_USAGE_NESTING_DEPTH: usize = 64;

struct RequestLogObservedChunk {
    bytes: Bytes,
    written_at: Instant,
    _queue_budget: OwnedSemaphorePermit,
}

struct RequestLogResponseTap {
    sender: Option<mpsc::Sender<RequestLogObservedChunk>>,
    queue_budget: Arc<Semaphore>,
    probe: RouteRequestLogProbe,
}

impl RequestLogResponseTap {
    fn new(probe: RouteRequestLogProbe) -> Self {
        let (sender, mut receiver) =
            mpsc::channel::<RequestLogObservedChunk>(REQUEST_LOG_TAP_QUEUE_CHUNKS);
        let queue_budget = Arc::new(Semaphore::new(REQUEST_LOG_TAP_QUEUE_CHUNKS));
        let worker_probe = probe.clone();
        let finish_guard = probe.defer_finish();
        tokio::spawn(async move {
            let _finish_guard = finish_guard;
            let mut projector = RequestLogMetadataProjector::default();
            while let Some(chunk) = receiver.recv().await {
                if let Err(reason) =
                    projector.observe(&chunk.bytes, &worker_probe, chunk.written_at)
                {
                    worker_probe.mark_usage_unavailable(reason);
                    break;
                }
            }
            if projector.usage.is_some() {
                worker_probe.mark_usage_unavailable("usage_projection_failed");
            }
        });
        Self {
            sender: Some(sender),
            queue_budget,
            probe,
        }
    }

    fn observe(&mut self, chunk: &Bytes) {
        let Some(sender) = self.sender.as_ref() else {
            return;
        };
        let permits = chunk
            .len()
            .div_ceil(REQUEST_LOG_TAP_CHUNK_BYTES)
            .clamp(1, REQUEST_LOG_TAP_QUEUE_CHUNKS) as u32;
        let Ok(queue_budget) = Arc::clone(&self.queue_budget).try_acquire_many_owned(permits)
        else {
            self.probe.mark_usage_unavailable("observer_queue_full");
            self.sender = None;
            return;
        };
        let observed = RequestLogObservedChunk {
            bytes: chunk.clone(),
            written_at: Instant::now(),
            _queue_budget: queue_budget,
        };
        match sender.try_send(observed) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.probe.mark_usage_unavailable("observer_queue_full");
                self.sender = None;
            }
            Err(mpsc::error::TrySendError::Closed(_)) => self.sender = None,
        }
    }

    fn finish(&mut self) {
        self.sender = None;
    }
}

#[derive(Default)]
struct RequestLogMetadataProjector {
    depth: usize,
    in_string: bool,
    escaped: bool,
    string: Vec<u8>,
    string_overflow: bool,
    last_key: Option<ProjectedMetadataKey>,
    awaiting_usage: bool,
    awaiting_scalar: Option<ProjectedMetadataScalar>,
    capturing_scalar: Option<ProjectedMetadataScalar>,
    event_type_has_user_content: bool,
    event_delta_has_content: bool,
    usage: Option<UsageCapture>,
}

#[derive(Clone, Copy)]
enum ProjectedMetadataKey {
    Usage,
    Type,
    Delta,
    Status,
    Code,
}

#[derive(Clone, Copy)]
enum ProjectedMetadataScalar {
    EventType,
    Delta,
    ResponseStatus,
    ErrorCode,
}

struct UsageCapture {
    containers: Vec<UsageContainer>,
    values: [Option<(u8, u64)>; 6],
    in_string: bool,
    escaped: bool,
    unicode_escape_digits: u8,
    string_is_key: bool,
    string: Vec<u8>,
    string_overflow: bool,
    scalar: Vec<u8>,
    scalar_overflow: bool,
    scalar_field: Option<(ProjectedUsageField, u8)>,
}

#[derive(Clone, Copy)]
enum UsageObjectContext {
    Root,
    CachedDetails(u8),
    ReasoningDetails(u8),
    Other,
}

#[derive(Clone, Copy)]
enum ProjectedUsageKey {
    Field(ProjectedUsageField, u8),
    Object(UsageObjectContext),
    Other,
}

#[derive(Clone, Copy)]
enum ProjectedUsageField {
    Input,
    Output,
    CachedInput,
    CacheCreationInput,
    ReasoningOutput,
    Total,
}

impl ProjectedUsageField {
    fn index(self) -> usize {
        match self {
            Self::Input => 0,
            Self::Output => 1,
            Self::CachedInput => 2,
            Self::CacheCreationInput => 3,
            Self::ReasoningOutput => 4,
            Self::Total => 5,
        }
    }
}

#[derive(Clone, Copy)]
enum UsageJsonState {
    ObjectKeyOrEnd,
    ObjectKey,
    ObjectColon,
    ObjectValue,
    ObjectCommaOrEnd,
    ArrayValueOrEnd,
    ArrayValue,
    ArrayCommaOrEnd,
}

struct UsageContainer {
    context: Option<UsageObjectContext>,
    state: UsageJsonState,
    key: ProjectedUsageKey,
}

impl UsageCapture {
    fn new() -> Self {
        Self {
            containers: vec![UsageContainer {
                context: Some(UsageObjectContext::Root),
                state: UsageJsonState::ObjectKeyOrEnd,
                key: ProjectedUsageKey::Other,
            }],
            values: [None; 6],
            in_string: false,
            escaped: false,
            unicode_escape_digits: 0,
            string_is_key: false,
            string: Vec::new(),
            string_overflow: false,
            scalar: Vec::new(),
            scalar_overflow: false,
            scalar_field: None,
        }
    }

    fn observe_byte(&mut self, byte: u8) -> std::result::Result<bool, &'static str> {
        if self.in_string {
            if self.unicode_escape_digits > 0 {
                if !byte.is_ascii_hexdigit() {
                    return Err("usage_projection_failed");
                }
                self.unicode_escape_digits -= 1;
            } else if self.escaped {
                if !matches!(
                    byte,
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' | b'u'
                ) {
                    return Err("usage_projection_failed");
                }
                self.escaped = false;
                if byte == b'u' {
                    self.unicode_escape_digits = 4;
                }
            } else if byte == b'\\' {
                self.escaped = true;
            } else if byte == b'"' {
                self.in_string = false;
                if self.string_is_key {
                    self.finish_key()?;
                }
                return Ok(false);
            } else if byte < b' ' {
                return Err("usage_projection_failed");
            }
            if self.string_is_key && !self.string_overflow {
                if self.string.len() < REQUEST_LOG_USAGE_KEY_BYTES {
                    self.string.push(byte);
                } else {
                    self.string_overflow = true;
                }
            }
            return Ok(false);
        }

        if !self.scalar.is_empty() || self.scalar_overflow {
            if byte.is_ascii_whitespace() || matches!(byte, b',' | b'}' | b']') {
                self.finish_scalar()?;
            } else {
                if self.scalar.len() < REQUEST_LOG_USAGE_SCALAR_BYTES {
                    self.scalar.push(byte);
                } else {
                    self.scalar_overflow = true;
                }
                return Ok(false);
            }
        }

        if byte.is_ascii_whitespace() {
            return Ok(false);
        }

        match byte {
            b'"' => {
                let state = self
                    .containers
                    .last()
                    .map(|container| container.state)
                    .ok_or("usage_projection_failed")?;
                self.string_is_key = matches!(
                    state,
                    UsageJsonState::ObjectKeyOrEnd | UsageJsonState::ObjectKey
                );
                if !self.string_is_key {
                    self.begin_value()?;
                }
                self.string.clear();
                self.string_overflow = false;
                self.in_string = true;
                self.escaped = false;
                self.unicode_escape_digits = 0;
            }
            b'{' | b'[' => {
                let key = self.begin_value()?;
                if self.containers.len() >= REQUEST_LOG_USAGE_NESTING_DEPTH {
                    return Err("usage_projection_limit_exceeded");
                }
                self.containers.push(if byte == b'{' {
                    UsageContainer {
                        context: Some(match key {
                            ProjectedUsageKey::Object(context) => context,
                            _ => UsageObjectContext::Other,
                        }),
                        state: UsageJsonState::ObjectKeyOrEnd,
                        key: ProjectedUsageKey::Other,
                    }
                } else {
                    UsageContainer {
                        context: None,
                        state: UsageJsonState::ArrayValueOrEnd,
                        key: ProjectedUsageKey::Other,
                    }
                });
            }
            b'}' => {
                let Some(container) = self.containers.last() else {
                    return Err("usage_projection_failed");
                };
                if container.context.is_none()
                    || !matches!(
                        container.state,
                        UsageJsonState::ObjectKeyOrEnd | UsageJsonState::ObjectCommaOrEnd
                    )
                {
                    return Err("usage_projection_failed");
                }
                self.containers.pop();
                if self.containers.is_empty() {
                    return Ok(true);
                }
            }
            b']' => {
                let Some(container) = self.containers.last() else {
                    return Err("usage_projection_failed");
                };
                if container.context.is_some()
                    || !matches!(
                        container.state,
                        UsageJsonState::ArrayValueOrEnd | UsageJsonState::ArrayCommaOrEnd
                    )
                {
                    return Err("usage_projection_failed");
                }
                self.containers.pop();
            }
            b':' => {
                let container = self
                    .containers
                    .last_mut()
                    .ok_or("usage_projection_failed")?;
                if !matches!(container.state, UsageJsonState::ObjectColon) {
                    return Err("usage_projection_failed");
                }
                container.state = UsageJsonState::ObjectValue;
            }
            b',' => {
                let container = self
                    .containers
                    .last_mut()
                    .ok_or("usage_projection_failed")?;
                container.state = match container.state {
                    UsageJsonState::ObjectCommaOrEnd => UsageJsonState::ObjectKey,
                    UsageJsonState::ArrayCommaOrEnd => UsageJsonState::ArrayValue,
                    _ => return Err("usage_projection_failed"),
                };
            }
            b'-' | b'0'..=b'9' | b't' | b'f' | b'n' => {
                let key = self.begin_value()?;
                self.scalar.clear();
                self.scalar.push(byte);
                self.scalar_overflow = false;
                self.scalar_field = match key {
                    ProjectedUsageKey::Field(field, priority) => Some((field, priority)),
                    _ => None,
                };
            }
            _ => return Err("usage_projection_failed"),
        }
        Ok(false)
    }

    fn begin_value(&mut self) -> std::result::Result<ProjectedUsageKey, &'static str> {
        let container = self
            .containers
            .last_mut()
            .ok_or("usage_projection_failed")?;
        match container.state {
            UsageJsonState::ObjectValue => {
                container.state = UsageJsonState::ObjectCommaOrEnd;
                Ok(std::mem::replace(
                    &mut container.key,
                    ProjectedUsageKey::Other,
                ))
            }
            UsageJsonState::ArrayValueOrEnd | UsageJsonState::ArrayValue => {
                container.state = UsageJsonState::ArrayCommaOrEnd;
                Ok(ProjectedUsageKey::Other)
            }
            _ => Err("usage_projection_failed"),
        }
    }

    fn finish_key(&mut self) -> std::result::Result<(), &'static str> {
        let container = self
            .containers
            .last_mut()
            .ok_or("usage_projection_failed")?;
        let context = container.context.ok_or("usage_projection_failed")?;
        if !matches!(
            container.state,
            UsageJsonState::ObjectKeyOrEnd | UsageJsonState::ObjectKey
        ) {
            return Err("usage_projection_failed");
        }
        container.key = if self.string_overflow {
            ProjectedUsageKey::Other
        } else {
            let mut encoded = Vec::with_capacity(self.string.len() + 2);
            encoded.push(b'"');
            encoded.extend_from_slice(&self.string);
            encoded.push(b'"');
            let key = serde_json::from_slice::<String>(&encoded)
                .map_err(|_| "usage_projection_failed")?;
            projected_usage_key(context, key.as_bytes())
        };
        container.state = UsageJsonState::ObjectColon;
        Ok(())
    }

    fn finish_scalar(&mut self) -> std::result::Result<(), &'static str> {
        if self.scalar_overflow {
            if self.scalar_field.is_some() {
                return Err("usage_projection_failed");
            }
        } else {
            let value = serde_json::from_slice::<Value>(&self.scalar)
                .map_err(|_| "usage_projection_failed")?;
            if let Some((field, priority)) = self.scalar_field
                && let Some(value) = value.as_u64()
            {
                let slot = &mut self.values[field.index()];
                if slot.is_none_or(|(current_priority, _)| priority <= current_priority) {
                    *slot = Some((priority, value));
                }
            }
        }
        self.scalar.clear();
        self.scalar_overflow = false;
        self.scalar_field = None;
        Ok(())
    }

    fn into_value(self) -> Value {
        let value = |field: ProjectedUsageField| self.values[field.index()].map(|(_, value)| value);
        json!({
            "input_tokens": value(ProjectedUsageField::Input),
            "output_tokens": value(ProjectedUsageField::Output),
            "input_tokens_details": {
                "cached_tokens": value(ProjectedUsageField::CachedInput),
            },
            "cache_creation_input_tokens": value(ProjectedUsageField::CacheCreationInput),
            "output_tokens_details": {
                "reasoning_tokens": value(ProjectedUsageField::ReasoningOutput),
            },
            "total_tokens": value(ProjectedUsageField::Total),
        })
    }
}

fn projected_usage_key(context: UsageObjectContext, value: &[u8]) -> ProjectedUsageKey {
    use ProjectedUsageField as Field;
    use ProjectedUsageKey::{Field as KeyField, Object, Other};
    match (context, value) {
        (UsageObjectContext::Root, b"input_tokens") => KeyField(Field::Input, 0),
        (UsageObjectContext::Root, b"prompt_tokens") => KeyField(Field::Input, 1),
        (UsageObjectContext::Root, b"inputTokens") => KeyField(Field::Input, 2),
        (UsageObjectContext::Root, b"output_tokens") => KeyField(Field::Output, 0),
        (UsageObjectContext::Root, b"completion_tokens") => KeyField(Field::Output, 1),
        (UsageObjectContext::Root, b"outputTokens") => KeyField(Field::Output, 2),
        (UsageObjectContext::Root, b"input_tokens_details") => {
            Object(UsageObjectContext::CachedDetails(0))
        }
        (UsageObjectContext::Root, b"prompt_tokens_details") => {
            Object(UsageObjectContext::CachedDetails(1))
        }
        (UsageObjectContext::Root, b"cache_read_input_tokens") => KeyField(Field::CachedInput, 2),
        (UsageObjectContext::Root, b"cache_read_tokens") => KeyField(Field::CachedInput, 3),
        (UsageObjectContext::Root, b"cached_input_tokens") => KeyField(Field::CachedInput, 4),
        (UsageObjectContext::Root, b"cache_creation_input_tokens") => {
            KeyField(Field::CacheCreationInput, 0)
        }
        (UsageObjectContext::Root, b"cache_creation_tokens") => {
            KeyField(Field::CacheCreationInput, 1)
        }
        (UsageObjectContext::Root, b"cache_write_input_tokens") => {
            KeyField(Field::CacheCreationInput, 2)
        }
        (UsageObjectContext::Root, b"output_tokens_details") => {
            Object(UsageObjectContext::ReasoningDetails(0))
        }
        (UsageObjectContext::Root, b"completion_tokens_details") => {
            Object(UsageObjectContext::ReasoningDetails(1))
        }
        (UsageObjectContext::Root, b"reasoning_tokens") => KeyField(Field::ReasoningOutput, 2),
        (UsageObjectContext::Root, b"total_tokens") => KeyField(Field::Total, 0),
        (UsageObjectContext::Root, b"totalTokens") => KeyField(Field::Total, 1),
        (UsageObjectContext::CachedDetails(priority), b"cached_tokens") => {
            KeyField(Field::CachedInput, priority)
        }
        (UsageObjectContext::ReasoningDetails(priority), b"reasoning_tokens") => {
            KeyField(Field::ReasoningOutput, priority)
        }
        _ => Other,
    }
}

impl RequestLogMetadataProjector {
    fn observe(
        &mut self,
        bytes: &[u8],
        probe: &RouteRequestLogProbe,
        written_at: Instant,
    ) -> std::result::Result<(), &'static str> {
        for &byte in bytes {
            if let Some(capture) = self.usage.as_mut() {
                let complete = capture.observe_byte(byte)?;
                if complete {
                    let capture = self.usage.take().expect("usage capture exists");
                    probe.observe_event(&json!({"usage": capture.into_value()}));
                }
                continue;
            }

            if self.in_string {
                if self.escaped {
                    self.escaped = false;
                } else if byte == b'\\' {
                    self.escaped = true;
                } else if byte == b'"' {
                    self.in_string = false;
                    if let Some(field) = self.capturing_scalar.take() {
                        match field {
                            ProjectedMetadataScalar::Delta => {
                                self.event_delta_has_content =
                                    self.string_overflow || !self.string.is_empty();
                                if self.event_type_has_user_content && self.event_delta_has_content
                                {
                                    probe.mark_first_downstream_content_at(written_at);
                                }
                            }
                            ProjectedMetadataScalar::EventType if !self.string_overflow => {
                                let value = String::from_utf8_lossy(&self.string);
                                self.event_type_has_user_content =
                                    responses_event_type_has_user_content(&value);
                                if self.event_type_has_user_content && self.event_delta_has_content
                                {
                                    probe.mark_first_downstream_content_at(written_at);
                                }
                                probe.observe_terminal_projection(Some(&value), None, None)
                            }
                            ProjectedMetadataScalar::ResponseStatus if !self.string_overflow => {
                                let value = String::from_utf8_lossy(&self.string);
                                probe.observe_terminal_projection(None, Some(&value), None)
                            }
                            ProjectedMetadataScalar::ErrorCode if !self.string_overflow => {
                                let value = String::from_utf8_lossy(&self.string);
                                probe.observe_terminal_projection(None, None, Some(&value))
                            }
                            ProjectedMetadataScalar::EventType
                            | ProjectedMetadataScalar::ResponseStatus
                            | ProjectedMetadataScalar::ErrorCode => {}
                        }
                        self.last_key = None;
                    } else {
                        self.last_key = (!self.string_overflow)
                            .then(|| projected_metadata_key(&self.string))
                            .flatten();
                    }
                } else if self.string.len() < 64 {
                    self.string.push(byte);
                } else {
                    self.string_overflow = true;
                }
                continue;
            }

            match byte {
                b'"' => {
                    self.in_string = true;
                    self.escaped = false;
                    self.string.clear();
                    self.string_overflow = false;
                    self.capturing_scalar = self.awaiting_scalar.take();
                    self.awaiting_usage = false;
                }
                b':' => {
                    let key = self.last_key.take();
                    self.awaiting_usage =
                        self.depth <= 2 && matches!(key, Some(ProjectedMetadataKey::Usage));
                    self.awaiting_scalar = match (self.depth, key) {
                        (1, Some(ProjectedMetadataKey::Type)) => {
                            Some(ProjectedMetadataScalar::EventType)
                        }
                        (1, Some(ProjectedMetadataKey::Delta)) => {
                            Some(ProjectedMetadataScalar::Delta)
                        }
                        (1 | 2, Some(ProjectedMetadataKey::Status)) => {
                            Some(ProjectedMetadataScalar::ResponseStatus)
                        }
                        (2 | 3, Some(ProjectedMetadataKey::Code)) => {
                            Some(ProjectedMetadataScalar::ErrorCode)
                        }
                        _ => None,
                    };
                }
                b'{' if self.awaiting_usage => {
                    self.awaiting_usage = false;
                    self.awaiting_scalar = None;
                    self.usage = Some(UsageCapture::new());
                }
                b'{' => {
                    if self.depth == 0 {
                        self.event_type_has_user_content = false;
                        self.event_delta_has_content = false;
                    }
                    self.depth += 1;
                    self.awaiting_usage = false;
                    self.awaiting_scalar = None;
                    self.last_key = None;
                }
                b'[' => {
                    self.depth += 1;
                    self.awaiting_usage = false;
                    self.awaiting_scalar = None;
                    self.last_key = None;
                }
                b'}' | b']' => {
                    self.depth = self.depth.saturating_sub(1);
                    self.awaiting_usage = false;
                    self.awaiting_scalar = None;
                    self.last_key = None;
                }
                byte if byte.is_ascii_whitespace() => {}
                _ => {
                    self.awaiting_usage = false;
                    self.awaiting_scalar = None;
                    self.last_key = None;
                }
            }
        }
        Ok(())
    }
}

fn projected_metadata_key(value: &[u8]) -> Option<ProjectedMetadataKey> {
    match value {
        b"usage" => Some(ProjectedMetadataKey::Usage),
        b"type" => Some(ProjectedMetadataKey::Type),
        b"delta" => Some(ProjectedMetadataKey::Delta),
        b"status" => Some(ProjectedMetadataKey::Status),
        b"code" => Some(ProjectedMetadataKey::Code),
        _ => None,
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct UpstreamErrorSummary {
    message: Option<String>,
    error_type: Option<String>,
    code: Option<String>,
}

fn first_string_at<'a>(value: &'a Value, pointers: &[&str]) -> Option<&'a str> {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn sanitize_upstream_error_text(
    value: &str,
    route: &RouteTarget,
    max_chars: usize,
) -> Option<String> {
    let mut sanitized = value.to_string();
    if let Ok(headers) = route.upstream_headers.as_ref() {
        for secret in headers.values().filter_map(|value| value.to_str().ok()) {
            let secret = secret.trim();
            if secret.len() < 4 {
                continue;
            }
            sanitized = sanitized.replace(secret, "***");
            if let Some((scheme, token)) = secret.split_once(' ')
                && scheme.eq_ignore_ascii_case("bearer")
                && token.trim().len() >= 4
            {
                sanitized = sanitized.replace(token.trim(), "***");
            }
        }
    }
    let collapsed = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let mut chars = collapsed.chars();
    let mut bounded = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        bounded.push('…');
    }
    Some(bounded)
}

fn upstream_error_summary(value: &Value, route: &RouteTarget) -> UpstreamErrorSummary {
    let message = first_string_at(
        value,
        &[
            "/response/error/message",
            "/error/error/message",
            "/error/message",
            "/message",
            "/detail",
            "/error",
        ],
    )
    .and_then(|message| sanitize_upstream_error_text(message, route, 512));
    let error_type = first_string_at(
        value,
        &["/response/error/type", "/error/error/type", "/error/type"],
    )
    .and_then(|kind| sanitize_upstream_error_text(kind, route, 128));
    let code = first_string_at(
        value,
        &[
            "/response/error/code",
            "/error/error/code",
            "/error/code",
            "/code",
        ],
    )
    .and_then(|code| sanitize_upstream_error_text(code, route, 128));
    UpstreamErrorSummary {
        message,
        error_type,
        code,
    }
}

fn upstream_error_detail(summary: &UpstreamErrorSummary) -> Option<String> {
    let mut detail = summary.message.clone().unwrap_or_default();
    let mut attributes = Vec::new();
    if let Some(error_type) = summary.error_type.as_deref() {
        attributes.push(format!("类型：{error_type}"));
    }
    if let Some(code) = summary.code.as_deref() {
        attributes.push(format!("代码：{code}"));
    }
    if !attributes.is_empty() {
        if detail.is_empty() {
            detail = attributes.join("；");
        } else {
            detail.push_str(&format!("（{}）", attributes.join("；")));
        }
    }
    (!detail.is_empty()).then_some(detail)
}

fn bounded_upstream_request_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut chars = trimmed.chars().filter(|character| !character.is_control());
    let bounded = chars.by_ref().take(128).collect::<String>();
    (!bounded.is_empty()).then_some(bounded)
}

fn upstream_request_id_from_headers(headers: &HeaderMap) -> Option<String> {
    ["x-request-id", "request-id", "x-amzn-requestid", "cf-ray"]
        .iter()
        .find_map(|name| headers.get(*name).and_then(|value| value.to_str().ok()))
        .and_then(bounded_upstream_request_id)
}

async fn write_upstream_http_error<D>(
    downstream: &mut D,
    response: reqwest::Response,
    resolved: &RouteSelection,
    bridge: ProtocolBridge,
    request_kind: ResponsesRequestKind,
) -> Result<()>
where
    D: ResponsesDownstream + ?Sized,
{
    let status = response.status().as_u16();
    let upstream_request_id = upstream_request_id_from_headers(response.headers());
    let probe = downstream.request_log_probe().cloned();
    let body = await_upstream(
        downstream,
        read_bounded_upstream_error_body(response, probe.as_ref()),
    )
    .await??;
    let parsed = serde_json::from_slice::<Value>(&body).ok();
    let summary = parsed
        .as_ref()
        .map(|value| upstream_error_summary(value, &resolved.route))
        .unwrap_or_default();
    let detail = upstream_error_detail(&summary);
    if let (Some(probe), Some(detail)) = (probe.as_ref(), detail.as_deref()) {
        probe.mark_upstream_error_summary(detail);
    }
    let mut message = format!(
        "Codey 线路「{}」请求模型 {} 时，上游返回 HTTP {status}",
        route_display_name(&resolved.route),
        resolved.upstream_model
    );
    if let Some(detail) = detail.as_deref() {
        message.push_str(&format!("：{detail}"));
    }
    if let Some(request_id) = upstream_request_id.as_deref() {
        message.push_str(&format!("（上游请求 ID：{request_id}）"));
    }
    record_router_failure_nonblocking(
        "local_router_upstream_http_error",
        "proxy_local_router_response",
        message.clone(),
        serde_json::json!({
            "routeId": resolved.provider_id.as_str(),
            "routeName": resolved.route.route_name.as_str(),
            "requestedModel": resolved.requested_model.as_str(),
            "model": resolved.upstream_model.as_str(),
            "status": status,
            "upstreamRequestId": upstream_request_id,
            "upstreamErrorType": summary.error_type,
            "upstreamErrorCode": summary.code,
            "upstream": resolved.route.upstream_authority.as_str(),
            "upstreamProtocol": bridge.upstream_protocol().label(),
            "protocolBridge": bridge.label(),
            "requestKind": request_kind.label(),
            "requestId": current_router_request_id(),
        }),
    );
    if downstream.is_websocket() {
        downstream
            .write_error(
                status,
                "upstream_http_error",
                message,
                Some(&resolved.route),
            )
            .await
    } else {
        downstream
            .write_text_error(status, "upstream_http_error", message)
            .await
    }
}

fn annotate_upstream_websocket_failure(
    event: &mut Value,
    route: &RouteTarget,
    model: &str,
    upstream_url: &str,
) -> Option<String> {
    let summary = upstream_error_summary(event, route);
    let error_summary = upstream_error_detail(&summary);
    let detail = error_summary.as_deref().unwrap_or("上游未提供具体错误信息");
    let message = format!(
        "Codey 线路「{}」请求模型 {model} 时，Responses WebSocket 上游返回错误：{detail}",
        route_display_name(route)
    );
    let upstream_request_id = first_string_at(
        event,
        &[
            "/response/error/request_id",
            "/error/request_id",
            "/request_id",
        ],
    )
    .and_then(bounded_upstream_request_id);
    record_router_failure_nonblocking(
        "local_router_upstream_websocket_error",
        "proxy_responses_websocket_event",
        message.clone(),
        serde_json::json!({
            "routeId": route.provider_id.as_str(),
            "routeName": route.route_name.as_str(),
            "model": model,
            "upstream": route.upstream_authority.as_str(),
            "upstreamEndpoint": upstream_url,
            "upstreamRequestId": upstream_request_id,
            "upstreamErrorType": summary.error_type,
            "upstreamErrorCode": summary.code,
            "requestId": current_router_request_id(),
        }),
    );

    let mut updated = false;
    if let Some(error) = event
        .get_mut("response")
        .and_then(Value::as_object_mut)
        .and_then(|response| response.get_mut("error"))
        .and_then(Value::as_object_mut)
    {
        error.insert("message".to_string(), Value::String(message.clone()));
        updated = true;
    }
    if !updated && let Some(error) = event.get_mut("error").and_then(Value::as_object_mut) {
        error.insert("message".to_string(), Value::String(message.clone()));
        updated = true;
    }
    if !updated && let Some(object) = event.as_object_mut() {
        object.insert("message".to_string(), Value::String(message));
    }
    error_summary
}

async fn write_chat_completions_as_responses<D>(
    downstream: &mut D,
    response: reqwest::Response,
    model: &str,
    stream_requested: bool,
    route: &RouteTarget,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()>
where
    D: ResponsesDownstream + ?Sized,
{
    let probe = downstream.request_log_probe().cloned();
    let prepared = match await_upstream(
        downstream,
        prepare_upstream_response(
            response,
            "读取 Chat Completions 上游响应失败",
            probe.as_ref(),
        ),
    )
    .await?
    {
        Ok(prepared) => prepared,
        Err(error) => {
            return downstream
                .write_error(
                    502,
                    "upstream_protocol_error",
                    format!(
                        "线路「{}」的 Chat Completions 响应无法转换为 Responses：{error:#}",
                        route_display_name(route)
                    ),
                    Some(route),
                )
                .await;
        }
    };
    if stream_requested && prepared.is_sse {
        return stream_chat_completions_as_responses(
            downstream,
            prepared,
            model,
            route,
            tool_bridge,
        )
        .await;
    }
    let responses = match await_upstream(
        downstream,
        read_chat_completions_as_responses(prepared, model, tool_bridge, probe.as_ref()),
    )
    .await?
    {
        Ok(responses) => responses,
        Err(error) => {
            return downstream
                .write_error(
                    502,
                    "upstream_protocol_error",
                    format!(
                        "线路「{}」的 Chat Completions 响应无法转换为 Responses：{error:#}",
                        route_display_name(route)
                    ),
                    Some(route),
                )
                .await;
        }
    };
    if stream_requested {
        write_responses_response_as_events(downstream, &responses).await
    } else {
        downstream.write_json(200, &responses).await
    }
}

async fn read_chat_completions_as_responses(
    mut prepared: PreparedUpstreamResponse,
    model: &str,
    tool_bridge: &ResponsesToolBridge,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<Value> {
    let chat = if prepared.is_sse {
        collect_chat_completion_sse(&mut prepared, model, probe).await?
    } else {
        let body = read_bounded_prepared_upstream_body(
            prepared,
            MAX_UPSTREAM_RESPONSE_BYTES,
            "读取 Chat Completions 上游响应失败",
            probe,
        )
        .await?;
        match serde_json::from_slice::<Value>(&body) {
            Ok(chat) => chat,
            Err(json_error) if body.starts_with(b"data:") || body.windows(6).any(|w| w == b"\ndata:") => {
                parse_chat_completion_sse_bytes(&body, model).with_context(|| {
                    format!("Chat Completions 上游响应既不是有效 JSON，也无法作为 SSE 解析：{json_error}")
                })?
            }
            Err(error) => return Err(error).context("Chat Completions 上游响应不是有效 JSON"),
        }
    };
    chat_completion_to_responses_body_with_tool_bridge(chat, model, tool_bridge)
}

async fn write_anthropic_messages_as_responses<D>(
    downstream: &mut D,
    response: reqwest::Response,
    model: &str,
    stream_requested: bool,
    route: &RouteTarget,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()>
where
    D: ResponsesDownstream + ?Sized,
{
    if !response.status().is_success() {
        let status = response.status();
        let probe = downstream.request_log_probe().cloned();
        let body = await_upstream(
            downstream,
            read_bounded_upstream_error_body(response, probe.as_ref()),
        )
        .await??;
        let detail = anthropic_upstream_error_detail(&body, route);
        if let (Some(probe), Some(detail)) = (probe.as_ref(), detail.as_deref()) {
            probe.mark_upstream_error_summary(detail);
        }
        return downstream
            .write_error(
                502,
                "anthropic_upstream_error",
                format!(
                    "线路「{}」的 Anthropic Messages 上游返回 HTTP {}{}",
                    route_display_name(route),
                    status.as_u16(),
                    detail
                        .as_deref()
                        .map(|detail| format!("：{detail}"))
                        .unwrap_or_default()
                ),
                Some(route),
            )
            .await;
    }
    let probe = downstream.request_log_probe().cloned();
    let prepared = match await_upstream(
        downstream,
        prepare_upstream_response(
            response,
            "读取 Anthropic Messages 上游响应失败",
            probe.as_ref(),
        ),
    )
    .await?
    {
        Ok(prepared) => prepared,
        Err(error) => {
            return downstream
                .write_error(
                    502,
                    "upstream_protocol_error",
                    format!(
                        "线路「{}」的 Anthropic Messages 响应无法转换为 Responses：{error:#}",
                        route_display_name(route)
                    ),
                    Some(route),
                )
                .await;
        }
    };
    if stream_requested && prepared.is_sse {
        return stream_anthropic_messages_as_responses(
            downstream,
            prepared,
            model,
            route,
            tool_bridge,
        )
        .await;
    }
    let responses = match await_upstream(
        downstream,
        read_anthropic_messages_as_responses(prepared, model, tool_bridge, probe.as_ref()),
    )
    .await?
    {
        Ok(responses) => responses,
        Err(error) => {
            return downstream
                .write_error(
                    502,
                    "upstream_protocol_error",
                    format!(
                        "线路「{}」的 Anthropic Messages 响应无法转换为 Responses：{error:#}",
                        route_display_name(route)
                    ),
                    Some(route),
                )
                .await;
        }
    };
    if stream_requested {
        write_responses_response_as_events(downstream, &responses).await
    } else {
        downstream.write_json(200, &responses).await
    }
}

fn anthropic_upstream_error_detail(body: &[u8], route: &RouteTarget) -> Option<String> {
    let value = serde_json::from_slice::<Value>(body).ok()?;
    upstream_error_detail(&upstream_error_summary(&value, route))
}

async fn read_bounded_upstream_error_body(
    mut response: reqwest::Response,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) =
        read_upstream_chunk(&mut response, "读取上游错误响应失败", probe).await?
    {
        let remaining = MAX_UPSTREAM_ERROR_BYTES.saturating_sub(body.len());
        if remaining == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if body.len() == MAX_UPSTREAM_ERROR_BYTES {
            break;
        }
    }
    Ok(body)
}

async fn read_anthropic_messages_as_responses(
    mut prepared: PreparedUpstreamResponse,
    model: &str,
    tool_bridge: &ResponsesToolBridge,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<Value> {
    let message = if prepared.is_sse {
        collect_anthropic_message_sse(&mut prepared, model, probe).await?
    } else {
        let body = read_bounded_prepared_upstream_body(
            prepared,
            MAX_UPSTREAM_RESPONSE_BYTES,
            "读取 Anthropic Messages 上游响应失败",
            probe,
        )
        .await?;
        match serde_json::from_slice::<Value>(&body) {
            Ok(message) => message,
            Err(json_error)
                if body.starts_with(b"event:")
                    || body.starts_with(b"data:")
                    || body.windows(7).any(|window| window == b"\nevent:")
                    || body.windows(6).any(|window| window == b"\ndata:") =>
            {
                parse_anthropic_message_sse_bytes(&body, model).with_context(|| {
                    format!(
                        "Anthropic Messages 上游响应既不是有效 JSON，也无法作为 SSE 解析：{json_error}"
                    )
                })?
            }
            Err(error) => {
                return Err(error).context("Anthropic Messages 上游响应不是有效 JSON");
            }
        }
    };
    anthropic_message_to_responses_body_with_tool_bridge(&message, model, tool_bridge)
}

#[cfg(test)]
fn anthropic_message_to_responses_body(message: &Value, fallback_model: &str) -> Result<Value> {
    let tool_bridge = ResponsesToolBridge::default();
    anthropic_message_to_responses_body_with_tool_bridge(message, fallback_model, &tool_bridge)
}

fn anthropic_message_to_responses_body_with_tool_bridge(
    message: &Value,
    fallback_model: &str,
    tool_bridge: &ResponsesToolBridge,
) -> Result<Value> {
    if message.get("type").and_then(Value::as_str) == Some("error") {
        let detail = message
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("未知错误");
        anyhow::bail!("Anthropic Messages 返回错误：{detail}");
    }
    let content = message
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("Anthropic message 缺少 content 数组"))?;
    let mut output = Vec::new();
    let mut message_content = Vec::new();
    let mut output_text_parts = Vec::new();
    for block in content {
        let block_type = block
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Anthropic content block 缺少 type"))?;
        match block_type {
            "text" => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("Anthropic text block 缺少 text"))?;
                output_text_parts.push(text.to_string());
                message_content.push(json!({
                    "type":"output_text",
                    "text":text,
                    "annotations":[],
                }));
            }
            "refusal" => {
                let refusal = block
                    .get("refusal")
                    .or_else(|| block.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                message_content.push(json!({"type":"refusal","refusal":refusal}));
            }
            "tool_use" => {
                let call_id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("Anthropic tool_use 缺少 id"))?;
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("Anthropic tool_use 缺少 name"))?;
                let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                if !input.is_object() {
                    anyhow::bail!("Anthropic tool_use.input 必须是 JSON 对象");
                }
                let tool_name = tool_bridge.restore_upstream_name(name)?;
                let arguments = serde_json::to_string(&input)
                    .context("序列化 Anthropic tool_use.input 失败")?;
                output.push(responses_tool_call_item_from_upstream_arguments(
                    &tool_name,
                    call_id.to_string(),
                    arguments,
                    "completed",
                    "Anthropic custom tool_use.input",
                )?);
            }
            // Raw chain-of-thought must not be surfaced as assistant text.
            // Signature/redacted blocks are provider state and have no safe
            // stateless Responses representation.
            "thinking" | "redacted_thinking" => {}
            other => {
                anyhow::bail!("Anthropic content block 类型 {other} 不能转换为 Responses output")
            }
        }
    }
    if !message_content.is_empty() {
        output.insert(
            0,
            json!({
                "id":format!("msg_codey_{}", Uuid::new_v4()),
                "type":"message",
                "status":"completed",
                "role":"assistant",
                "content":message_content,
            }),
        );
    }
    if output.is_empty() {
        output.push(json!({
            "id":format!("msg_codey_{}", Uuid::new_v4()),
            "type":"message",
            "status":"completed",
            "role":"assistant",
            "content":[{"type":"output_text","text":"","annotations":[]}],
        }));
    }
    let stop_reason = message.get("stop_reason").and_then(Value::as_str);
    let incomplete_reason = match stop_reason {
        Some("max_tokens") => Some("max_output_tokens"),
        Some("refusal") => Some("content_filter"),
        _ => None,
    };
    let status = if incomplete_reason.is_some() {
        "incomplete"
    } else {
        "completed"
    };
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_model);
    let mut responses = json!({
        "id":format!("resp_codey_{}", Uuid::new_v4()),
        "object":"response",
        "created_at":current_unix_timestamp(),
        "status":status,
        "model":model,
        "output":output,
        "output_text":output_text_parts.join(""),
        "error":Value::Null,
        "incomplete_details":incomplete_reason.map(|reason| json!({"reason":reason})),
    });
    if let Some(usage) = message.get("usage") {
        responses
            .as_object_mut()
            .expect("Responses wrapper must be an object")
            .insert(
                "usage".to_string(),
                anthropic_usage_to_responses_usage(usage),
            );
    }
    Ok(responses)
}

fn anthropic_usage_to_responses_usage(usage: &Value) -> Value {
    let uncached_input_tokens = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_creation_tokens = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached_tokens = usage
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let input_tokens = uncached_input_tokens
        .saturating_add(cache_creation_tokens)
        .saturating_add(cached_tokens);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "input_tokens":input_tokens,
        "input_tokens_details":{"cached_tokens":cached_tokens},
        "output_tokens":output_tokens,
        "output_tokens_details":{"reasoning_tokens":0},
        "total_tokens":input_tokens.saturating_add(output_tokens),
    })
}

#[derive(Debug, Default)]
struct AnthropicSseBlock {
    block_type: String,
    text: String,
    id: String,
    name: String,
    input: Option<Value>,
    partial_json: String,
}

#[derive(Debug)]
struct AnthropicSseAccumulator {
    id: String,
    model: String,
    blocks: BTreeMap<usize, AnthropicSseBlock>,
    stop_reason: Option<String>,
    usage: serde_json::Map<String, Value>,
    stopped: bool,
}

impl AnthropicSseAccumulator {
    fn new(model: &str) -> Self {
        Self {
            id: format!("msg_codey_{}", Uuid::new_v4()),
            model: model.to_string(),
            blocks: BTreeMap::new(),
            stop_reason: None,
            usage: serde_json::Map::new(),
            stopped: false,
        }
    }

    fn ingest(&mut self, event: &Value) -> Result<()> {
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Anthropic SSE 事件缺少 type"))?;
        match event_type {
            "ping" => {}
            "error" => {
                let detail = event
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("未知错误");
                anyhow::bail!("Anthropic SSE 返回错误：{detail}");
            }
            "message_start" => {
                let message = event
                    .get("message")
                    .and_then(Value::as_object)
                    .ok_or_else(|| anyhow::anyhow!("message_start 缺少 message"))?;
                if let Some(id) = message
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    self.id = id.to_string();
                }
                if let Some(model) = message
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    self.model = model.to_string();
                }
                if let Some(usage) = message.get("usage").and_then(Value::as_object) {
                    self.usage.extend(usage.clone());
                }
                if let Some(content) = message.get("content").and_then(Value::as_array) {
                    for (index, block) in content.iter().enumerate() {
                        self.blocks
                            .insert(index, anthropic_sse_block_from_value(block)?);
                    }
                }
            }
            "content_block_start" => {
                let index = anthropic_sse_index(event)?;
                let block = event
                    .get("content_block")
                    .ok_or_else(|| anyhow::anyhow!("content_block_start 缺少 content_block"))?;
                self.blocks
                    .insert(index, anthropic_sse_block_from_value(block)?);
            }
            "content_block_delta" => {
                let index = anthropic_sse_index(event)?;
                let delta = event
                    .get("delta")
                    .and_then(Value::as_object)
                    .ok_or_else(|| anyhow::anyhow!("content_block_delta 缺少 delta"))?;
                let delta_type = delta
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("Anthropic content delta 缺少 type"))?;
                let block = self.blocks.entry(index).or_default();
                match delta_type {
                    "text_delta" => {
                        if block.block_type.is_empty() {
                            block.block_type = "text".to_string();
                        }
                        block.text.push_str(
                            delta
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        );
                    }
                    "input_json_delta" => {
                        if block.block_type.is_empty() {
                            block.block_type = "tool_use".to_string();
                        }
                        block.partial_json.push_str(
                            delta
                                .get("partial_json")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        );
                    }
                    "thinking_delta" => {
                        if block.block_type.is_empty() {
                            block.block_type = "thinking".to_string();
                        }
                        block.text.push_str(
                            delta
                                .get("thinking")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        );
                    }
                    "signature_delta" | "citations_delta" => {}
                    other => anyhow::bail!("不支持的 Anthropic content delta 类型 {other}"),
                }
            }
            "content_block_stop" => {}
            "message_delta" => {
                if let Some(stop_reason) = event
                    .get("delta")
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    self.stop_reason = Some(stop_reason.to_string());
                }
                if let Some(usage) = event.get("usage").and_then(Value::as_object) {
                    self.usage.extend(usage.clone());
                }
            }
            "message_stop" => self.stopped = true,
            other => anyhow::bail!("不支持的 Anthropic SSE 事件类型 {other}"),
        }
        Ok(())
    }

    fn into_message(self) -> Result<Value> {
        if !self.stopped
            && self
                .stop_reason
                .as_deref()
                .is_none_or(|reason| reason.trim().is_empty())
        {
            anyhow::bail!("Anthropic Messages SSE 在结束事件或 stop_reason 前断开");
        }
        let content = self
            .blocks
            .into_values()
            .map(anthropic_sse_block_into_value)
            .collect::<Result<Vec<_>>>()?;
        Ok(json!({
            "id":self.id,
            "type":"message",
            "role":"assistant",
            "model":self.model,
            "content":content,
            "stop_reason":self.stop_reason,
            "usage":Value::Object(self.usage),
        }))
    }
}

fn anthropic_sse_index(event: &Value) -> Result<usize> {
    event
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| anyhow::anyhow!("Anthropic SSE 事件缺少有效 index"))
}

fn anthropic_sse_block_from_value(block: &Value) -> Result<AnthropicSseBlock> {
    let block_type = block
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Anthropic content block 缺少 type"))?;
    Ok(AnthropicSseBlock {
        block_type: block_type.to_string(),
        text: block
            .get("text")
            .or_else(|| block.get("thinking"))
            .or_else(|| block.get("refusal"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        id: block
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        name: block
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        input: block.get("input").cloned(),
        partial_json: String::new(),
    })
}

fn anthropic_sse_block_into_value(block: AnthropicSseBlock) -> Result<Value> {
    match block.block_type.as_str() {
        "text" => Ok(json!({"type":"text","text":block.text})),
        "refusal" => Ok(json!({"type":"refusal","refusal":block.text})),
        "thinking" | "redacted_thinking" => {
            Ok(json!({"type":block.block_type,"thinking":block.text}))
        }
        "tool_use" => {
            if block.id.is_empty() || block.name.is_empty() {
                anyhow::bail!("Anthropic 流式 tool_use 缺少 id 或 name");
            }
            let input = if block.partial_json.trim().is_empty() {
                block.input.unwrap_or_else(|| json!({}))
            } else {
                serde_json::from_str::<Value>(&block.partial_json)
                    .context("Anthropic 流式 tool_use 参数不是有效 JSON")?
            };
            if !input.is_object() {
                anyhow::bail!("Anthropic 流式 tool_use.input 必须是 JSON 对象");
            }
            Ok(json!({
                "type":"tool_use",
                "id":block.id,
                "name":block.name,
                "input":input,
            }))
        }
        other => anyhow::bail!("不支持的 Anthropic content block 类型 {other}"),
    }
}

async fn collect_anthropic_message_sse(
    prepared: &mut PreparedUpstreamResponse,
    model: &str,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<Value> {
    let mut accumulator = AnthropicSseAccumulator::new(model);
    let mut buffer = Vec::new();
    let mut cursor = SseCursor::default();
    while let Some(chunk) =
        read_prepared_upstream_chunk(prepared, "读取 Anthropic Messages SSE 流失败", probe).await?
    {
        compact_sse_buffer(&mut buffer, &mut cursor);
        buffer.extend_from_slice(&chunk);
        ensure_sse_buffer_within_limit(&buffer, cursor.consumed)?;
        while let Some(frame) = take_next_sse_frame(&buffer, &mut cursor) {
            let Some(data) = sse_frame_data(frame)? else {
                continue;
            };
            accumulator.ingest(
                &serde_json::from_str::<Value>(&data)
                    .context("Anthropic Messages SSE data 不是有效 JSON")?,
            )?;
        }
        if accumulator.stopped {
            break;
        }
    }
    if !accumulator.stopped
        && !buffer[cursor.consumed..]
            .iter()
            .all(u8::is_ascii_whitespace)
        && let Some(data) = sse_frame_data(&buffer[cursor.consumed..])?
    {
        accumulator.ingest(
            &serde_json::from_str::<Value>(&data)
                .context("Anthropic Messages SSE 末尾 data 不是有效 JSON")?,
        )?;
    }
    accumulator.into_message()
}

fn parse_anthropic_message_sse_bytes(bytes: &[u8], model: &str) -> Result<Value> {
    let mut accumulator = AnthropicSseAccumulator::new(model);
    let mut cursor = SseCursor::default();
    while let Some(frame) = take_next_sse_frame(bytes, &mut cursor) {
        let Some(data) = sse_frame_data(frame)? else {
            continue;
        };
        accumulator.ingest(
            &serde_json::from_str::<Value>(&data)
                .context("Anthropic Messages SSE data 不是有效 JSON")?,
        )?;
        if accumulator.stopped {
            return accumulator.into_message();
        }
    }
    if !bytes[cursor.consumed..].iter().all(u8::is_ascii_whitespace)
        && let Some(data) = sse_frame_data(&bytes[cursor.consumed..])?
    {
        accumulator.ingest(
            &serde_json::from_str::<Value>(&data)
                .context("Anthropic Messages SSE 末尾 data 不是有效 JSON")?,
        )?;
    }
    accumulator.into_message()
}

async fn stream_anthropic_messages_as_responses<D>(
    downstream: &mut D,
    mut prepared: PreparedUpstreamResponse,
    model: &str,
    route: &RouteTarget,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()>
where
    D: ResponsesDownstream + ?Sized,
{
    let mut output = ResponsesSseState::new(model, tool_bridge);
    output.start(downstream).await?;
    let mut accumulator = AnthropicSseAccumulator::new(model);
    let request_log_probe = downstream.request_log_probe().cloned();
    let result: Result<()> = async {
        let mut buffer = Vec::new();
        let mut cursor = SseCursor::default();
        while let Some(chunk) = await_upstream(
            downstream,
            read_prepared_upstream_chunk(
                &mut prepared,
                "读取 Anthropic Messages SSE 流失败",
                request_log_probe.as_ref(),
            ),
        )
        .await??
        {
            compact_sse_buffer(&mut buffer, &mut cursor);
            buffer.extend_from_slice(&chunk);
            ensure_sse_buffer_within_limit(&buffer, cursor.consumed)?;
            while let Some(frame) = take_next_sse_frame(&buffer, &mut cursor) {
                let Some(data) = sse_frame_data(frame)? else {
                    continue;
                };
                let event = serde_json::from_str::<Value>(&data)
                    .context("Anthropic Messages SSE data 不是有效 JSON")?;
                accumulator.ingest(&event)?;
                emit_anthropic_stream_event(&mut output, downstream, &event).await?;
            }
            if accumulator.stopped {
                break;
            }
        }
        if !accumulator.stopped
            && !buffer[cursor.consumed..]
                .iter()
                .all(u8::is_ascii_whitespace)
            && let Some(data) = sse_frame_data(&buffer[cursor.consumed..])?
        {
            let event = serde_json::from_str::<Value>(&data)
                .context("Anthropic Messages SSE 末尾 data 不是有效 JSON")?;
            accumulator.ingest(&event)?;
            emit_anthropic_stream_event(&mut output, downstream, &event).await?;
        }
        // Text is already retained by output. Keep tool and unknown blocks in
        // the existing conversion so validation and error ordering stay intact.
        accumulator.blocks.retain(|_, block| {
            !matches!(
                block.block_type.as_str(),
                "text" | "refusal" | "thinking" | "redacted_thinking"
            )
        });
        let message = accumulator.into_message()?;
        let completed =
            anthropic_message_to_responses_body_with_tool_bridge(&message, model, tool_bridge)?;
        drop(message);
        if output.output_order.is_empty() {
            let events = output.ensure_message();
            output.write_events(downstream, events).await?;
        }
        let usage = completed.get("usage").cloned();
        let incomplete_reason = completed
            .get("incomplete_details")
            .and_then(|details| details.get("reason"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        drop(completed);
        output
            .finish(downstream, usage, incomplete_reason.as_deref())
            .await
    }
    .await;
    if let Err(error) = result {
        if error.is::<DownstreamClosed>() {
            return Err(error);
        }
        let (code, message) = streaming_failure_message(&error, route);
        let _ = output.fail(downstream, code, &message).await;
        return Err(error);
    }
    Ok(())
}

async fn emit_anthropic_stream_event<D>(
    output: &mut ResponsesSseState<'_>,
    downstream: &mut D,
    event: &Value,
) -> Result<()>
where
    D: ResponsesDownstream + ?Sized,
{
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Anthropic SSE 事件缺少 type"))?;
    let mut events = Vec::new();
    match event_type {
        "message_start" => {
            if let Some(content) = event
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array)
            {
                for (index, block) in content.iter().enumerate() {
                    events.extend(anthropic_stream_block_start(output, index, block)?);
                }
            }
        }
        "content_block_start" => {
            let index = anthropic_sse_index(event)?;
            let block = event
                .get("content_block")
                .ok_or_else(|| anyhow::anyhow!("content_block_start 缺少 content_block"))?;
            events.extend(anthropic_stream_block_start(output, index, block)?);
        }
        "content_block_delta" => {
            let index = anthropic_sse_index(event)?;
            let delta = event
                .get("delta")
                .and_then(Value::as_object)
                .ok_or_else(|| anyhow::anyhow!("content_block_delta 缺少 delta"))?;
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => events.extend(
                    output.text_delta(
                        delta
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    ),
                ),
                Some("input_json_delta") => events.extend(output.tool_delta(
                    index,
                    None,
                    None,
                    delta.get("partial_json").and_then(Value::as_str),
                    None,
                )?),
                Some("thinking_delta" | "signature_delta" | "citations_delta") => {}
                Some(other) => anyhow::bail!("不支持的 Anthropic content delta 类型 {other}"),
                None => anyhow::bail!("Anthropic content delta 缺少 type"),
            }
        }
        "ping" | "content_block_stop" | "message_delta" | "message_stop" => {}
        "error" => anyhow::bail!("Anthropic SSE 返回错误"),
        other => anyhow::bail!("不支持的 Anthropic SSE 事件类型 {other}"),
    }
    output.write_events(downstream, events).await
}

fn anthropic_stream_block_start(
    output: &mut ResponsesSseState<'_>,
    index: usize,
    block: &Value,
) -> Result<Vec<Value>> {
    let block_type = block
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Anthropic content block 缺少 type"))?;
    match block_type {
        "text" => Ok(output.text_delta(
            block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )),
        "refusal" => Ok(output.refusal_delta(
            block
                .get("refusal")
                .or_else(|| block.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )),
        "tool_use" => {
            let fallback_arguments = block
                .get("input")
                .map(serde_json::to_string)
                .transpose()
                .context("序列化 Anthropic tool_use.input 失败")?;
            output.tool_delta(
                index,
                block.get("id").and_then(Value::as_str),
                block.get("name").and_then(Value::as_str),
                None,
                fallback_arguments,
            )
        }
        "thinking" | "redacted_thinking" => Ok(Vec::new()),
        other => anyhow::bail!("不支持的 Anthropic content block 类型 {other}"),
    }
}

fn ensure_sse_buffer_within_limit(buffer: &[u8], cursor: usize) -> Result<()> {
    if buffer.len().saturating_sub(cursor) > MAX_UPSTREAM_SSE_BUFFER_BYTES {
        anyhow::bail!("上游 SSE 单帧超过 Codey 安全上限");
    }
    Ok(())
}

fn streaming_failure_message(error: &anyhow::Error, route: &RouteTarget) -> (&'static str, String) {
    if error.downcast_ref::<UpstreamReadIdleTimeout>().is_some() {
        (
            "upstream_idle_timeout",
            format!(
                "线路「{}」的上游流长时间没有返回新数据",
                route_display_name(route)
            ),
        )
    } else {
        (
            "upstream_stream_error",
            format!(
                "线路「{}」返回了无法继续处理的流式响应",
                route_display_name(route)
            ),
        )
    }
}

fn chat_message_content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                part.as_object().and_then(|object| {
                    object
                        .get("text")
                        .or_else(|| object.get("content"))
                        .and_then(Value::as_str)
                })
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn chat_message_annotations(message: &serde_json::Map<String, Value>) -> Value {
    message
        .get("annotations")
        .filter(|annotations| annotations.is_array())
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()))
}

#[cfg(test)]
fn chat_completion_to_responses_body(chat: Value, model: &str) -> Result<Value> {
    let tool_bridge = ResponsesToolBridge::default();
    chat_completion_to_responses_body_with_tool_bridge(chat, model, &tool_bridge)
}

fn chat_completion_to_responses_body_with_tool_bridge(
    mut chat: Value,
    model: &str,
    tool_bridge: &ResponsesToolBridge,
) -> Result<Value> {
    let response_id = chat
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("resp_codey_{}", Uuid::new_v4()));
    let created_at = chat
        .get("created")
        .and_then(Value::as_i64)
        .unwrap_or_else(current_unix_timestamp);
    let choice = chat
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("Chat Completions 响应缺少 choices[0]"))?;
    let message = choice
        .get("message")
        .or_else(|| choice.get("delta"))
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("Chat Completions 响应缺少 assistant message"))?;
    let text = message
        .get("content")
        .map(chat_message_content_text)
        .unwrap_or_default();
    let refusal = message
        .get("refusal")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let annotations = chat_message_annotations(message);
    let mut output = Vec::new();
    if !text.is_empty() || !refusal.is_empty() {
        let mut content = Vec::new();
        if !text.is_empty() {
            content.push(json!({
                "type": "output_text",
                "text": text,
                "annotations": annotations
            }));
        }
        if !refusal.is_empty() {
            content.push(json!({"type":"refusal","refusal":refusal}));
        }
        output.push(json!({
            "id": format!("msg_codey_{}", Uuid::new_v4()),
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": content,
        }));
    }
    if let Some(tool_calls) = message.get("tool_calls") {
        append_chat_tool_calls_to_responses_output(tool_calls, &mut output, tool_bridge)?;
    }
    if let Some(function_call) = message.get("function_call") {
        append_legacy_chat_function_call_to_responses_output(
            function_call,
            &mut output,
            tool_bridge,
        )?;
    }
    if output.is_empty() {
        output.push(json!({
            "id": format!("msg_codey_{}", Uuid::new_v4()),
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": "",
                "annotations": []
            }]
        }));
    }
    let finish_reason = choice.get("finish_reason").and_then(Value::as_str);
    let incomplete_reason = match finish_reason {
        Some("length") => Some("max_output_tokens"),
        Some("content_filter") => Some("content_filter"),
        _ => None,
    };
    let status = if incomplete_reason.is_some() {
        "incomplete"
    } else {
        "completed"
    };
    let mut response = json!({
        "id": response_id,
        "object": "response",
        "created_at": created_at,
        "status": status,
        "model": model,
        "output": output,
        "output_text": text,
        "error": Value::Null,
        "incomplete_details": incomplete_reason.map(|reason| json!({"reason":reason})),
    });
    if let Some(usage) = chat
        .as_object_mut()
        .and_then(|object| object.remove("usage"))
    {
        response
            .as_object_mut()
            .expect("Responses wrapper must be an object")
            .insert("usage".to_string(), chat_usage_to_responses_usage(&usage));
    }
    Ok(response)
}

fn append_chat_tool_calls_to_responses_output(
    tool_calls: &Value,
    output: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    let tool_calls = tool_calls
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Chat message.tool_calls 必须是数组"))?;
    for tool_call in tool_calls {
        let object = tool_call
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Chat tool_call 必须是对象"))?;
        let call_type = object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("function");
        if call_type != "function" {
            anyhow::bail!("Chat tool_call 类型 {call_type} 不能转换为 Responses item");
        }
        let function = object
            .get("function")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("Chat tool_call 缺少 function"))?;
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Chat tool_call.function 缺少 name"))?;
        let arguments = json_value_as_chat_string(function.get("arguments"))
            .unwrap_or_else(|| "{}".to_string());
        let call_id = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("call_codey_{}", Uuid::new_v4()));
        let tool_name = tool_bridge.restore_upstream_name(name)?;
        output.push(responses_tool_call_item_from_upstream_arguments(
            &tool_name,
            call_id,
            arguments,
            "completed",
            "Chat custom tool_call.function.arguments",
        )?);
    }
    Ok(())
}

fn append_legacy_chat_function_call_to_responses_output(
    function_call: &Value,
    output: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    let function = function_call
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Chat message.function_call 必须是对象"))?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Chat message.function_call 缺少 name"))?;
    let tool_name = tool_bridge.restore_upstream_name(name)?;
    output.push(responses_tool_call_item_from_upstream_arguments(
        &tool_name,
        format!("call_codey_{}", Uuid::new_v4()),
        json_value_as_chat_string(function.get("arguments")).unwrap_or_else(|| "{}".to_string()),
        "completed",
        "Chat legacy custom function_call.arguments",
    )?);
    Ok(())
}

fn chat_usage_to_responses_usage(usage: &Value) -> Value {
    let input_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(input_tokens.saturating_add(output_tokens));
    let cached_tokens = usage
        .get("prompt_tokens_details")
        .or_else(|| usage.get("input_tokens_details"))
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let reasoning_tokens = usage
        .get("completion_tokens_details")
        .or_else(|| usage.get("output_tokens_details"))
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "input_tokens": input_tokens,
        "input_tokens_details": {"cached_tokens": cached_tokens},
        "output_tokens": output_tokens,
        "output_tokens_details": {"reasoning_tokens": reasoning_tokens},
        "total_tokens": total_tokens,
    })
}

#[derive(Debug, Default)]
struct ChatSseToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Debug)]
struct ChatSseAccumulator {
    id: String,
    created: i64,
    model: String,
    content: String,
    refusal: String,
    tool_calls: BTreeMap<usize, ChatSseToolCall>,
    finish_reason: Option<String>,
    usage: Option<Value>,
}

impl ChatSseAccumulator {
    fn new(model: &str) -> Self {
        Self {
            id: format!("chatcmpl_codey_{}", Uuid::new_v4()),
            created: current_unix_timestamp(),
            model: model.to_string(),
            content: String::new(),
            refusal: String::new(),
            tool_calls: BTreeMap::new(),
            finish_reason: None,
            usage: None,
        }
    }

    fn ingest(&mut self, chunk: &Value) -> Result<()> {
        if let Some(error) = chunk.get("error") {
            anyhow::bail!("Chat Completions 流返回错误：{error}");
        }
        if let Some(id) = chunk
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            self.id = id.to_string();
        }
        if let Some(created) = chunk.get("created").and_then(Value::as_i64) {
            self.created = created;
        }
        if let Some(model) = chunk
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty())
        {
            self.model = model.to_string();
        }
        if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
            self.usage = Some(usage.clone());
        }
        let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
            return Ok(());
        };
        for choice in choices {
            if choice.get("index").and_then(Value::as_u64).unwrap_or(0) != 0 {
                continue;
            }
            if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str) {
                self.finish_reason = Some(finish_reason.to_string());
            }
            let Some(delta) = choice
                .get("delta")
                .or_else(|| choice.get("message"))
                .and_then(Value::as_object)
            else {
                continue;
            };
            if let Some(content) = delta.get("content") {
                self.content.push_str(&chat_message_content_text(content));
            }
            if let Some(refusal) = delta.get("refusal").and_then(Value::as_str) {
                self.refusal.push_str(refusal);
            }
            if let Some(tool_calls) = delta
                .get("tool_calls")
                .filter(|tool_calls| !tool_calls.is_null())
            {
                self.ingest_tool_calls(tool_calls)?;
            }
            if let Some(function_call) = delta.get("function_call") {
                self.ingest_legacy_function_call(function_call)?;
            }
        }
        Ok(())
    }

    fn ingest_tool_calls(&mut self, tool_calls: &Value) -> Result<()> {
        let tool_calls = tool_calls
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Chat stream delta.tool_calls 必须是数组"))?;
        for tool_call in tool_calls {
            let object = tool_call
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("Chat stream tool_call delta 必须是对象"))?;
            let index = object.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let state = self.tool_calls.entry(index).or_default();
            if let Some(id) = object
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            {
                state.id = id.to_string();
            }
            if let Some(call_type) = object.get("type").and_then(Value::as_str)
                && call_type != "function"
            {
                anyhow::bail!("Chat stream tool_call 类型 {call_type} 不受支持");
            }
            if let Some(function) = object.get("function").and_then(Value::as_object) {
                if let Some(name_delta) = function.get("name").and_then(Value::as_str) {
                    state.name.push_str(name_delta);
                }
                if let Some(arguments_delta) = function.get("arguments") {
                    state.arguments.push_str(
                        &json_value_as_chat_string(Some(arguments_delta)).unwrap_or_default(),
                    );
                }
            }
        }
        Ok(())
    }

    fn ingest_legacy_function_call(&mut self, function_call: &Value) -> Result<()> {
        let function = function_call
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Chat stream function_call delta 必须是对象"))?;
        let state = self.tool_calls.entry(0).or_default();
        if let Some(name_delta) = function.get("name").and_then(Value::as_str) {
            state.name.push_str(name_delta);
        }
        if let Some(arguments_delta) = function.get("arguments") {
            state
                .arguments
                .push_str(&json_value_as_chat_string(Some(arguments_delta)).unwrap_or_default());
        }
        Ok(())
    }

    fn into_chat_completion(self, done: bool) -> Result<Value> {
        if !done
            && self
                .finish_reason
                .as_deref()
                .is_none_or(|reason| reason.trim().is_empty())
        {
            anyhow::bail!("Chat Completions SSE 在 [DONE] 或 finish_reason 前断开");
        }
        let mut message = serde_json::Map::from_iter([(
            "role".to_string(),
            Value::String("assistant".to_string()),
        )]);
        if !self.content.is_empty() {
            message.insert("content".to_string(), Value::String(self.content));
        } else {
            message.insert("content".to_string(), Value::Null);
        }
        if !self.refusal.is_empty() {
            message.insert("refusal".to_string(), Value::String(self.refusal));
        }
        if !self.tool_calls.is_empty() {
            let tool_calls = self
                .tool_calls
                .into_values()
                .map(|tool_call| {
                    if tool_call.name.is_empty() {
                        anyhow::bail!("Chat stream tool_call 缺少 function.name");
                    }
                    Ok(json!({
                        "id": if tool_call.id.is_empty() {
                            format!("call_codey_{}", Uuid::new_v4())
                        } else {
                            tool_call.id
                        },
                        "type": "function",
                        "function": {
                            "name": tool_call.name,
                            "arguments": tool_call.arguments,
                        }
                    }))
                })
                .collect::<Result<Vec<_>>>()?;
            message.insert("tool_calls".to_string(), Value::Array(tool_calls));
        }
        let mut chat = json!({
            "id": self.id,
            "object": "chat.completion",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "message": Value::Object(message),
                "finish_reason": self.finish_reason,
            }],
        });
        if let Some(usage) = self.usage {
            chat.as_object_mut()
                .expect("Chat completion wrapper must be an object")
                .insert("usage".to_string(), usage);
        }
        Ok(chat)
    }
}

async fn collect_chat_completion_sse(
    prepared: &mut PreparedUpstreamResponse,
    model: &str,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<Value> {
    let mut accumulator = ChatSseAccumulator::new(model);
    let mut buffer = Vec::new();
    let mut cursor = SseCursor::default();
    let mut done = false;
    while let Some(chunk) =
        read_prepared_upstream_chunk(prepared, "读取 Chat Completions SSE 流失败", probe).await?
    {
        compact_sse_buffer(&mut buffer, &mut cursor);
        buffer.extend_from_slice(&chunk);
        ensure_sse_buffer_within_limit(&buffer, cursor.consumed)?;
        while let Some(frame) = take_next_sse_frame(&buffer, &mut cursor) {
            let Some(data) = sse_frame_data(frame)? else {
                continue;
            };
            if data.trim() == "[DONE]" {
                done = true;
                break;
            }
            accumulator.ingest(
                &serde_json::from_str::<Value>(&data)
                    .context("Chat Completions SSE data 不是有效 JSON")?,
            )?;
        }
        if done {
            break;
        }
    }
    if !done
        && !buffer[cursor.consumed..]
            .iter()
            .all(u8::is_ascii_whitespace)
        && let Some(data) = sse_frame_data(&buffer[cursor.consumed..])?
    {
        done = data.trim() == "[DONE]";
        if !done {
            accumulator.ingest(
                &serde_json::from_str::<Value>(&data)
                    .context("Chat Completions SSE 末尾 data 不是有效 JSON")?,
            )?;
        }
    }
    accumulator.into_chat_completion(done)
}

async fn stream_chat_completions_as_responses<D>(
    downstream: &mut D,
    mut prepared: PreparedUpstreamResponse,
    model: &str,
    route: &RouteTarget,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()>
where
    D: ResponsesDownstream + ?Sized,
{
    let mut output = ResponsesSseState::new(model, tool_bridge);
    output.start(downstream).await?;
    let mut accumulator = ChatSseAccumulator::new(model);
    let request_log_probe = downstream.request_log_probe().cloned();
    let result: Result<()> = async {
        let mut buffer = Vec::new();
        let mut cursor = SseCursor::default();
        let mut done = false;
        while let Some(chunk) = await_upstream(
            downstream,
            read_prepared_upstream_chunk(
                &mut prepared,
                "读取 Chat Completions SSE 流失败",
                request_log_probe.as_ref(),
            ),
        )
        .await??
        {
            compact_sse_buffer(&mut buffer, &mut cursor);
            buffer.extend_from_slice(&chunk);
            ensure_sse_buffer_within_limit(&buffer, cursor.consumed)?;
            while let Some(frame) = take_next_sse_frame(&buffer, &mut cursor) {
                let Some(data) = sse_frame_data(frame)? else {
                    continue;
                };
                if data.trim() == "[DONE]" {
                    done = true;
                    break;
                }
                let event = serde_json::from_str::<Value>(&data)
                    .context("Chat Completions SSE data 不是有效 JSON")?;
                accumulator.ingest(&event)?;
                emit_chat_stream_event(&mut output, downstream, &event).await?;
            }
            if done {
                break;
            }
        }
        if !done
            && !buffer[cursor.consumed..]
                .iter()
                .all(u8::is_ascii_whitespace)
            && let Some(data) = sse_frame_data(&buffer[cursor.consumed..])?
        {
            done = data.trim() == "[DONE]";
            if !done {
                let event = serde_json::from_str::<Value>(&data)
                    .context("Chat Completions SSE 末尾 data 不是有效 JSON")?;
                accumulator.ingest(&event)?;
                emit_chat_stream_event(&mut output, downstream, &event).await?;
            }
        }
        // The final conversion is needed for tool validation, not visible text.
        // Release duplicate text before building its temporary response objects.
        accumulator.content = String::new();
        accumulator.refusal = String::new();
        let chat = accumulator.into_chat_completion(done)?;
        let completed =
            chat_completion_to_responses_body_with_tool_bridge(chat, model, tool_bridge)?;
        if output.output_order.is_empty() {
            let events = output.ensure_message();
            output.write_events(downstream, events).await?;
        }
        let usage = completed.get("usage").cloned();
        let incomplete_reason = completed
            .get("incomplete_details")
            .and_then(|details| details.get("reason"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        drop(completed);
        output
            .finish(downstream, usage, incomplete_reason.as_deref())
            .await
    }
    .await;
    if let Err(error) = result {
        if error.is::<DownstreamClosed>() {
            return Err(error);
        }
        let (code, message) = streaming_failure_message(&error, route);
        let _ = output.fail(downstream, code, &message).await;
        return Err(error);
    }
    Ok(())
}

async fn emit_chat_stream_event<D>(
    output: &mut ResponsesSseState<'_>,
    downstream: &mut D,
    event: &Value,
) -> Result<()>
where
    D: ResponsesDownstream + ?Sized,
{
    let mut events = Vec::new();
    let Some(choices) = event.get("choices").and_then(Value::as_array) else {
        return Ok(());
    };
    for choice in choices {
        if choice.get("index").and_then(Value::as_u64).unwrap_or(0) != 0 {
            continue;
        }
        let Some(delta) = choice
            .get("delta")
            .or_else(|| choice.get("message"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        if let Some(content) = delta.get("content") {
            events.extend(output.text_delta(&chat_message_content_text(content)));
        }
        if let Some(refusal) = delta.get("refusal").and_then(Value::as_str) {
            events.extend(output.refusal_delta(refusal));
        }
        if let Some(tool_calls) = delta
            .get("tool_calls")
            .filter(|tool_calls| !tool_calls.is_null())
        {
            let tool_calls = tool_calls
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Chat stream delta.tool_calls 必须是数组"))?;
            for tool_call in tool_calls {
                let tool_call = tool_call
                    .as_object()
                    .ok_or_else(|| anyhow::anyhow!("Chat stream tool_call delta 必须是对象"))?;
                if let Some(call_type) = tool_call.get("type").and_then(Value::as_str)
                    && call_type != "function"
                {
                    anyhow::bail!("Chat stream tool_call 类型 {call_type} 不受支持");
                }
                let index = tool_call
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .unwrap_or(0);
                let function = tool_call.get("function").and_then(Value::as_object);
                let arguments_delta = function
                    .and_then(|function| function.get("arguments"))
                    .and_then(|arguments| json_value_as_chat_string(Some(arguments)));
                events.extend(
                    output.tool_delta(
                        index,
                        tool_call.get("id").and_then(Value::as_str),
                        function
                            .and_then(|function| function.get("name"))
                            .and_then(Value::as_str),
                        arguments_delta.as_deref(),
                        None,
                    )?,
                );
            }
        }
        if let Some(function_call) = delta.get("function_call") {
            let function_call = function_call
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("Chat stream function_call delta 必须是对象"))?;
            let arguments_delta = function_call
                .get("arguments")
                .and_then(|arguments| json_value_as_chat_string(Some(arguments)));
            events.extend(output.tool_delta(
                usize::MAX,
                None,
                function_call.get("name").and_then(Value::as_str),
                arguments_delta.as_deref(),
                None,
            )?);
        }
    }
    output.write_events(downstream, events).await
}

fn parse_chat_completion_sse_bytes(bytes: &[u8], model: &str) -> Result<Value> {
    let mut accumulator = ChatSseAccumulator::new(model);
    let mut cursor = SseCursor::default();
    let mut done = false;
    while let Some(frame) = take_next_sse_frame(bytes, &mut cursor) {
        let Some(data) = sse_frame_data(frame)? else {
            continue;
        };
        if data.trim() == "[DONE]" {
            return accumulator.into_chat_completion(true);
        }
        accumulator.ingest(
            &serde_json::from_str::<Value>(&data)
                .context("Chat Completions SSE data 不是有效 JSON")?,
        )?;
    }
    if !bytes[cursor.consumed..].iter().all(u8::is_ascii_whitespace)
        && let Some(data) = sse_frame_data(&bytes[cursor.consumed..])?
    {
        done = data.trim() == "[DONE]";
        if !done {
            accumulator.ingest(
                &serde_json::from_str::<Value>(&data)
                    .context("Chat Completions SSE 末尾 data 不是有效 JSON")?,
            )?;
        }
    }
    accumulator.into_chat_completion(done)
}

#[derive(Default)]
struct SseCursor {
    consumed: usize,
    scanned: usize,
}

fn take_next_sse_frame<'a>(buffer: &'a [u8], cursor: &mut SseCursor) -> Option<&'a [u8]> {
    for index in cursor.scanned..buffer.len() {
        let length = if buffer.get(index..index + 4) == Some(b"\r\n\r\n") {
            4
        } else if buffer.get(index..index + 2) == Some(b"\n\n") {
            2
        } else {
            continue;
        };
        let frame = &buffer[cursor.consumed..index];
        cursor.consumed = index + length;
        cursor.scanned = cursor.consumed;
        return Some(frame);
    }
    // Revisit only the suffix that can begin a delimiter split across chunks.
    cursor.scanned = buffer.len().saturating_sub(3).max(cursor.consumed);
    None
}

fn compact_sse_buffer(buffer: &mut Vec<u8>, cursor: &mut SseCursor) {
    if cursor.consumed == 0 {
        return;
    }
    if cursor.consumed == buffer.len() {
        buffer.clear();
        *cursor = SseCursor::default();
        return;
    }
    if cursor.consumed >= 64 * 1024 || cursor.consumed.saturating_mul(2) >= buffer.len() {
        buffer.drain(..cursor.consumed);
        cursor.scanned -= cursor.consumed;
        cursor.consumed = 0;
    }
}

fn sse_frame_data(frame: &[u8]) -> Result<Option<Cow<'_, str>>> {
    let frame = std::str::from_utf8(frame).context("上游 SSE 不是 UTF-8")?;
    let mut data = frame
        .lines()
        .filter_map(|line| line.trim_end_matches('\r').strip_prefix("data:"))
        .map(str::trim_start);
    let Some(first) = data.next() else {
        return Ok(None);
    };
    let Some(second) = data.next() else {
        return Ok(Some(Cow::Borrowed(first)));
    };
    let mut joined = String::with_capacity(first.len() + second.len() + 1);
    joined.push_str(first);
    joined.push('\n');
    joined.push_str(second);
    for line in data {
        joined.push('\n');
        joined.push_str(line);
    }
    Ok(Some(Cow::Owned(joined)))
}

fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[derive(Clone, Copy, Debug)]
enum StreamOutputKind {
    Message,
    Tool(usize),
}

#[derive(Debug)]
struct ResponsesStreamMessage {
    item_id: String,
    output_index: usize,
    text: String,
    refusal: String,
    text_content_index: Option<usize>,
    refusal_content_index: Option<usize>,
    next_content_index: usize,
}

#[derive(Debug)]
struct ResponsesStreamTool {
    item_id: String,
    output_index: usize,
    call_id: String,
    name: String,
    response_name: Option<ResponsesToolName>,
    arguments: String,
    response_input: Option<String>,
    emitted_arguments: usize,
    fallback_arguments: Option<String>,
    added: bool,
}

#[derive(Debug)]
struct ResponsesSseState<'a> {
    response_id: String,
    model: String,
    tool_bridge: &'a ResponsesToolBridge,
    created_at: i64,
    next_output_index: usize,
    message: Option<ResponsesStreamMessage>,
    tools: BTreeMap<usize, ResponsesStreamTool>,
    output_order: Vec<StreamOutputKind>,
    terminal_started: bool,
}

impl<'a> ResponsesSseState<'a> {
    fn new(model: &str, tool_bridge: &'a ResponsesToolBridge) -> Self {
        Self {
            response_id: format!("resp_codey_{}", Uuid::new_v4()),
            model: model.to_string(),
            tool_bridge,
            created_at: current_unix_timestamp(),
            next_output_index: 0,
            message: None,
            tools: BTreeMap::new(),
            output_order: Vec::new(),
            terminal_started: false,
        }
    }

    async fn start<D>(&self, downstream: &mut D) -> Result<()>
    where
        D: ResponsesDownstream + ?Sized,
    {
        downstream.start_event_stream().await?;
        downstream
            .write_event(&json!({
                "type":"response.created",
                "response":{
                    "id":self.response_id,
                    "object":"response",
                    "created_at":self.created_at,
                    "status":"in_progress",
                    "model":self.model,
                    "output":[],
                    "error":Value::Null,
                    "incomplete_details":Value::Null,
                }
            }))
            .await
    }

    fn ensure_message(&mut self) -> Vec<Value> {
        if self.message.is_some() {
            return Vec::new();
        }
        let output_index = self.next_output_index;
        self.next_output_index += 1;
        let item_id = format!("msg_codey_{}", Uuid::new_v4());
        self.message = Some(ResponsesStreamMessage {
            item_id: item_id.clone(),
            output_index,
            text: String::new(),
            refusal: String::new(),
            text_content_index: None,
            refusal_content_index: None,
            next_content_index: 0,
        });
        self.output_order.push(StreamOutputKind::Message);
        vec![json!({
            "type":"response.output_item.added",
            "response_id":self.response_id,
            "output_index":output_index,
            "item":{
                "id":item_id,
                "type":"message",
                "status":"in_progress",
                "role":"assistant",
                "content":[],
            }
        })]
    }

    fn text_delta(&mut self, delta: &str) -> Vec<Value> {
        if delta.is_empty() {
            return Vec::new();
        }
        let mut events = self.ensure_message();
        let response_id = self.response_id.clone();
        let message = self
            .message
            .as_mut()
            .expect("message state must exist after ensure_message");
        let content_index = *message.text_content_index.get_or_insert_with(|| {
            let index = message.next_content_index;
            message.next_content_index += 1;
            index
        });
        if message.text.is_empty() {
            events.push(json!({
                "type":"response.content_part.added",
                "response_id":response_id,
                "item_id":message.item_id,
                "output_index":message.output_index,
                "content_index":content_index,
                "part":{"type":"output_text","text":"","annotations":[]},
            }));
        }
        message.text.push_str(delta);
        events.push(json!({
            "type":"response.output_text.delta",
            "response_id":response_id,
            "item_id":message.item_id,
            "output_index":message.output_index,
            "content_index":content_index,
            "delta":delta,
        }));
        events
    }

    fn refusal_delta(&mut self, delta: &str) -> Vec<Value> {
        if delta.is_empty() {
            return Vec::new();
        }
        let mut events = self.ensure_message();
        let response_id = self.response_id.clone();
        let message = self
            .message
            .as_mut()
            .expect("message state must exist after ensure_message");
        let content_index = *message.refusal_content_index.get_or_insert_with(|| {
            let index = message.next_content_index;
            message.next_content_index += 1;
            index
        });
        if message.refusal.is_empty() {
            events.push(json!({
                "type":"response.content_part.added",
                "response_id":response_id,
                "item_id":message.item_id,
                "output_index":message.output_index,
                "content_index":content_index,
                "part":{"type":"refusal","refusal":""},
            }));
        }
        message.refusal.push_str(delta);
        events.push(json!({
            "type":"response.refusal.delta",
            "response_id":response_id,
            "item_id":message.item_id,
            "output_index":message.output_index,
            "content_index":content_index,
            "delta":delta,
        }));
        events
    }

    fn tool_delta(
        &mut self,
        upstream_index: usize,
        call_id: Option<&str>,
        name_delta: Option<&str>,
        arguments_delta: Option<&str>,
        fallback_arguments: Option<String>,
    ) -> Result<Vec<Value>> {
        if !self.tools.contains_key(&upstream_index) {
            let output_index = self.next_output_index;
            self.next_output_index += 1;
            self.tools.insert(
                upstream_index,
                ResponsesStreamTool {
                    item_id: String::new(),
                    output_index,
                    call_id: String::new(),
                    name: String::new(),
                    response_name: None,
                    arguments: String::new(),
                    response_input: None,
                    emitted_arguments: 0,
                    fallback_arguments: None,
                    added: false,
                },
            );
            self.output_order
                .push(StreamOutputKind::Tool(upstream_index));
        }
        let response_id = self.response_id.clone();
        let tool = self
            .tools
            .get_mut(&upstream_index)
            .expect("tool state must exist after insertion");
        if tool.call_id.is_empty()
            && let Some(call_id) = call_id.filter(|value| !value.is_empty())
        {
            tool.call_id = call_id.to_string();
        }
        if let Some(name_delta) = name_delta {
            tool.name.push_str(name_delta);
        }
        if tool.fallback_arguments.is_none() {
            tool.fallback_arguments = fallback_arguments;
        }
        if let Some(arguments_delta) = arguments_delta {
            tool.arguments.push_str(arguments_delta);
        }
        if tool.response_name.is_none()
            && !tool.name.is_empty()
            && let Some(response_name) = self
                .tool_bridge
                .restore_stream_upstream_name(&tool.name, false)?
        {
            tool.response_name = Some(response_name);
        }

        let mut events = Vec::new();
        if !tool.added
            && let Some(response_name) = tool.response_name.as_ref()
        {
            if tool.call_id.is_empty() {
                tool.call_id = format!("call_codey_{}", Uuid::new_v4());
            }
            if tool.item_id.is_empty() {
                tool.item_id = responses_tool_call_item_id(response_name);
            }
            tool.added = true;
            events.push(json!({
                "type":"response.output_item.added",
                "response_id":response_id,
                "output_index":tool.output_index,
                "item":responses_tool_call_item_with_id(
                    response_name,
                    tool.item_id.clone(),
                    tool.call_id.clone(),
                    responses_tool_call_initial_payload(response_name),
                    "in_progress",
                )
            }));
        }
        if tool.added
            && tool
                .response_name
                .as_ref()
                .is_some_and(ResponsesToolName::is_function)
            && tool.emitted_arguments < tool.arguments.len()
        {
            let delta = &tool.arguments[tool.emitted_arguments..];
            events.push(json!({
                "type":"response.function_call_arguments.delta",
                "response_id":response_id,
                "item_id":tool.item_id,
                "output_index":tool.output_index,
                "delta":delta,
            }));
            tool.emitted_arguments = tool.arguments.len();
        }
        Ok(events)
    }

    async fn write_events<D>(&self, downstream: &mut D, events: Vec<Value>) -> Result<()>
    where
        D: ResponsesDownstream + ?Sized,
    {
        for event in events {
            downstream.write_event(&event).await?;
        }
        Ok(())
    }

    async fn finish<D>(
        &mut self,
        downstream: &mut D,
        usage: Option<Value>,
        incomplete_reason: Option<&str>,
    ) -> Result<()>
    where
        D: ResponsesDownstream + ?Sized,
    {
        if self.terminal_started {
            return Ok(());
        }
        let mut events = Vec::new();
        if let Some(message) = self.message.as_ref() {
            if let Some(content_index) = message.text_content_index {
                events.push(json!({
                    "type":"response.output_text.done",
                    "response_id":self.response_id,
                    "item_id":message.item_id,
                    "output_index":message.output_index,
                    "content_index":content_index,
                    "text":message.text,
                }));
                events.push(json!({
                    "type":"response.content_part.done",
                    "response_id":self.response_id,
                    "item_id":message.item_id,
                    "output_index":message.output_index,
                    "content_index":content_index,
                    "part":{"type":"output_text","text":message.text,"annotations":[]},
                }));
            }
            if let Some(content_index) = message.refusal_content_index {
                events.push(json!({
                    "type":"response.refusal.done",
                    "response_id":self.response_id,
                    "item_id":message.item_id,
                    "output_index":message.output_index,
                    "content_index":content_index,
                    "refusal":message.refusal,
                }));
                events.push(json!({
                    "type":"response.content_part.done",
                    "response_id":self.response_id,
                    "item_id":message.item_id,
                    "output_index":message.output_index,
                    "content_index":content_index,
                    "part":{"type":"refusal","refusal":message.refusal},
                }));
            }
            events.push(json!({
                "type":"response.output_item.done",
                "response_id":self.response_id,
                "output_index":message.output_index,
                "item":stream_message_item(message),
            }));
        }

        for tool in self.tools.values_mut() {
            if tool.arguments.is_empty()
                && let Some(fallback) = tool.fallback_arguments.take()
            {
                tool.arguments = fallback;
            }
            if tool.name.is_empty() {
                anyhow::bail!("流式 function_call 缺少 name");
            }
            if tool.response_name.is_none() {
                tool.response_name = self
                    .tool_bridge
                    .restore_stream_upstream_name(&tool.name, true)?;
            }
            if tool.call_id.is_empty() {
                tool.call_id = format!("call_codey_{}", Uuid::new_v4());
            }
            let response_name = tool
                .response_name
                .as_ref()
                .expect("response tool name must be restored before serialization");
            if response_name.is_custom() && tool.response_input.is_none() {
                tool.response_input = Some(custom_tool_input_from_arguments(
                    &tool.arguments,
                    "流式 custom function arguments",
                )?);
            }
            if tool.item_id.is_empty() {
                tool.item_id = responses_tool_call_item_id(response_name);
            }
            if !tool.added {
                tool.added = true;
                events.push(json!({
                    "type":"response.output_item.added",
                    "response_id":self.response_id,
                    "output_index":tool.output_index,
                    "item":responses_tool_call_item_with_id(
                        response_name,
                        tool.item_id.clone(),
                        tool.call_id.clone(),
                        responses_tool_call_initial_payload(response_name),
                        "in_progress",
                    )
                }));
            }
            if response_name.is_custom() {
                let input = tool.response_input.as_deref().unwrap_or_default();
                if !input.is_empty() {
                    events.push(json!({
                        "type":"response.custom_tool_call_input.delta",
                        "response_id":self.response_id,
                        "item_id":tool.item_id,
                        "output_index":tool.output_index,
                        "delta":input,
                    }));
                }
                events.push(json!({
                    "type":"response.custom_tool_call_input.done",
                    "response_id":self.response_id,
                    "item_id":tool.item_id,
                    "output_index":tool.output_index,
                    "input":input,
                }));
            } else if response_name.is_function() {
                if tool.emitted_arguments < tool.arguments.len() {
                    let delta = &tool.arguments[tool.emitted_arguments..];
                    events.push(json!({
                        "type":"response.function_call_arguments.delta",
                        "response_id":self.response_id,
                        "item_id":tool.item_id,
                        "output_index":tool.output_index,
                        "delta":delta,
                    }));
                    tool.emitted_arguments = tool.arguments.len();
                }
                events.push(json!({
                    "type":"response.function_call_arguments.done",
                    "response_id":self.response_id,
                    "item_id":tool.item_id,
                    "output_index":tool.output_index,
                    "arguments":tool.arguments,
                }));
            }
            events.push(json!({
                "type":"response.output_item.done",
                "response_id":self.response_id,
                "output_index":tool.output_index,
                "item":stream_tool_item(tool)?,
            }));
        }

        let output = self
            .output_order
            .iter()
            .filter_map(|kind| match kind {
                StreamOutputKind::Message => self
                    .message
                    .as_ref()
                    .map(|message| Ok(stream_message_item(message))),
                StreamOutputKind::Tool(index) => self.tools.get(index).map(stream_tool_item),
            })
            .collect::<Result<Vec<_>>>()?;
        let status = if incomplete_reason.is_some() {
            "incomplete"
        } else {
            "completed"
        };
        downstream.remember_adapted_response(&self.response_id, &output);
        let mut response = json!({
            "id":self.response_id,
            "object":"response",
            "created_at":self.created_at,
            "status":status,
            "model":self.model,
            "output":output,
            "output_text":self.message.as_ref().map(|message| message.text.as_str()).unwrap_or(""),
            "error":Value::Null,
            "incomplete_details":incomplete_reason.map(|reason| json!({"reason":reason})),
        });
        if let Some(usage) = usage {
            response
                .as_object_mut()
                .expect("streaming Responses wrapper must be an object")
                .insert("usage".to_string(), usage);
        }
        let terminal_type = if incomplete_reason.is_some() {
            "response.incomplete"
        } else {
            "response.completed"
        };
        self.write_events(downstream, events).await?;
        self.terminal_started = true;
        downstream
            .write_event(&json!({"type":terminal_type,"response":response}))
            .await?;
        downstream.finish_event_stream().await
    }

    async fn fail<D>(&mut self, downstream: &mut D, code: &str, message: &str) -> Result<()>
    where
        D: ResponsesDownstream + ?Sized,
    {
        if self.terminal_started {
            return Ok(());
        }
        self.terminal_started = true;
        downstream
            .write_event(&json!({
                "type":"response.failed",
                "response":{
                    "id":self.response_id,
                    "object":"response",
                    "created_at":self.created_at,
                    "status":"failed",
                    "model":self.model,
                    "output":[],
                    "error":{
                        "type":"codey_route_error",
                        "code":code,
                        "message":message,
                    },
                    "incomplete_details":Value::Null,
                }
            }))
            .await?;
        downstream.finish_event_stream().await
    }
}

fn stream_message_item(message: &ResponsesStreamMessage) -> Value {
    let mut content = Vec::new();
    if message.text_content_index.is_some() {
        content.push((
            message.text_content_index.unwrap_or_default(),
            json!({"type":"output_text","text":message.text,"annotations":[]}),
        ));
    }
    if message.refusal_content_index.is_some() {
        content.push((
            message.refusal_content_index.unwrap_or_default(),
            json!({"type":"refusal","refusal":message.refusal}),
        ));
    }
    content.sort_by_key(|(index, _)| *index);
    json!({
        "id":message.item_id,
        "type":"message",
        "status":"completed",
        "role":"assistant",
        "content":content.into_iter().map(|(_, part)| part).collect::<Vec<_>>(),
    })
}

fn responses_tool_call_initial_payload(tool_name: &ResponsesToolName) -> Value {
    if tool_name.is_tool_search() {
        Value::Object(serde_json::Map::new())
    } else {
        Value::String(String::new())
    }
}

fn stream_tool_item(tool: &ResponsesStreamTool) -> Result<Value> {
    let response_name = tool
        .response_name
        .as_ref()
        .expect("stream tool response name must be restored before serialization");
    let payload = if response_name.is_custom() {
        Value::String(tool.response_input.clone().unwrap_or_default())
    } else if response_name.is_tool_search() {
        let arguments = serde_json::from_str::<Value>(&tool.arguments)
            .context("流式 tool_search function arguments 不是有效 JSON")?;
        if !arguments.is_object() {
            anyhow::bail!("流式 tool_search function arguments 必须是 JSON 对象");
        }
        arguments
    } else {
        Value::String(tool.arguments.clone())
    };
    Ok(responses_tool_call_item_with_id(
        response_name,
        tool.item_id.clone(),
        tool.call_id.clone(),
        payload,
        "completed",
    ))
}

async fn write_responses_sse_event(stream: &mut TcpStream, event: &Value) -> Result<()> {
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("response.event");
    let payload = format!(
        "event: {event_type}\ndata: {}\n\n",
        serde_json::to_string(event).context("序列化 Responses SSE 事件失败")?
    );
    write_chunked_frame(stream, payload.as_bytes(), "写入 Responses SSE 事件失败").await
}

async fn finish_chunked_response(stream: &mut TcpStream) -> Result<()> {
    write_all_with_timeout(stream, b"0\r\n\r\n", "结束 Responses SSE 流失败").await
}

fn responses_event_sequence(response: &Value) -> Result<Vec<Value>> {
    let response_id = response
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("resp_codey");
    let mut created = response.clone();
    if let Some(created) = created.as_object_mut() {
        created.insert(
            "status".to_string(),
            Value::String("in_progress".to_string()),
        );
        created.insert("output".to_string(), Value::Array(Vec::new()));
        created.remove("usage");
    }
    let mut events = vec![json!({"type":"response.created","response":created})];
    for (output_index, item) in response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let item_id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("item_codey");
        let mut added_item = item.clone();
        if let Some(added_item) = added_item.as_object_mut() {
            added_item.insert(
                "status".to_string(),
                Value::String("in_progress".to_string()),
            );
            match added_item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    added_item.insert("content".to_string(), Value::Array(Vec::new()));
                }
                Some("function_call") => {
                    added_item.insert("arguments".to_string(), Value::String(String::new()));
                }
                Some("tool_search_call") => {
                    added_item.insert(
                        "arguments".to_string(),
                        Value::Object(serde_json::Map::new()),
                    );
                }
                Some("custom_tool_call") => {
                    added_item.insert("input".to_string(), Value::String(String::new()));
                }
                _ => {}
            }
        }
        events.push(json!({
            "type":"response.output_item.added",
            "response_id": response_id,
            "output_index": output_index,
            "item": added_item,
        }));
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                append_message_sse_events(&mut events, response_id, item_id, output_index, item)
            }
            Some("function_call") => {
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                events.push(json!({
                    "type":"response.function_call_arguments.delta",
                    "response_id": response_id,
                    "item_id": item_id,
                    "output_index": output_index,
                    "delta": arguments,
                }));
                events.push(json!({
                    "type":"response.function_call_arguments.done",
                    "response_id": response_id,
                    "item_id": item_id,
                    "output_index": output_index,
                    "arguments": arguments,
                }));
            }
            Some("custom_tool_call") => {
                let input = item.get("input").and_then(Value::as_str).unwrap_or("");
                if !input.is_empty() {
                    events.push(json!({
                        "type":"response.custom_tool_call_input.delta",
                        "response_id": response_id,
                        "item_id": item_id,
                        "output_index": output_index,
                        "delta": input,
                    }));
                }
                events.push(json!({
                    "type":"response.custom_tool_call_input.done",
                    "response_id": response_id,
                    "item_id": item_id,
                    "output_index": output_index,
                    "input": input,
                }));
            }
            _ => {}
        }
        events.push(json!({
            "type":"response.output_item.done",
            "response_id": response_id,
            "output_index": output_index,
            "item": item,
        }));
    }
    let terminal_event = if response.get("status").and_then(Value::as_str) == Some("incomplete") {
        "response.incomplete"
    } else {
        "response.completed"
    };
    events.push(json!({"type":terminal_event,"response":response}));
    Ok(events)
}

async fn write_responses_response_as_events<D>(downstream: &mut D, response: &Value) -> Result<()>
where
    D: ResponsesDownstream + ?Sized,
{
    downstream.start_event_stream().await?;
    for event in responses_event_sequence(response)? {
        downstream.write_event(&event).await?;
    }
    downstream.finish_event_stream().await
}

fn append_message_sse_events(
    events: &mut Vec<Value>,
    response_id: &str,
    item_id: &str,
    output_index: usize,
    item: &Value,
) {
    for (content_index, part) in item
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let part_type = part.get("type").and_then(Value::as_str);
        let empty_part = match part_type {
            Some("refusal") => json!({"type":"refusal","refusal":""}),
            _ => json!({"type":"output_text","text":"","annotations":[]}),
        };
        events.push(json!({
            "type":"response.content_part.added",
            "response_id": response_id,
            "item_id": item_id,
            "output_index": output_index,
            "content_index": content_index,
            "part": empty_part,
        }));
        if part_type == Some("refusal") {
            let refusal = part.get("refusal").and_then(Value::as_str).unwrap_or("");
            events.push(json!({
                "type":"response.refusal.delta",
                "response_id": response_id,
                "item_id": item_id,
                "output_index": output_index,
                "content_index": content_index,
                "delta": refusal,
            }));
            events.push(json!({
                "type":"response.refusal.done",
                "response_id": response_id,
                "item_id": item_id,
                "output_index": output_index,
                "content_index": content_index,
                "refusal": refusal,
            }));
        } else {
            let text = part.get("text").and_then(Value::as_str).unwrap_or("");
            events.push(json!({
                "type":"response.output_text.delta",
                "response_id": response_id,
                "item_id": item_id,
                "output_index": output_index,
                "content_index": content_index,
                "delta": text,
            }));
            events.push(json!({
                "type":"response.output_text.done",
                "response_id": response_id,
                "item_id": item_id,
                "output_index": output_index,
                "content_index": content_index,
                "text": text,
            }));
        }
        events.push(json!({
            "type":"response.content_part.done",
            "response_id": response_id,
            "item_id": item_id,
            "output_index": output_index,
            "content_index": content_index,
            "part": part,
        }));
    }
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        408 => "Request Timeout",
        409 => "Conflict",
        415 => "Unsupported Media Type",
        404 => "Not Found",
        424 => "Failed Dependency",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "OK",
    }
}

#[cfg(test)]
#[path = "local_router_bench.rs"]
mod latency_bench;

#[cfg(test)]
#[path = "local_router_stability_tests.rs"]
mod stability_tests;

#[cfg(test)]
#[path = "local_router_tail_tests.rs"]
mod tail_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderProfile;

    #[test]
    fn fragmented_sse_preserves_frames_tail_and_scan_progress() {
        for large in [false, true] {
            let first = if large {
                "x".repeat(70 * 1024)
            } else {
                "中文🙂".into()
            };
            let source =
                format!("data: {first}\r\n\r\ndata: second\n\n: heartbeat\r\n\r\ndata: tail");
            for chunk_size in [1, 2, 3, 4, 7, 127, 4096, 65536] {
                let mut buffer = Vec::new();
                let mut cursor = SseCursor::default();
                let mut frames = Vec::new();
                for chunk in source.as_bytes().chunks(chunk_size) {
                    compact_sse_buffer(&mut buffer, &mut cursor);
                    buffer.extend_from_slice(chunk);
                    while let Some(frame) = take_next_sse_frame(&buffer, &mut cursor) {
                        frames.push(frame.to_vec());
                    }
                    assert!(cursor.scanned >= buffer.len().saturating_sub(3));
                    assert!(cursor.scanned >= cursor.consumed);
                }
                assert_eq!(
                    frames,
                    vec![
                        format!("data: {first}").into_bytes(),
                        b"data: second".to_vec(),
                        b": heartbeat".to_vec()
                    ]
                );
                assert_eq!(&buffer[cursor.consumed..], b"data: tail");
                buffer.extend_from_slice(b"\n\n");
                assert_eq!(
                    take_next_sse_frame(&buffer, &mut cursor),
                    Some(b"data: tail".as_slice())
                );
                compact_sse_buffer(&mut buffer, &mut cursor);
                assert!(buffer.is_empty());
                assert_eq!(cursor.scanned, 0);
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn downstream_backpressure_times_out_and_closed_reader_fails() {
        let (mut writer, reader) = tokio::io::duplex(1);
        let start = tokio::time::Instant::now();
        let error = write_all_with_timeout(&mut writer, b"too large", "test write")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("超过写入期限"));
        assert_eq!(start.elapsed(), DOWNSTREAM_WRITE_TIMEOUT);
        drop(reader);
        let error = write_all_with_timeout(&mut writer, b"x", "closed reader")
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<std::io::Error>().is_some());
        assert_eq!(start.elapsed(), DOWNSTREAM_WRITE_TIMEOUT);
    }

    #[tokio::test]
    async fn upstream_body_idle_timeout_is_bounded() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (release, wait) = oneshot::channel::<()>();
        let mock = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_http_request(&mut socket).await.unwrap();
            socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n").await.unwrap();
            let _ = wait.await;
        });
        let mut response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/responses"))
            .send()
            .await
            .unwrap();
        tokio::time::pause();
        let start = tokio::time::Instant::now();
        let error = read_upstream_chunk(&mut response, "idle test", None)
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<UpstreamReadIdleTimeout>().is_some());
        // Tokio timers round deadlines up to the next millisecond.
        assert!(start.elapsed() >= UPSTREAM_READ_IDLE_TIMEOUT);
        assert!(start.elapsed() <= UPSTREAM_READ_IDLE_TIMEOUT + Duration::from_millis(2));
        drop(release);
        mock.await.unwrap();
    }

    #[test]
    fn sse_sniffer_handles_fragmented_and_mislabeled_prefixes() {
        assert_eq!(classify_upstream_sse_prefix(b"d"), None);
        assert_eq!(classify_upstream_sse_prefix(b"data"), None);
        assert_eq!(classify_upstream_sse_prefix(b"data: {}\n\n"), Some(true));
        assert_eq!(
            classify_upstream_sse_prefix(b"\xef\xbb\xbf event: message\n"),
            Some(true)
        );
        assert_eq!(classify_upstream_sse_prefix(br#"{"ok":true}"#), Some(false));
    }

    #[test]
    fn downstream_content_timing_ignores_control_events() {
        assert!(!responses_event_has_user_content(
            &json!({"type":"response.created"})
        ));
        assert!(!responses_event_has_user_content(
            &json!({"type":"response.output_text.delta","delta":""})
        ));
        assert!(responses_event_has_user_content(
            &json!({"type":"response.output_text.delta","delta":"hello"})
        ));
    }

    #[test]
    fn request_log_projector_waits_for_a_nonempty_content_delta() {
        let probe = RouteRequestLogProbe::detached_test_probe();
        let mut projector = RequestLogMetadataProjector::default();
        projector
            .observe(
                br#"data: {"type":"response.created"}\n\ndata: {"type":"response.output_text.delta","delta":""}\n\n"#,
                &probe,
                Instant::now(),
            )
            .unwrap();
        assert!(!probe.downstream_content_observed_for_test());

        for chunk in br#"data: {"delta":"hello","type":"response.output_text.delta"}\n\n"#.chunks(7)
        {
            projector.observe(chunk, &probe, Instant::now()).unwrap();
        }
        assert!(probe.downstream_content_observed_for_test());
    }

    #[test]
    fn custom_tool_bridge_description_is_bounded_without_embedding_the_definition() {
        let tool = json!({
            "type":"custom",
            "name":"apply_patch",
            "description":"d".repeat(MAX_CUSTOM_TOOL_BRIDGE_DESCRIPTION_BYTES * 2),
            "format":{"type":"grammar","syntax":"lark","definition":"start: WORD"},
            "unrelated":"must-not-be-forwarded",
        });
        let description = custom_tool_bridge_description(tool.as_object().unwrap()).unwrap();
        assert!(description.len() <= MAX_CUSTOM_TOOL_BRIDGE_DESCRIPTION_BYTES);
        assert!(description.contains("Codey compatibility bridge"));
        assert!(description.contains("\"syntax\":\"lark\""));
        assert!(!description.contains("must-not-be-forwarded"));
    }

    #[tokio::test]
    async fn large_request_json_is_parsed_off_the_router_worker() {
        let encoded = serde_json::to_vec(&json!({
            "model":"test-model",
            "input":"x".repeat(REQUEST_JSON_OFFLOAD_BYTES),
        }))
        .unwrap();
        let (encoded, parsed) = parse_responses_request_body(encoded).await.unwrap();
        assert!(encoded.len() >= REQUEST_JSON_OFFLOAD_BYTES);
        assert_eq!(parsed.unwrap()["model"], "test-model");
    }

    #[test]
    fn request_log_projector_extracts_usage_after_large_terminal_payload() {
        let probe = RouteRequestLogProbe::detached_test_probe();
        let mut projector = RequestLogMetadataProjector::default();
        let event = format!(
            "data: {{\"type\":\"response.completed\",\"response\":{{\"output\":[{{\"text\":\"{}\"}}],\"usage\":{{\"input_tokens\":11,\"output_tokens\":7,\"total_tokens\":18}}}}}}\n\n",
            "x".repeat(128 * 1024)
        );
        for chunk in event.as_bytes().chunks(997) {
            projector.observe(chunk, &probe, Instant::now()).unwrap();
        }
        let usage = probe.token_usage_for_test();
        assert_eq!(usage.input_tokens, Some(11));
        assert_eq!(usage.output_tokens, Some(7));
        assert_eq!(usage.total_tokens, Some(18));
    }

    #[test]
    fn request_log_projector_skips_large_unknown_usage_values() {
        let probe = RouteRequestLogProbe::detached_test_probe();
        let mut projector = RequestLogMetadataProjector::default();
        let event = format!(
            "{{\"usage\":{{\"padding\":[{{\"nested\":[{}0]}}],\"input_tokens\":11,\"output_tokens\":7,\"input_tokens_details\":{{\"cached_tokens\":3}},\"cache_creation_input_tokens\":2,\"output_tokens_details\":{{\"reasoning_tokens\":4}},\"total_tokens\":18}}}}",
            "0,".repeat(128 * 1024)
        );
        for chunk in event.as_bytes().chunks(997) {
            projector.observe(chunk, &probe, Instant::now()).unwrap();
        }

        let usage = probe.token_usage_for_test();
        assert_eq!(usage.input_tokens, Some(11));
        assert_eq!(usage.output_tokens, Some(7));
        assert_eq!(usage.cached_input_tokens, Some(3));
        assert_eq!(usage.cache_creation_input_tokens, Some(2));
        assert_eq!(usage.reasoning_output_tokens, Some(4));
        assert_eq!(usage.total_tokens, Some(18));
    }

    #[test]
    fn request_log_projector_ignores_large_usage_strings() {
        let probe = RouteRequestLogProbe::detached_test_probe();
        let mut projector = RequestLogMetadataProjector::default();
        let event = format!(
            "{{\"usage\":{{\"padding\":\"{}\",\"input_tokens\":11,\"output_tokens\":7,\"total_tokens\":18}}}}",
            "x".repeat(128 * 1024)
        );
        for chunk in event.as_bytes().chunks(997) {
            projector.observe(chunk, &probe, Instant::now()).unwrap();
        }

        let usage = probe.token_usage_for_test();
        assert_eq!(usage.input_tokens, Some(11));
        assert_eq!(usage.output_tokens, Some(7));
        assert_eq!(usage.total_tokens, Some(18));
    }

    #[test]
    fn request_log_projector_preserves_terminal_status_and_error_metadata() {
        let probe = RouteRequestLogProbe::detached_test_probe();
        let mut projector = RequestLogMetadataProjector::default();
        let event = br#"data: {"type":"response.failed","response":{"status":"failed","output":[],"error":{"code":"quota_exhausted"}}}\n\n"#;
        for chunk in event.chunks(13) {
            projector.observe(chunk, &probe, Instant::now()).unwrap();
        }

        let (status, error_code, unavailable_reason) = probe.projected_metadata_for_test();
        assert_eq!(status.as_deref(), Some("failed"));
        assert_eq!(error_code.as_deref(), Some("quota_exhausted"));
        assert_eq!(unavailable_reason, None);
    }

    #[test]
    fn request_log_response_tap_accepts_one_large_upstream_chunk() {
        let probe = RouteRequestLogProbe::detached_test_probe();
        let (sender, _receiver) =
            mpsc::channel::<RequestLogObservedChunk>(REQUEST_LOG_TAP_QUEUE_CHUNKS);
        let mut tap = RequestLogResponseTap {
            sender: Some(sender),
            queue_budget: Arc::new(Semaphore::new(REQUEST_LOG_TAP_QUEUE_CHUNKS)),
            probe: probe.clone(),
        };
        let chunk = Bytes::from(vec![
            b'x';
            REQUEST_LOG_TAP_CHUNK_BYTES
                * (REQUEST_LOG_TAP_QUEUE_CHUNKS + 1)
        ]);

        tap.observe(&chunk);

        let (_, _, unavailable_reason) = probe.projected_metadata_for_test();
        assert_eq!(unavailable_reason, None);
    }

    #[test]
    fn request_log_response_tap_drops_observation_without_backpressure_when_full() {
        let probe = RouteRequestLogProbe::detached_test_probe();
        let (sender, _receiver) =
            mpsc::channel::<RequestLogObservedChunk>(REQUEST_LOG_TAP_QUEUE_CHUNKS);
        let mut tap = RequestLogResponseTap {
            sender: Some(sender),
            queue_budget: Arc::new(Semaphore::new(REQUEST_LOG_TAP_QUEUE_CHUNKS)),
            probe: probe.clone(),
        };
        let chunk = Bytes::from(vec![b'x'; REQUEST_LOG_TAP_CHUNK_BYTES]);

        for _ in 0..=REQUEST_LOG_TAP_QUEUE_CHUNKS {
            tap.observe(&chunk);
        }
        let (_, _, unavailable_reason) = probe.projected_metadata_for_test();
        assert_eq!(unavailable_reason.as_deref(), Some("observer_queue_full"));
    }

    #[test]
    fn request_log_response_tap_preserves_worker_reason_when_closed() {
        let probe = RouteRequestLogProbe::detached_test_probe();
        probe.mark_usage_unavailable("usage_projection_limit_exceeded");
        let (sender, receiver) = mpsc::channel::<RequestLogObservedChunk>(1);
        drop(receiver);
        let mut tap = RequestLogResponseTap {
            sender: Some(sender),
            queue_budget: Arc::new(Semaphore::new(REQUEST_LOG_TAP_QUEUE_CHUNKS)),
            probe: probe.clone(),
        };

        tap.observe(&Bytes::from_static(b"x"));

        let (_, _, unavailable_reason) = probe.projected_metadata_for_test();
        assert_eq!(
            unavailable_reason.as_deref(),
            Some("usage_projection_limit_exceeded")
        );
    }

    fn client_tool_search_definition() -> Value {
        json!({
            "type":"tool_search",
            "execution":"client",
            "description":"Search the client tool catalog",
            "parameters":{
                "type":"object",
                "properties":{"goal":{"type":"string"}},
                "required":["goal"],
                "additionalProperties":false
            }
        })
    }

    pub(super) fn router_config(base_url: String) -> (CodeyConfig, String, String) {
        let mut route = ProviderProfile::new("Relay");
        route.id = "route-a".into();
        route.base_url = base_url;
        route.api_key = "sk-upstream".into();
        route.normalize();
        let provider_id = route.provider_id().to_string();
        let model = "provider-model".to_string();
        let mut config = CodeyConfig {
            active_profile_id: route.id.clone(),
            profiles: vec![route],
            ..CodeyConfig::default()
        }
        .normalize();
        config
            .selected_models_by_provider
            .insert(provider_id.clone(), vec![model.clone()]);
        (config, provider_id, model)
    }

    #[test]
    fn outbound_proxy_matcher_detects_effective_proxy_for_official_route() {
        let proxied = SystemProxyMatcher::builder()
            .https("http://127.0.0.1:7890")
            .build();
        assert!(outbound_proxy_applies_to_url_with_matcher(
            CHATGPT_CODEX_BASE_URL,
            &proxied
        ));

        let bypassed = SystemProxyMatcher::builder()
            .https("http://127.0.0.1:7890")
            .no("chatgpt.com")
            .build();
        assert!(!outbound_proxy_applies_to_url_with_matcher(
            CHATGPT_CODEX_BASE_URL,
            &bypassed
        ));

        let http_only = SystemProxyMatcher::builder()
            .http("http://127.0.0.1:7890")
            .build();
        assert!(!outbound_proxy_applies_to_url_with_matcher(
            CHATGPT_CODEX_BASE_URL,
            &http_only
        ));
    }

    pub(super) async fn connect_router_websocket(
        endpoint: &RuntimeRouterEndpoint,
    ) -> WebSocketStream<MaybeTlsStream<TcpStream>> {
        connect_router_websocket_with_headers(endpoint, &[]).await
    }

    async fn connect_router_websocket_with_headers(
        endpoint: &RuntimeRouterEndpoint,
        headers: &[(&str, &str)],
    ) -> WebSocketStream<MaybeTlsStream<TcpStream>> {
        let url = format!(
            "{}/responses",
            endpoint.base_url.replacen("http://", "ws://", 1)
        );
        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", endpoint.token)).unwrap(),
        );
        for (name, value) in headers {
            request.headers_mut().insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        connect_async_with_config(request, None, false)
            .await
            .unwrap()
            .0
    }

    pub(super) async fn local_websocket_pair() -> (
        WebSocketStream<TcpStream>,
        WebSocketStream<MaybeTlsStream<TcpStream>>,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = async {
            let (stream, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(stream).await.unwrap()
        };
        let client = async {
            connect_async_with_config(format!("ws://{address}/responses"), None, false)
                .await
                .unwrap()
                .0
        };
        tokio::join!(server, client)
    }

    async fn send_router_websocket_request(
        socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
        model: &str,
        input: &str,
    ) -> Vec<Value> {
        send_router_websocket_request_on_stream(socket, model, input, None).await
    }

    async fn send_router_websocket_request_on_stream(
        socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
        model: &str,
        input: &str,
        stream_id: Option<&str>,
    ) -> Vec<Value> {
        let mut request = json!({
            "type":"response.create",
            "model":model,
            "input":input,
        });
        if let Some(stream_id) = stream_id {
            request
                .as_object_mut()
                .unwrap()
                .insert("stream_id".into(), Value::String(stream_id.into()));
        }
        socket
            .send(WebSocketMessage::Text(
                serde_json::to_string(&request).unwrap().into(),
            ))
            .await
            .unwrap();
        let mut events = Vec::new();
        loop {
            let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let WebSocketMessage::Text(text) = message else {
                continue;
            };
            let event = serde_json::from_str::<Value>(text.as_str()).unwrap();
            let terminal = responses_event_is_terminal(&event);
            events.push(event);
            if terminal {
                return events;
            }
        }
    }

    async fn write_test_http_chunk(stream: &mut TcpStream, payload: &str) {
        stream
            .write_all(format!("{:x}\r\n", payload.len()).as_bytes())
            .await
            .unwrap();
        stream.write_all(payload.as_bytes()).await.unwrap();
        stream.write_all(b"\r\n").await.unwrap();
        stream.flush().await.unwrap();
    }

    #[tokio::test]
    async fn local_router_errors_expose_a_safe_request_id_for_correlation() {
        let (mut reader, mut writer) = tokio::io::duplex(4096);
        ROUTER_REQUEST_ID
            .scope("request-123".to_string(), async {
                write_text_error_response(&mut writer, 504, "upstream_timeout", "上游响应超时")
                    .await
                    .unwrap();
            })
            .await;
        drop(writer);
        let mut response = String::new();
        reader.read_to_string(&mut response).await.unwrap();

        assert!(response.contains("x-codey-request-id: request-123\r\n"));
        assert!(response.contains("请求 ID：request-123"));
    }

    #[test]
    fn runtime_endpoint_validation_allows_http_but_rejects_url_credentials() {
        assert!(responses_endpoint("http://api.example.com/v1").is_ok());
        assert!(responses_endpoint("https://user:pass@api.example.com/v1").is_err());
        assert!(responses_endpoint("http://127.0.0.1:11434/v1").is_ok());
        assert_eq!(
            responses_websocket_endpoint("https://api.example.com/v1").unwrap(),
            "wss://api.example.com/v1/responses"
        );
        assert_eq!(
            responses_websocket_endpoint("http://127.0.0.1:11434/v1").unwrap(),
            "ws://127.0.0.1:11434/v1/responses"
        );
    }

    #[test]
    fn upstream_websocket_handshake_carries_route_auth_and_beta_header() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer route-key"));
        headers.insert(
            HeaderName::from_static(CHATGPT_ACCOUNT_ID_HEADER),
            HeaderValue::from_static("account-test"),
        );
        headers.insert(
            HeaderName::from_static("openai-beta"),
            HeaderValue::from_static("another_feature=v1"),
        );

        let request =
            upstream_websocket_request("wss://relay.example/v1/responses", &headers).unwrap();

        assert_eq!(request.uri().path(), "/v1/responses");
        assert_eq!(request.headers()[AUTHORIZATION], "Bearer route-key");
        assert_eq!(request.headers()[CHATGPT_ACCOUNT_ID_HEADER], "account-test");
        let beta = request.headers()["openai-beta"].to_str().unwrap();
        assert!(beta.contains("another_feature=v1"));
        assert!(beta.contains(RESPONSES_WEBSOCKET_BETA));
    }

    #[tokio::test]
    async fn upstream_websocket_connection_disables_nagle() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = socket.next().await;
        });

        let mut socket = connect_upstream_responses_websocket(
            &format!("ws://{address}/v1/responses"),
            &HeaderMap::new(),
        )
        .await
        .unwrap();
        let MaybeTlsStream::Plain(stream) = socket.get_ref() else {
            panic!("loopback WebSocket must use a plain TCP stream");
        };
        assert!(stream.nodelay().unwrap());

        socket.close(None).await.unwrap();
        server.await.unwrap();
    }

    #[test]
    fn upstream_websocket_sse_wrapper_parser_accepts_complete_and_unterminated_frames() {
        let text = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-sse\"}}\n\n",
            "event: response.output_text.delta\r\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}"
        );

        let events = parse_responses_websocket_sse_events(text).unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["type"], "response.created");
        assert_eq!(events[0]["response"]["id"], "resp-sse");
        assert_eq!(events[1]["type"], "response.output_text.delta");
        assert_eq!(events[1]["delta"], "ok");
    }

    #[allow(clippy::result_large_err)]
    #[tokio::test]
    async fn upstream_websocket_normalizes_sse_wrapped_events_to_json_frames() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                WebSocketMessage::Text(_)
            ));
            for event in [
                json!({
                    "type":"response.created",
                    "response":{
                        "id":"resp-sse-wrapped",
                        "object":"response",
                        "status":"in_progress",
                        "output":[],
                    }
                }),
                json!({
                    "type":"response.completed",
                    "response":{
                        "id":"resp-sse-wrapped",
                        "object":"response",
                        "status":"completed",
                        "output":[],
                    }
                }),
            ] {
                let event_type = event["type"].as_str().unwrap();
                let frame = format!(
                    "event: {event_type}\ndata: {}\n\n",
                    serde_json::to_string(&event).unwrap()
                );
                socket
                    .send(WebSocketMessage::Text(frame.into()))
                    .await
                    .unwrap();
            }
        });

        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].supports_websockets = true;
        let snapshot = RouterSnapshot::from_config(&config);
        let route = Arc::clone(&snapshot.routes[&provider_id]);
        let (downstream_socket, mut downstream_peer) = local_websocket_pair().await;
        let mut downstream = WebSocketResponsesDownstream::new(downstream_socket);
        let mut body = json!({"model":model,"input":"hello"});

        assert_eq!(
            downstream
                .proxy_upstream_websocket(&route, &HeaderMap::new(), &mut body, None)
                .await
                .unwrap(),
            UpstreamWebSocketAttempt::Completed
        );

        let mut received = Vec::new();
        for _ in 0..2 {
            let WebSocketMessage::Text(text) = downstream_peer.next().await.unwrap().unwrap()
            else {
                panic!("expected downstream JSON text frame");
            };
            received.push(serde_json::from_str::<Value>(text.as_str()).unwrap());
        }
        assert_eq!(received[0]["type"], "response.created");
        assert_eq!(received[1]["type"], "response.completed");
        assert!(
            downstream
                .upstream
                .as_ref()
                .unwrap()
                .response_ids
                .contains("resp-sse-wrapped")
        );
        upstream_task.await.unwrap();
    }

    #[tokio::test]
    async fn websocket_http_fallback_parses_sse_with_a_json_content_type() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let first_sse = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-mislabeled\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[]}}\n\n"
        );
        let final_sse = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-mislabeled\",\"object\":\"response\",\"status\":\"completed\",\"output\":[]}}\n\n"
        );
        let (first_event_sent, first_event_observed) = oneshot::channel();
        let (release_upstream, wait_for_release) = oneshot::channel();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            write_test_http_chunk(&mut stream, first_sse).await;
            first_event_sent.send(()).unwrap();
            wait_for_release.await.unwrap();
            write_test_http_chunk(&mut stream, final_sse).await;
            stream.write_all(b"0\r\n\r\n").await.unwrap();
        });
        let response = reqwest::get(format!("http://{upstream_address}/responses"))
            .await
            .unwrap();
        let (downstream_socket, mut downstream_peer) = local_websocket_pair().await;
        let mut downstream = WebSocketResponsesDownstream::new(downstream_socket);
        let proxy_task = tokio::spawn(async move {
            proxy_native_response_to_websocket(&mut downstream, response, None)
                .await
                .unwrap();
        });

        first_event_observed.await.unwrap();
        let WebSocketMessage::Text(text) =
            tokio::time::timeout(Duration::from_secs(2), downstream_peer.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
        else {
            panic!("expected downstream JSON text frame");
        };
        let event = serde_json::from_str::<Value>(text.as_str()).unwrap();
        assert_eq!(event["type"], "response.created");
        assert_eq!(event["response"]["id"], "resp-mislabeled");

        release_upstream.send(()).unwrap();
        let WebSocketMessage::Text(text) = downstream_peer.next().await.unwrap().unwrap() else {
            panic!("expected downstream JSON text frame");
        };
        let event = serde_json::from_str::<Value>(text.as_str()).unwrap();
        assert_eq!(event["type"], "response.completed");
        assert_eq!(event["response"]["id"], "resp-mislabeled");
        proxy_task.await.unwrap();
        upstream_task.await.unwrap();
    }

    #[test]
    fn websocket_error_events_are_terminal_and_connection_limits_force_reconnect() {
        let request_error = json!({
            "type":"error",
            "error":{"code":"invalid_stream_id"},
        });
        assert!(responses_event_is_terminal(&request_error));
        assert!(responses_websocket_connection_is_reusable(&request_error));

        let connection_limit = json!({
            "type":"error",
            "error":{"code":"websocket_connection_limit_reached"},
        });
        assert!(responses_event_is_terminal(&connection_limit));
        assert!(!responses_websocket_connection_is_reusable(
            &connection_limit
        ));
    }

    #[test]
    fn upstream_websocket_liveness_sends_heartbeat_and_expires_missing_pong() {
        let connected_at = Instant::now();
        let mut liveness = UpstreamWebSocketLiveness::new(connected_at);

        assert_eq!(
            liveness.maintenance_deadline(),
            connected_at + UPSTREAM_WEBSOCKET_HEARTBEAT_INTERVAL
        );
        assert_eq!(
            liveness.maintenance_action(
                connected_at + UPSTREAM_WEBSOCKET_HEARTBEAT_INTERVAL - Duration::from_millis(1)
            ),
            UpstreamWebSocketMaintenanceAction::None
        );
        let heartbeat_at = connected_at + UPSTREAM_WEBSOCKET_HEARTBEAT_INTERVAL;
        assert_eq!(
            liveness.maintenance_action(heartbeat_at),
            UpstreamWebSocketMaintenanceAction::SendPing
        );

        liveness.record_heartbeat_sent(heartbeat_at);
        liveness.record_activity(heartbeat_at + Duration::from_secs(1));
        assert_eq!(liveness.heartbeat_sent_at, Some(heartbeat_at));
        assert_eq!(
            liveness.maintenance_deadline(),
            heartbeat_at + UPSTREAM_WEBSOCKET_PONG_TIMEOUT
        );
        assert_eq!(
            liveness.maintenance_action(
                heartbeat_at + UPSTREAM_WEBSOCKET_PONG_TIMEOUT - Duration::from_millis(1)
            ),
            UpstreamWebSocketMaintenanceAction::None
        );
        assert_eq!(
            liveness.maintenance_action(heartbeat_at + UPSTREAM_WEBSOCKET_PONG_TIMEOUT),
            UpstreamWebSocketMaintenanceAction::Drop
        );

        let pong_at = heartbeat_at + Duration::from_secs(1);
        liveness.record_pong(pong_at);
        assert!(liveness.heartbeat_sent_at.is_none());
        assert_eq!(
            liveness.maintenance_deadline(),
            pong_at + UPSTREAM_WEBSOCKET_HEARTBEAT_INTERVAL
        );
        assert_eq!(
            liveness.maintenance_action(connected_at + UPSTREAM_WEBSOCKET_MAX_REUSE_AGE),
            UpstreamWebSocketMaintenanceAction::Drop
        );
    }

    #[test]
    fn upstream_websocket_backoff_is_shared_and_scoped_to_route_and_auth() {
        let now = Instant::now();
        let mut backoffs = UpstreamWebSocketBackoffs::default();
        let key = UpstreamWebSocketBackoffKey::new(
            "route-a",
            "wss://a.example/responses",
            UpstreamWebSocketAuthIdentity::default(),
        );
        let (first_count, first_duration) = backoffs.record_failure(key.clone(), now);
        assert_eq!(first_count, 1);
        assert_eq!(first_duration, Duration::from_secs(60));
        assert!(backoffs.is_backing_off(&key, now));

        let (second_count, second_duration) = backoffs.record_failure(key.clone(), now);
        assert_eq!(second_count, 2);
        assert_eq!(second_duration, Duration::from_secs(5 * 60));

        let (third_count, third_duration) = backoffs.record_failure(key.clone(), now);
        assert_eq!(third_count, 3);
        assert_eq!(third_duration, Duration::from_secs(15 * 60));
        let (fourth_count, fourth_duration) = backoffs.record_failure(key.clone(), now);
        assert_eq!(fourth_count, 4);
        assert_eq!(fourth_duration, Duration::from_secs(15 * 60));

        let changed_url = UpstreamWebSocketBackoffKey::new(
            "route-a",
            "wss://b.example/responses",
            UpstreamWebSocketAuthIdentity::default(),
        );
        assert!(!backoffs.is_backing_off(&changed_url, now));
        let changed_route = UpstreamWebSocketBackoffKey::new(
            "route-b",
            "wss://a.example/responses",
            UpstreamWebSocketAuthIdentity::default(),
        );
        assert!(!backoffs.is_backing_off(&changed_route, now));
        let changed_auth = UpstreamWebSocketBackoffKey::new(
            "route-a",
            "wss://a.example/responses",
            UpstreamWebSocketAuthIdentity {
                authorization: Some([7; 32]),
                account_id: Some([9; 32]),
            },
        );
        assert!(!backoffs.is_backing_off(&changed_auth, now));

        backoffs.record_success(&key);
        assert!(!backoffs.is_backing_off(&key, now));

        backoffs.record_unsupported(key.clone(), now);
        assert!(backoffs.is_backing_off(&key, now + Duration::from_secs(365 * 24 * 60 * 60)));
        backoffs.clear();
        assert!(!backoffs.is_backing_off(&key, now));
    }

    #[test]
    fn only_endpoint_capability_statuses_make_websocket_degradation_permanent() {
        for status in [
            WebSocketStatusCode::NOT_FOUND,
            WebSocketStatusCode::METHOD_NOT_ALLOWED,
            WebSocketStatusCode::GONE,
            WebSocketStatusCode::NOT_IMPLEMENTED,
        ] {
            let response = tokio_tungstenite::tungstenite::http::Response::builder()
                .status(status)
                .body(None::<Vec<u8>>)
                .unwrap();
            let error = anyhow::Error::new(WebSocketError::Http(response))
                .context("wrapped websocket failure");
            assert!(upstream_websocket_endpoint_is_unsupported(&error));
        }
        for status in [
            WebSocketStatusCode::UNAUTHORIZED,
            WebSocketStatusCode::FORBIDDEN,
            WebSocketStatusCode::TOO_MANY_REQUESTS,
            WebSocketStatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let response = tokio_tungstenite::tungstenite::http::Response::builder()
                .status(status)
                .body(None::<Vec<u8>>)
                .unwrap();
            let error = anyhow::Error::new(WebSocketError::Http(response));
            assert!(!upstream_websocket_endpoint_is_unsupported(&error));
        }
    }

    #[tokio::test]
    async fn router_config_update_clears_websocket_negative_cache() {
        let (config, provider_id, _) = router_config("http://127.0.0.1:9/v1".into());
        let router = LocalRouter::start(&config).await.unwrap();
        let now = Instant::now();
        let key = UpstreamWebSocketBackoffKey::new(
            &provider_id,
            "ws://127.0.0.1:9/v1/responses",
            UpstreamWebSocketAuthIdentity::default(),
        );
        router
            .websocket_backoffs
            .lock()
            .unwrap()
            .record_unsupported(key.clone(), now);
        assert!(
            router
                .websocket_backoffs
                .lock()
                .unwrap()
                .is_backing_off(&key, now + Duration::from_secs(24 * 60 * 60))
        );

        router.update_config(&config);

        assert!(
            !router
                .websocket_backoffs
                .lock()
                .unwrap()
                .is_backing_off(&key, now)
        );
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn idle_cached_upstream_heartbeat_confirms_socket_before_next_request() {
        let (downstream_socket, mut downstream_peer) = local_websocket_pair().await;
        let (mut upstream_peer, upstream_socket) = local_websocket_pair().await;
        let heartbeat_due_at = Instant::now() - UPSTREAM_WEBSOCKET_HEARTBEAT_INTERVAL;
        let mut downstream = WebSocketResponsesDownstream::new(downstream_socket);
        downstream.upstream = Some(CachedUpstreamWebSocket {
            route_id: "route-a".to_string(),
            url: "ws://upstream.example/responses".to_string(),
            auth_identity: UpstreamWebSocketAuthIdentity::default(),
            response_ids: HashSet::new(),
            liveness: UpstreamWebSocketLiveness::new(heartbeat_due_at),
            socket: upstream_socket,
        });

        let next_message = tokio::spawn(async move {
            let message = downstream.next_message().await.unwrap();
            (message, downstream)
        });
        let heartbeat = tokio::time::timeout(Duration::from_secs(1), upstream_peer.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let WebSocketMessage::Ping(payload) = heartbeat else {
            panic!("expected an idle upstream Ping");
        };
        upstream_peer
            .send(WebSocketMessage::Pong(payload))
            .await
            .unwrap();
        downstream_peer
            .send(WebSocketMessage::Text("next request".into()))
            .await
            .unwrap();

        let (message, downstream) = tokio::time::timeout(Duration::from_secs(1), next_message)
            .await
            .unwrap()
            .unwrap();
        let Some(WebSocketMessage::Text(text)) = message else {
            panic!("expected the downstream request after Pong");
        };
        assert_eq!(text, "next request");
        let cached = downstream
            .upstream
            .expect("confirmed socket must stay cached");
        assert!(cached.liveness.heartbeat_sent_at.is_none());
    }

    #[tokio::test]
    async fn idle_cached_upstream_pong_timeout_drops_socket_before_next_request() {
        let (downstream_socket, mut downstream_peer) = local_websocket_pair().await;
        let (_upstream_peer, upstream_socket) = local_websocket_pair().await;
        let now = Instant::now();
        let mut liveness = UpstreamWebSocketLiveness::new(now - Duration::from_secs(20));
        liveness.heartbeat_sent_at = Some(now - UPSTREAM_WEBSOCKET_PONG_TIMEOUT);
        let mut downstream = WebSocketResponsesDownstream::new(downstream_socket);
        downstream.upstream = Some(CachedUpstreamWebSocket {
            route_id: "route-a".to_string(),
            url: "ws://upstream.example/responses".to_string(),
            auth_identity: UpstreamWebSocketAuthIdentity::default(),
            response_ids: HashSet::new(),
            liveness,
            socket: upstream_socket,
        });

        downstream_peer
            .send(WebSocketMessage::Text("next request".into()))
            .await
            .unwrap();
        let message = tokio::time::timeout(Duration::from_secs(1), downstream.next_message())
            .await
            .unwrap()
            .unwrap();

        assert!(matches!(message, Some(WebSocketMessage::Text(_))));
        assert!(downstream.upstream.is_none());
    }

    #[test]
    fn router_snapshot_routes_compact_through_each_protocol_endpoint() {
        let (mut config, _, _) = router_config("https://relay.example/v1".into());
        config.profiles[0].supports_websockets = true;
        let responses = RouterSnapshot::from_config(&config);
        assert!(responses.routes["route-a"].supports_websockets);
        assert_eq!(
            responses.routes["route-a"]
                .upstream_compact_url
                .as_ref()
                .unwrap(),
            "https://relay.example/v1/responses/compact"
        );

        config.profiles[0].upstream_protocol =
            crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
        let chat = RouterSnapshot::from_config(&config);
        assert!(!chat.routes["route-a"].supports_websockets);
        assert_eq!(
            chat.routes["route-a"]
                .upstream_compact_url
                .as_ref()
                .unwrap(),
            "https://relay.example/v1/chat/completions"
        );

        config.profiles[0].upstream_protocol =
            crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES.into();
        let anthropic = RouterSnapshot::from_config(&config);
        assert_eq!(
            anthropic.routes["route-a"]
                .upstream_compact_url
                .as_ref()
                .unwrap(),
            "https://relay.example/v1/messages"
        );
    }

    #[allow(clippy::result_large_err)]
    #[tokio::test]
    async fn declared_responses_route_reuses_upstream_websocket() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let handshake = Arc::new(Mutex::new(None));
        let captured_handshake = Arc::clone(&handshake);
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream.accept().await.unwrap();
            let mut socket = accept_hdr_async_with_config(
                stream,
                move |request: &WebSocketRequest, response: WebSocketResponse| {
                    *captured_handshake.lock().unwrap() = Some((
                        request.uri().path().to_string(),
                        request
                            .headers()
                            .get(AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string),
                        request
                            .headers()
                            .get("openai-beta")
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string),
                    ));
                    Ok(response)
                },
                None,
            )
            .await
            .unwrap();
            let mut requests = Vec::new();
            for sequence in 1..=2 {
                let message = socket.next().await.unwrap().unwrap();
                let WebSocketMessage::Text(text) = message else {
                    panic!("expected text response.create");
                };
                let request = serde_json::from_str::<Value>(text.as_str()).unwrap();
                requests.push(request.clone());
                let response = json!({
                    "id":format!("resp-{sequence}"),
                    "object":"response",
                    "status":"completed",
                    "model":request["model"],
                    "output":[],
                });
                socket
                    .send(WebSocketMessage::Text(
                        serde_json::to_string(&json!({
                            "type":"response.created",
                            "response":{
                                "id":format!("resp-{sequence}"),
                                "object":"response",
                                "status":"in_progress",
                                "model":request["model"],
                                "output":[],
                            }
                        }))
                        .unwrap()
                        .into(),
                    ))
                    .await
                    .unwrap();
                socket
                    .send(WebSocketMessage::Text(
                        serde_json::to_string(&json!({
                            "type":"response.completed",
                            "response":response,
                        }))
                        .unwrap()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
            requests
        });

        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].supports_websockets = true;
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        assert!(endpoint.supports_websockets);
        let alias = model_alias(&provider_id, &model);
        let mut socket = connect_router_websocket(&endpoint).await;

        let first =
            send_router_websocket_request_on_stream(&mut socket, &alias, "first", Some("main"))
                .await;
        let second = send_router_websocket_request(&mut socket, &alias, "second").await;
        assert_eq!(first.last().unwrap()["type"], "response.completed");
        assert_eq!(second.last().unwrap()["type"], "response.completed");
        assert!(first.iter().all(|event| event["stream_id"] == "main"));
        assert!(second.iter().all(|event| event.get("stream_id").is_none()));

        socket.close(None).await.unwrap();
        let requests = upstream_task.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["type"], "response.create");
        assert_eq!(requests[0]["model"], model);
        assert!(requests[0].get("stream").is_none());
        assert!(requests[0].get("background").is_none());
        assert!(requests[0].get("stream_id").is_none());
        assert_eq!(requests[1]["input"], "second");
        assert!(requests[1].get("stream_id").is_none());
        let handshake = handshake.lock().unwrap().clone().unwrap();
        assert_eq!(handshake.0, "/v1/responses");
        assert_eq!(handshake.1.as_deref(), Some("Bearer sk-upstream"));
        assert!(
            handshake
                .2
                .as_deref()
                .unwrap_or_default()
                .contains(RESPONSES_WEBSOCKET_BETA)
        );
        router.stop().await.unwrap();
    }

    #[allow(clippy::result_large_err)]
    #[tokio::test]
    async fn subagents_use_isolated_upstream_websockets() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for sequence in 1..=2 {
                let (stream, _) = tokio::time::timeout(Duration::from_secs(1), upstream.accept())
                    .await
                    .expect("each subagent must open its own upstream WebSocket")
                    .unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let WebSocketMessage::Text(text) = socket.next().await.unwrap().unwrap() else {
                    panic!("expected subagent response.create text message");
                };
                let request = serde_json::from_str::<Value>(text.as_str()).unwrap();
                assert_eq!(request["type"], "response.create");
                assert!(request.get("stream").is_none());
                socket
                    .send(WebSocketMessage::Text(
                        serde_json::to_string(&json!({
                            "type":"response.completed",
                            "response":{
                                "id":format!("resp-subagent-{sequence}"),
                                "object":"response",
                                "status":"completed",
                                "model":request["model"],
                                "output":[],
                            }
                        }))
                        .unwrap()
                        .into(),
                    ))
                    .await
                    .unwrap();
                requests.push(request);
            }
            requests
        });

        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].supports_websockets = true;
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let alias = model_alias(&provider_id, &model);

        for (header, value, input) in [
            ("x-openai-subagent", "codey_worker", "first child"),
            ("x-codex-parent-thread-id", "parent-thread", "second child"),
        ] {
            let mut socket =
                connect_router_websocket_with_headers(&endpoint, &[(header, value)]).await;
            let events = send_router_websocket_request(&mut socket, &alias, input).await;
            assert_eq!(events.last().unwrap()["type"], "response.completed");
            socket.close(None).await.unwrap();
        }

        let requests = upstream_task.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["input"], "first child");
        assert_eq!(requests[1]["input"], "second child");
        router.stop().await.unwrap();
    }

    #[allow(clippy::result_large_err)]
    #[tokio::test]
    async fn websocket_request_without_model_uses_configured_default() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let WebSocketMessage::Text(text) = socket.next().await.unwrap().unwrap() else {
                panic!("expected response.create text message");
            };
            let request = serde_json::from_str::<Value>(text.as_str()).unwrap();
            socket
                .send(WebSocketMessage::Text(
                    serde_json::to_string(&json!({
                        "type":"response.completed",
                        "response":{
                            "id":"resp-default-model",
                            "object":"response",
                            "status":"completed",
                            "model":request["model"],
                            "output":[],
                        }
                    }))
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            request
        });

        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].supports_websockets = true;
        config.default_model = model_alias(&provider_id, &model);
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let mut socket = connect_router_websocket(&endpoint).await;
        socket
            .send(WebSocketMessage::Text(
                serde_json::to_string(&json!({
                    "type":"response.create",
                    "input":"resume without an explicit model",
                }))
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();

        let event = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let WebSocketMessage::Text(text) = event else {
            panic!("expected response.completed text message");
        };
        let event = serde_json::from_str::<Value>(text.as_str()).unwrap();
        assert_eq!(event["type"], "response.completed");

        socket.close(None).await.unwrap();
        let request = upstream_task.await.unwrap();
        assert_eq!(request["model"], model);
        assert_eq!(request["input"], "resume without an explicit model");
        router.stop().await.unwrap();
    }

    #[allow(clippy::result_large_err)]
    #[tokio::test]
    async fn upstream_websocket_reconnects_when_account_identity_changes() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let captured_accounts = Arc::new(Mutex::new(Vec::new()));
        let server_accounts = Arc::clone(&captured_accounts);
        let upstream_task = tokio::spawn(async move {
            let mut sockets = Vec::new();
            for sequence in 1..=2 {
                let (stream, _) = tokio::time::timeout(Duration::from_secs(1), upstream.accept())
                    .await
                    .expect("changed account identity must open a new upstream connection")
                    .unwrap();
                let server_accounts = Arc::clone(&server_accounts);
                let mut socket = accept_hdr_async_with_config(
                    stream,
                    move |request: &WebSocketRequest, response: WebSocketResponse| {
                        server_accounts.lock().unwrap().push(
                            request
                                .headers()
                                .get(CHATGPT_ACCOUNT_ID_HEADER)
                                .and_then(|value| value.to_str().ok())
                                .unwrap_or_default()
                                .to_string(),
                        );
                        Ok(response)
                    },
                    None,
                )
                .await
                .unwrap();
                let message = socket.next().await.unwrap().unwrap();
                assert!(matches!(message, WebSocketMessage::Text(_)));
                socket
                    .send(WebSocketMessage::Text(
                        serde_json::to_string(&json!({
                            "type":"response.completed",
                            "response":{
                                "id":format!("resp-{sequence}"),
                                "object":"response",
                                "status":"completed",
                                "output":[],
                            }
                        }))
                        .unwrap()
                        .into(),
                    ))
                    .await
                    .unwrap();
                // Keep the first socket alive so the second handshake proves
                // that identity matching, rather than EOF detection, forced
                // the reconnect.
                sockets.push(socket);
            }
            server_accounts.lock().unwrap().clone()
        });

        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].supports_websockets = true;
        let snapshot = RouterSnapshot::from_config(&config);
        let route = Arc::clone(&snapshot.routes[&provider_id]);
        let (downstream_socket, mut downstream_peer) = local_websocket_pair().await;
        let mut downstream = WebSocketResponsesDownstream::new(downstream_socket);

        for (account_id, input) in [("acct-first", "first"), ("acct-second", "second")] {
            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer same-token"));
            headers.insert(
                HeaderName::from_static(CHATGPT_ACCOUNT_ID_HEADER),
                HeaderValue::from_str(account_id).unwrap(),
            );
            let mut body = json!({"model":model,"input":input});
            assert_eq!(
                downstream
                    .proxy_upstream_websocket(&route, &headers, &mut body, None)
                    .await
                    .unwrap(),
                UpstreamWebSocketAttempt::Completed
            );
            let event = downstream_peer.next().await.unwrap().unwrap();
            assert!(matches!(event, WebSocketMessage::Text(_)));
        }

        assert_eq!(
            upstream_task.await.unwrap(),
            vec!["acct-first".to_string(), "acct-second".to_string()]
        );
    }

    #[allow(clippy::result_large_err)]
    #[tokio::test]
    async fn upstream_websocket_keeps_continuation_on_original_account_connection() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream.accept().await.unwrap();
            let captured_account = Arc::new(Mutex::new(String::new()));
            let server_account = Arc::clone(&captured_account);
            let mut socket = accept_hdr_async_with_config(
                stream,
                move |request: &WebSocketRequest, response: WebSocketResponse| {
                    *server_account.lock().unwrap() = request
                        .headers()
                        .get(CHATGPT_ACCOUNT_ID_HEADER)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    Ok(response)
                },
                None,
            )
            .await
            .unwrap();

            let mut requests = Vec::new();
            for sequence in 1..=2 {
                let WebSocketMessage::Text(text) = socket.next().await.unwrap().unwrap() else {
                    panic!("expected response.create text message");
                };
                let request = serde_json::from_str::<Value>(text.as_str()).unwrap();
                requests.push(request);
                socket
                    .send(WebSocketMessage::Text(
                        serde_json::to_string(&json!({
                            "type":"response.completed",
                            "response":{
                                "id":format!("resp-{sequence}"),
                                "object":"response",
                                "status":"completed",
                                "output":[],
                            }
                        }))
                        .unwrap()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }

            let opened_replacement =
                tokio::time::timeout(Duration::from_millis(200), upstream.accept())
                    .await
                    .is_ok();
            (
                captured_account.lock().unwrap().clone(),
                requests,
                opened_replacement,
            )
        });

        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].supports_websockets = true;
        let snapshot = RouterSnapshot::from_config(&config);
        let route = Arc::clone(&snapshot.routes[&provider_id]);
        let (downstream_socket, mut downstream_peer) = local_websocket_pair().await;
        let mut downstream = WebSocketResponsesDownstream::new(downstream_socket);

        for (account_id, previous_response_id) in
            [("acct-first", None), ("acct-second", Some("resp-1"))]
        {
            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer same-token"));
            headers.insert(
                HeaderName::from_static(CHATGPT_ACCOUNT_ID_HEADER),
                HeaderValue::from_str(account_id).unwrap(),
            );
            let mut body = json!({"model":model,"input":account_id});
            if let Some(previous_response_id) = previous_response_id {
                body.as_object_mut().unwrap().insert(
                    "previous_response_id".to_string(),
                    Value::String(previous_response_id.to_string()),
                );
            }
            assert_eq!(
                downstream
                    .proxy_upstream_websocket(&route, &headers, &mut body, None)
                    .await
                    .unwrap(),
                UpstreamWebSocketAttempt::Completed
            );
            let event = downstream_peer.next().await.unwrap().unwrap();
            assert!(matches!(event, WebSocketMessage::Text(_)));
        }

        let (captured_account, requests, opened_replacement) = upstream_task.await.unwrap();
        assert_eq!(captured_account, "acct-first");
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1]["previous_response_id"], "resp-1");
        assert!(!opened_replacement);
    }

    #[allow(clippy::result_large_err)]
    #[tokio::test]
    async fn unknown_previous_response_id_uses_http_fallback_instead_of_websocket_reuse() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let WebSocketMessage::Text(text) = socket.next().await.unwrap().unwrap() else {
                panic!("expected first response.create text message");
            };
            let first_request = serde_json::from_str::<Value>(text.as_str()).unwrap();
            socket
                .send(WebSocketMessage::Text(
                    serde_json::to_string(&json!({
                        "type":"response.completed",
                        "response":{
                            "id":"resp-known-on-websocket",
                            "object":"response",
                            "status":"completed",
                            "output":[],
                        }
                    }))
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();

            let reused_existing_socket =
                tokio::time::timeout(Duration::from_millis(200), socket.next())
                    .await
                    .is_ok();
            let opened_replacement =
                tokio::time::timeout(Duration::from_millis(200), upstream.accept())
                    .await
                    .is_ok();
            (first_request, reused_existing_socket, opened_replacement)
        });

        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].supports_websockets = true;
        let snapshot = RouterSnapshot::from_config(&config);
        let route = Arc::clone(&snapshot.routes[&provider_id]);
        let (downstream_socket, mut downstream_peer) = local_websocket_pair().await;
        let mut downstream = WebSocketResponsesDownstream::new(downstream_socket);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer same-token"));
        headers.insert(
            HeaderName::from_static(CHATGPT_ACCOUNT_ID_HEADER),
            HeaderValue::from_static("acct-same"),
        );

        let mut first_body = json!({"model":model,"input":"first"});
        assert_eq!(
            downstream
                .proxy_upstream_websocket(&route, &headers, &mut first_body, None)
                .await
                .unwrap(),
            UpstreamWebSocketAttempt::Completed
        );
        let event = downstream_peer.next().await.unwrap().unwrap();
        assert!(matches!(event, WebSocketMessage::Text(_)));

        let mut continuation_from_elsewhere = json!({
            "model":model,
            "input":"second",
            "previous_response_id":"resp-created-over-http"
        });
        assert_eq!(
            downstream
                .proxy_upstream_websocket(&route, &headers, &mut continuation_from_elsewhere, None,)
                .await
                .unwrap(),
            UpstreamWebSocketAttempt::UseHttp
        );

        let (first_request, reused_existing_socket, opened_replacement) =
            upstream_task.await.unwrap();
        assert_eq!(first_request["input"], "first");
        assert!(!reused_existing_socket);
        assert!(!opened_replacement);
    }

    #[tokio::test]
    async fn local_responses_websocket_rejects_missing_router_token() {
        let (config, _, _) = router_config("http://127.0.0.1:9/v1".into());
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let url = format!(
            "{}/responses",
            endpoint.base_url.replacen("http://", "ws://", 1)
        );

        let error = connect_async_with_config(url, None, false)
            .await
            .unwrap_err();
        let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
            panic!("expected HTTP handshake rejection");
        };
        assert_eq!(response.status(), WebSocketStatusCode::UNAUTHORIZED);
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn unsupported_websocket_handshake_falls_back_to_http_until_config_changes() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut handshake_stream, _) = upstream.accept().await.unwrap();
            let mut handshake_bytes = Vec::new();
            let mut chunk = [0_u8; 2048];
            loop {
                let read = handshake_stream.read(&mut chunk).await.unwrap();
                assert!(read > 0);
                handshake_bytes.extend_from_slice(&chunk[..read]);
                if find_header_end(&handshake_bytes).is_some() {
                    break;
                }
            }
            handshake_stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            drop(handshake_stream);

            let mut requests = Vec::new();
            for sequence in 1..=2 {
                let (mut stream, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut stream).await.unwrap();
                let request_body = serde_json::from_slice::<Value>(&request.body).unwrap();
                let response = json!({
                    "id":format!("resp-http-{sequence}"),
                    "object":"response",
                    "status":"completed",
                    "model":request_body["model"],
                    "output":[],
                })
                .to_string();
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                            response.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                requests.push((request.method, request.path, request_body));
            }
            (String::from_utf8(handshake_bytes).unwrap(), requests)
        });

        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].supports_websockets = true;
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let alias = model_alias(&provider_id, &model);
        let mut socket = connect_router_websocket(&endpoint).await;

        let first =
            send_router_websocket_request_on_stream(&mut socket, &alias, "first", Some("main"))
                .await;
        assert!(
            router
                .websocket_backoffs
                .lock()
                .unwrap()
                .entries
                .values()
                .any(|backoff| backoff.permanent),
            "404 handshake should permanently suppress WebSocket retries for this config"
        );
        socket.close(None).await.unwrap();
        let mut second_socket = connect_router_websocket(&endpoint).await;
        let second = send_router_websocket_request(&mut second_socket, &alias, "second").await;
        assert_eq!(first.last().unwrap()["type"], "response.completed");
        assert_eq!(second.last().unwrap()["type"], "response.completed");
        assert!(first.iter().all(|event| event["stream_id"] == "main"));
        assert!(second.iter().all(|event| event.get("stream_id").is_none()));

        second_socket.close(None).await.unwrap();
        let (handshake, requests) = upstream_task.await.unwrap();
        assert!(handshake.starts_with("GET /v1/responses HTTP/1.1\r\n"));
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.0 == "POST"));
        assert!(requests.iter().all(|request| request.1 == "/v1/responses"));
        assert_eq!(requests[0].2["model"], model);
        assert_eq!(requests[0].2["stream"], true);
        assert!(requests[0].2.get("stream_id").is_none());
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn websocket_disconnect_after_response_create_is_not_replayed_over_http() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let message = socket.next().await.unwrap().unwrap();
            let WebSocketMessage::Text(text) = message else {
                panic!("expected response.create text message");
            };
            let request = serde_json::from_str::<Value>(text.as_str()).unwrap();
            socket.close(None).await.unwrap();
            let replayed = tokio::time::timeout(Duration::from_millis(500), upstream.accept())
                .await
                .is_ok();
            (request, replayed)
        });

        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].supports_websockets = true;
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let alias = model_alias(&provider_id, &model);
        let mut socket = connect_router_websocket(&endpoint).await;

        let events = send_router_websocket_request(&mut socket, &alias, "side effect").await;
        assert_eq!(events.last().unwrap()["type"], "response.failed");
        assert_eq!(
            events.last().unwrap()["response"]["error"]["code"],
            "websocket_proxy_failed"
        );

        let (request, replayed) = upstream_task.await.unwrap();
        assert_eq!(request["type"], "response.create");
        assert!(
            !replayed,
            "committed WebSocket request must not be replayed"
        );
        socket.close(None).await.unwrap();
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn request_body_budget_rejects_before_allocating_the_declared_body() {
        let (mut reader, mut writer) = tokio::io::duplex(4096);
        writer
            .write_all(
                b"POST /v1/responses HTTP/1.1\r\ncontent-length: 131072\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let budget = Arc::new(Semaphore::new(1));

        let error = read_http_request_with_budget(&mut reader, Some(&budget))
            .await
            .unwrap_err();

        assert!(
            error
                .downcast_ref::<RequestBodyBudgetUnavailable>()
                .is_some()
        );
        assert_eq!(budget.available_permits(), 1);
    }

    #[tokio::test]
    async fn request_budget_accounts_for_json_and_conversion_working_memory() {
        let (mut reader, mut writer) = tokio::io::duplex(4096);
        writer
            .write_all(
                b"POST /v1/responses HTTP/1.1\r\ncontent-length: 65536\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let budget = Arc::new(Semaphore::new(REQUEST_MEMORY_BUDGET_MULTIPLIER - 1));

        let error = read_http_request_with_budget(&mut reader, Some(&budget))
            .await
            .unwrap_err();

        assert!(
            error
                .downcast_ref::<RequestBodyBudgetUnavailable>()
                .is_some()
        );
        assert_eq!(
            budget.available_permits(),
            REQUEST_MEMORY_BUDGET_MULTIPLIER - 1
        );
    }

    #[tokio::test]
    async fn zstd_compressed_responses_request_is_decoded_and_forwarded_as_json() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let content_encoding =
                incoming_header(&request, CONTENT_ENCODING.as_str()).map(str::to_string);
            let content_types = request
                .headers
                .iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case(CONTENT_TYPE.as_str()))
                .map(|(_, value)| value.clone())
                .collect::<Vec<_>>();
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            write_json_response(
                &mut stream,
                200,
                &json!({
                    "id":"resp-zstd",
                    "object":"response",
                    "status":"completed",
                    "model":body["model"],
                    "output":[],
                }),
            )
            .await
            .unwrap();
            (content_encoding, content_types, body)
        });
        let (config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let request_body = serde_json::to_vec(&json!({
            "model":model_alias(&provider_id, &model),
            "input":[{
                "role":"user",
                "content":[{"type":"input_text","text":"compressed"}],
            }],
            "store":false,
            "stream":false,
        }))
        .unwrap();
        let compressed = zstd::stream::encode_all(Cursor::new(request_body), 3).unwrap();

        let response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .header(CONTENT_TYPE, "application/json")
            .header(CONTENT_ENCODING, "zstd")
            .body(compressed)
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.json::<Value>().await.unwrap()["id"], "resp-zstd");
        let (content_encoding, content_types, body) = upstream_task.await.unwrap();
        assert!(content_encoding.is_none());
        assert_eq!(content_types, ["application/json"]);
        assert_eq!(body["model"], model);
        assert_eq!(body["input"][0]["content"][0]["text"], "compressed");
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn stopping_router_aborts_connections_that_outlive_the_drain_deadline() {
        let router = LocalRouter::start(&CodeyConfig::default()).await.unwrap();
        let endpoint = router.endpoint();
        let port = endpoint
            .base_url
            .strip_prefix("http://127.0.0.1:")
            .and_then(|value| value.split('/').next())
            .unwrap()
            .parse::<u16>()
            .unwrap();
        let mut connection = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        connection
            .write_all(
                format!(
                    "POST /v1/responses HTTP/1.1\r\nauthorization: Bearer {}\r\ncontent-length: 1024\r\n\r\n{{",
                    endpoint.token
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        tokio::task::yield_now().await;

        tokio::time::timeout(Duration::from_secs(1), router.stop())
            .await
            .expect("router shutdown should have a bounded drain")
            .unwrap();

        let mut byte = [0_u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(1), connection.read(&mut byte))
            .await
            .expect("managed connection should close with the router");
        assert!(matches!(read, Ok(0) | Err(_)));
    }

    #[tokio::test]
    async fn header_limit_does_not_count_body_buffered_by_the_same_read() {
        let prefix = b"POST /v1/responses HTTP/1.1\r\ncontent-length: 2\r\nx-pad: ";
        let padding = "a".repeat(MAX_HEADER_BYTES - prefix.len());
        let request = format!("{}{}\r\n\r\nok", String::from_utf8_lossy(prefix), padding);
        let mut reader = request.as_bytes();

        let parsed = read_http_request(&mut reader).await.unwrap();

        assert_eq!(parsed.body, b"ok");
    }

    #[tokio::test]
    async fn header_limit_still_rejects_an_oversized_header() {
        let prefix = b"GET /health HTTP/1.1\r\nx-pad: ";
        let padding = "a".repeat(MAX_HEADER_BYTES + 1 - prefix.len());
        let request = format!("{}{}\r\n\r\n", String::from_utf8_lossy(prefix), padding);
        let mut reader = request.as_bytes();

        let error = read_http_request(&mut reader).await.unwrap_err();

        assert!(error.to_string().contains("HTTP 请求头超过"));
    }

    #[test]
    fn route_bindings_refresh_in_amortized_constant_time_and_evict_stale_entries() {
        let mut bindings = RouteBindings::default();
        for index in 0..MAX_ROUTE_BINDINGS {
            bindings.remember(&[format!("thread-id:{index}")], "route-a", false);
        }
        for _ in 0..(MAX_ROUTE_BINDINGS * 5) {
            bindings.remember(&["thread-id:0".to_string()], "route-b", false);
        }

        assert!(bindings.order.len() <= MAX_ROUTE_BINDINGS * 4);
        assert_eq!(
            bindings.route_for_keys(&["thread-id:0".to_string()]),
            Some("route-b".to_string())
        );

        bindings.remember(&["thread-id:new".to_string()], "route-c", false);
        assert_eq!(bindings.routes.len(), MAX_ROUTE_BINDINGS);
        assert!(bindings.routes.contains_key("thread-id:0"));
        assert!(bindings.routes.contains_key("thread-id:new"));
        assert!(!bindings.routes.contains_key("thread-id:1"));
    }

    #[test]
    fn explicit_parent_switch_refreshes_session_fallback_without_child_overwrite() {
        let mut bindings = RouteBindings::default();
        let keys = [
            "thread-id:parent".to_string(),
            "session-id:tree".to_string(),
        ];
        bindings.remember(&keys, "route-a", false);
        bindings.remember(&keys, "route-b", false);

        assert_eq!(
            bindings.route_for_keys(&["session-id:tree".to_string()]),
            Some("route-a".to_string())
        );

        bindings.remember(&keys, "route-b", true);

        assert_eq!(
            bindings.route_for_keys(&["session-id:tree".to_string()]),
            Some("route-b".to_string())
        );
    }

    #[test]
    fn request_log_codex_session_prefers_thread_and_falls_back_to_session() {
        let mut request = HttpRequest {
            method: "POST".into(),
            path: "/v1/responses".into(),
            headers: vec![
                ("session-id".into(), "session-tree".into()),
                ("thread-id".into(), "current-thread".into()),
            ],
            body: b"must-not-be-inspected".to_vec(),
            _body_budget_permit: None,
        };
        assert_eq!(
            request_log_codex_session(&request),
            (Some("current-thread"), false)
        );

        request.headers.retain(|(name, _)| name != "thread-id");
        assert_eq!(
            request_log_codex_session(&request),
            (Some("session-tree"), false)
        );
    }

    #[test]
    fn request_log_parent_session_suppresses_child_identifiers() {
        let mut request = HttpRequest {
            method: "POST".into(),
            path: "/v1/responses".into(),
            headers: vec![
                ("x-codex-parent-thread-id".into(), "parent-thread".into()),
                ("thread-id".into(), "child-thread".into()),
                ("session-id".into(), "child-session".into()),
            ],
            body: b"child prompt must remain unrelated".to_vec(),
            _body_budget_permit: None,
        };
        assert_eq!(
            request_log_codex_session(&request),
            (Some("parent-thread"), true)
        );

        request.headers[0].1.clear();
        assert_eq!(request_log_codex_session(&request), (None, true));

        request.headers = vec![
            ("x-openai-subagent".into(), "codey_worker".into()),
            ("thread-id".into(), "child-thread".into()),
            ("session-id".into(), "child-session".into()),
        ];
        assert_eq!(request_log_codex_session(&request), (None, true));
    }

    #[test]
    fn router_snapshot_maps_route_aliases_to_upstream_models() {
        let (config, provider_id, model) = router_config("https://relay.example/v1".to_string());

        let snapshot = RouterSnapshot::from_config(&config);
        let resolved = snapshot
            .target_for_model(&model_alias(&provider_id, &model))
            .unwrap();

        assert_eq!(
            resolved.route.upstream_url.as_ref().unwrap(),
            "https://relay.example/v1/responses"
        );
        assert_eq!(resolved.upstream_model, model);
        let raw = snapshot.target_for_model("provider-model").unwrap();
        assert_eq!(raw.route.provider_id, provider_id);
    }

    #[test]
    fn historical_aliases_recover_after_route_deletion_disable_and_restart() {
        let (mut config, provider, model) = router_config("https://relay.example/v1".into());
        for legacy in ["codey", "old/relay"] {
            let mut old = config.profiles[0].clone();
            old.id = legacy.into();
            config.profiles.push(old);
            config
                .selected_models_by_provider
                .insert(legacy.into(), vec![model.clone()]);
        }
        config = config.normalize();
        config.profiles.retain(|route| route.id != "old/relay");
        config.selected_models_by_provider.remove("old/relay");
        config.selected_models_by_provider.remove("codey");
        let directory = tempfile::tempdir().unwrap();
        let store = crate::config::ConfigStore::new(directory.path().join("config.json"));
        store.save(&config).unwrap();
        let restored = store.load().unwrap();
        let snapshot = RouterSnapshot::from_config(&restored);
        for requested in [
            model.clone(),
            format!("CODEY/{model}"),
            model_alias("old/relay", &model),
        ] {
            let selected = snapshot
                .target_for_request(&requested, Some("old/relay"), Some("codey"))
                .unwrap();
            assert_eq!(selected.provider_id, provider);
            assert_eq!(selected.upstream_model, model);
            assert_eq!(selected.requested_model, requested);
        }
    }

    #[test]
    fn legacy_codey_without_history_resolves_case_and_preserves_slash_raw_models() {
        let (mut config, provider, _) = router_config("https://relay.example/v1".into());
        config.selected_models_by_provider.insert(
            provider.clone(),
            vec!["Vendor/Model".into(), "codey/vendor/model".into()],
        );
        let snapshot = RouterSnapshot::from_config(&config);
        for (requested, upstream) in [
            ("vendor/model", "Vendor/Model"),
            ("codey/vendor/model", "codey/vendor/model"),
            ("CODEY/codey/vendor/model", "codey/vendor/model"),
            ("ROUTE-A/VENDOR/MODEL", "Vendor/Model"),
        ] {
            assert_eq!(
                snapshot.target_for_model(requested).unwrap().upstream_model,
                upstream
            );
        }
        assert!(snapshot.target_for_model("unknown/Vendor/Model").is_err());
        assert!(
            snapshot
                .target_for_model("codey/missing")
                .unwrap_err()
                .to_string()
                .contains("历史线路已不可用")
        );
        assert!(
            snapshot
                .target_for_model("")
                .unwrap_err()
                .to_string()
                .contains("缺少 model")
        );
    }

    #[test]
    fn historical_aliases_require_an_unambiguous_route_and_never_override_active_aliases() {
        let (mut config, provider, model) = router_config("https://relay.example/v1".into());
        let mut second = config.profiles[0].clone();
        second.id = "route-b".into();
        config.profiles.push(second);
        config
            .selected_models_by_provider
            .insert("route-b".into(), vec![model.clone()]);
        let snapshot = RouterSnapshot::from_config(&config);
        let legacy = format!("codey/{model}");
        let error = snapshot.target_for_model(&legacy).unwrap_err();
        assert!(format!("{error:#}").contains("缺少明确"));
        for (hint, binding) in [(Some("route-b"), None), (None, Some("route-b"))] {
            assert_eq!(
                snapshot
                    .target_for_request(&legacy, hint, binding)
                    .unwrap()
                    .provider_id,
                "route-b"
            );
        }
        let explicit = model_alias(&provider, &model);
        assert_eq!(
            snapshot
                .target_for_request(&explicit, Some("route-b"), Some("route-b"))
                .unwrap()
                .provider_id,
            provider
        );
        config.profiles[0].name = "Renamed display label".into();
        assert_eq!(
            RouterSnapshot::from_config(&config)
                .target_for_model(&explicit)
                .unwrap()
                .provider_id,
            provider
        );
    }

    #[test]
    fn historical_upstream_ids_are_not_recursively_interpreted_as_active_aliases() {
        let (mut config, provider, _) = router_config("https://relay.example/v1".into());
        config.model_alias_history.insert(
            "old/route-a/provider-model".into(),
            "route-a/provider-model".into(),
        );
        let snapshot = RouterSnapshot::from_config(&config);
        assert!(
            snapshot
                .target_for_model("old/route-a/provider-model")
                .is_err()
        );
        config
            .selected_models_by_provider
            .get_mut(&provider)
            .unwrap()
            .push("route-a/provider-model".into());
        assert_eq!(
            RouterSnapshot::from_config(&config)
                .target_for_model("old/route-a/provider-model")
                .unwrap()
                .upstream_model,
            "route-a/provider-model"
        );
    }

    #[test]
    fn router_snapshot_keeps_all_third_party_routes_active_at_once() {
        let (mut config, provider_a, model) =
            router_config("https://relay-a.example/v1".to_string());
        let mut route_b = config.profiles[0].clone();
        route_b.id = "route-b".into();
        route_b.name = "Relay B".into();
        route_b.base_url = "https://relay-b.example/v1".into();
        route_b.api_key = "sk-route-b".into();
        route_b.normalize();
        let provider_b = route_b.provider_id().to_string();
        config.profiles.push(route_b);
        config
            .selected_models_by_provider
            .insert(provider_b.clone(), vec![model.clone()]);

        let snapshot = RouterSnapshot::from_config(&config);
        let resolved_a = snapshot
            .target_for_model(&model_alias(&provider_a, &model))
            .unwrap();
        let resolved_b = snapshot
            .target_for_model(&model_alias(&provider_b, &model))
            .unwrap();

        assert_eq!(
            resolved_a.route.upstream_url.as_ref().unwrap(),
            "https://relay-a.example/v1/responses"
        );
        assert_eq!(
            resolved_b.route.upstream_url.as_ref().unwrap(),
            "https://relay-b.example/v1/responses"
        );
        assert_eq!(snapshot.model_aliases().len(), 2);
        assert_eq!(snapshot.model_ids(), vec![model.clone()]);
        assert!(snapshot.target_for_model(&model).is_err());
        let hinted = snapshot
            .target_for_request(&model, Some(&provider_b), None)
            .unwrap();
        assert_eq!(hinted.route.provider_id, provider_b);
        let qualified_alias_with_stale_hint = snapshot
            .target_for_request(&model_alias(&provider_a, &model), Some(&provider_b), None)
            .unwrap();
        assert_eq!(
            qualified_alias_with_stale_hint.route.provider_id,
            provider_a
        );
        assert_eq!(qualified_alias_with_stale_hint.upstream_model, model);
    }

    #[test]
    fn stale_thread_binding_yields_to_only_an_unambiguous_new_model() {
        let (mut config, provider_a, model_a) =
            router_config("https://relay-a.example/v1".to_string());
        let mut route_b = config.profiles[0].clone();
        route_b.id = "route-b".into();
        route_b.name = "Relay B".into();
        route_b.base_url = "https://relay-b.example/v1".into();
        route_b.api_key = "sk-route-b".into();
        route_b.normalize();
        let provider_b = route_b.provider_id().to_string();
        let model_b = "new-model".to_string();
        config.profiles.push(route_b);
        config
            .selected_models_by_provider
            .insert(provider_b.clone(), vec![model_b.clone()]);

        let snapshot = RouterSnapshot::from_config(&config);
        let switched = snapshot
            .target_for_request(&model_b, None, Some(&provider_a))
            .unwrap();
        assert_eq!(switched.route.provider_id, provider_b);
        assert_eq!(switched.upstream_model, model_b);
        let switched_with_replayed_hint = snapshot
            .target_for_request(&model_b, Some(&provider_a), Some(&provider_a))
            .unwrap();
        assert_eq!(switched_with_replayed_hint.route.provider_id, provider_b);
        assert_eq!(switched_with_replayed_hint.upstream_model, model_b);
        let switched_with_unbound_replayed_hint = snapshot
            .target_for_request("new-model", Some(&provider_a), None)
            .unwrap();
        assert_eq!(
            switched_with_unbound_replayed_hint.route.provider_id,
            provider_b
        );

        let mut route_c = config.profiles[0].clone();
        route_c.id = "route-c".into();
        route_c.name = "Relay C".into();
        route_c.base_url = "https://relay-c.example/v1".into();
        route_c.api_key = "sk-route-c".into();
        route_c.normalize();
        let provider_c = route_c.provider_id().to_string();
        config.profiles.push(route_c);
        config
            .selected_models_by_provider
            .insert(provider_c, vec!["new-model".into()]);
        let ambiguous = RouterSnapshot::from_config(&config)
            .target_for_request("new-model", Some(&provider_a), Some(&provider_a))
            .unwrap_err()
            .to_string();
        assert!(ambiguous.contains("缺少明确"));

        let unchanged = snapshot
            .target_for_request(&model_a, None, Some(&provider_a))
            .unwrap();
        assert_eq!(unchanged.route.provider_id, provider_a);
    }

    #[test]
    fn router_does_not_invent_models_for_an_unconfigured_api_route() {
        let (mut config, provider_id, _) = router_config("https://relay.example/v1".to_string());
        config.selected_models_by_provider.remove(&provider_id);

        let snapshot = RouterSnapshot::from_config(&config);

        assert!(snapshot.model_aliases().is_empty());
        assert!(
            snapshot
                .target_for_model(&model_alias(&provider_id, "gpt-5.6-sol"))
                .is_err()
        );
    }

    #[test]
    fn official_looking_ids_declared_on_an_api_route_remain_route_scoped() {
        let (mut config, provider_id, _) = router_config("https://relay.example/v1".to_string());
        config.selected_models_by_provider.remove(&provider_id);
        config
            .declared_official_models_by_provider
            .insert(provider_id.clone(), vec!["gpt-5.6-sol".into()]);

        let snapshot = RouterSnapshot::from_config(&config);
        let resolved = snapshot
            .target_for_model(&model_alias(&provider_id, "gpt-5.6-sol"))
            .unwrap();

        assert_eq!(resolved.route.provider_id, provider_id);
        assert_eq!(resolved.upstream_model, "gpt-5.6-sol");
    }

    #[test]
    fn official_account_models_enter_the_router_only_when_login_is_available() {
        let mut official = ProviderProfile::new("OpenAI 官方直登");
        official.id = crate::config::DERIVED_OFFICIAL_PROFILE_ID.into();
        official.source_provider_id = Some("openai".into());
        official.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.into();
        official.normalize();
        let mut config = CodeyConfig {
            active_profile_id: official.id.clone(),
            profiles: vec![official],
            official_account_available_this_launch: true,
            ..CodeyConfig::default()
        }
        .normalize();
        config
            .selected_models_by_provider
            .insert("openai".into(), vec!["gpt-5.6-sol".into()]);

        let snapshot = RouterSnapshot::from_config(&config);
        let official_alias = model_alias("openai", "gpt-5.6-sol");
        let resolved = snapshot.target_for_model(&official_alias).unwrap();

        assert_eq!(resolved.route.provider_id, "openai");
        assert!(resolved.route.official_account);
        assert!(resolved.route.supports_websockets);
        assert_eq!(
            resolved.route.upstream_url.as_ref().unwrap(),
            &format!("{CHATGPT_CODEX_BASE_URL}/responses")
        );
        assert_eq!(
            resolved.route.upstream_compact_url.as_ref().unwrap(),
            &format!("{CHATGPT_CODEX_BASE_URL}/responses/compact")
        );
        assert_eq!(
            resolved.route.upstream_websocket_url.as_ref().unwrap(),
            &format!(
                "{}/responses",
                CHATGPT_CODEX_BASE_URL.replacen("https://", "wss://", 1)
            )
        );
        assert_eq!(resolved.upstream_model, "gpt-5.6-sol");
        let raw = snapshot.target_for_model("gpt-5.6-sol").unwrap();
        assert_eq!(raw.route.provider_id, "openai");
        let auto_review = snapshot.target_for_model(CODEX_AUTO_REVIEW_MODEL).unwrap();
        assert_eq!(auto_review.route.provider_id, "openai");
        assert_eq!(auto_review.upstream_model, CODEX_AUTO_REVIEW_MODEL);

        let mut mixed = config.clone();
        let mut relay = ProviderProfile::new("Relay");
        relay.id = "relay".into();
        relay.base_url = "https://relay.example/v1".into();
        relay.api_key = "relay-key".into();
        relay.normalize();
        let relay_id = relay.provider_id().to_string();
        mixed.profiles.push(relay);
        mixed
            .selected_models_by_provider
            .insert(relay_id.clone(), vec!["gpt-5.6-sol".into()]);
        let mixed_snapshot = RouterSnapshot::from_config(&mixed);
        assert_eq!(
            mixed_snapshot
                .target_for_model("gpt-5.6-sol")
                .unwrap()
                .route
                .provider_id,
            "openai"
        );
        assert_eq!(
            mixed_snapshot
                .target_for_request("gpt-5.6-sol", Some(&relay_id), None)
                .unwrap()
                .route
                .provider_id,
            relay_id
        );

        let mut api_key_launch = config;
        api_key_launch.official_account_available_this_launch = false;
        assert!(!api_key_launch.runtime_supports_websockets());
        assert!(
            RouterSnapshot::from_config(&api_key_launch)
                .target_for_model(&official_alias)
                .is_err()
        );
        assert!(
            RouterSnapshot::from_config(&api_key_launch)
                .target_for_model(CODEX_AUTO_REVIEW_MODEL)
                .is_err()
        );
    }

    #[test]
    fn auto_review_uses_a_capable_bound_route_and_otherwise_prefers_official() {
        let (mut config, third_party_provider, _) =
            router_config("https://relay.example/v1".to_string());
        config.profiles[0].supports_auto_review = true;
        let mut official = ProviderProfile::new("OpenAI 官方直登");
        official.id = crate::config::DERIVED_OFFICIAL_PROFILE_ID.into();
        official.source_provider_id = Some("openai".into());
        official.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.into();
        official.normalize();
        config.profiles.push(official);
        config.official_account_available_this_launch = true;
        config
            .selected_models_by_provider
            .insert("openai".into(), vec!["gpt-5.6-sol".into()]);
        let snapshot = RouterSnapshot::from_config(&config);

        let bound = snapshot
            .target_for_request(CODEX_AUTO_REVIEW_MODEL, None, Some(&third_party_provider))
            .unwrap();
        assert_eq!(bound.route.provider_id, third_party_provider);

        let unbound = snapshot
            .target_for_request(CODEX_AUTO_REVIEW_MODEL, None, None)
            .unwrap();
        assert_eq!(unbound.route.provider_id, "openai");
    }

    #[test]
    fn auto_review_is_not_invented_for_an_unsupported_third_party_route() {
        let (mut config, provider_id, _) = router_config("https://relay.example/v1".to_string());
        config
            .upstream_models_by_provider
            .insert(provider_id, vec![CODEX_AUTO_REVIEW_MODEL.into()]);

        assert!(
            RouterSnapshot::from_config(&config)
                .target_for_model(CODEX_AUTO_REVIEW_MODEL)
                .is_err()
        );
    }

    #[test]
    fn third_party_routes_forward_codex_identity_without_chatgpt_account_headers() {
        assert!(should_forward_incoming_header("chatgpt-account-id", true));
        assert!(should_forward_incoming_header("x-openai-originator", true));
        assert!(!should_forward_incoming_header("chatgpt-account-id", false));
        assert!(should_forward_incoming_header("x-openai-originator", false));
        assert!(should_forward_incoming_header("user-agent", false));
        assert!(should_forward_incoming_header(
            "x-codex-installation-id",
            false
        ));
        assert!(should_forward_incoming_header("x-codex-window-id", false));
        assert!(should_forward_incoming_header("originator", false));
        assert!(should_forward_incoming_header("x-stainless-os", false));
        assert!(should_forward_incoming_header("thread-id", false));
        assert!(should_forward_incoming_header("session-id", false));
        assert!(should_forward_incoming_header("prompt-cache-key", false));
        assert!(should_forward_incoming_header("prompt_cache_key", false));
        assert!(!should_forward_incoming_header("authorization", true));
        assert!(!should_forward_incoming_header(ROUTER_AUTH_HEADER, true));
        assert!(!should_forward_incoming_header(ROUTE_METADATA_KEY, true));
        assert!(!should_forward_incoming_header(TURN_METADATA_HEADER, false));
        assert!(should_forward_incoming_header("accept", false));
    }

    #[test]
    fn generated_prompt_cache_key_is_stable_and_scoped() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer token-a"));
        headers.insert(
            HeaderName::from_static(CHATGPT_ACCOUNT_ID_HEADER),
            HeaderValue::from_static("acct-a"),
        );
        let key = stable_prompt_cache_key(
            "route-a",
            "https://api.example/v1/responses",
            "model-a",
            &headers,
        );
        assert_eq!(
            key,
            stable_prompt_cache_key(
                "route-a",
                "https://api.example/v1/responses",
                "model-a",
                &headers,
            )
        );
        assert!(key.starts_with("codey-"));
        assert_eq!(key.len(), "codey-".len() + 48);

        let mut refreshed_auth = headers.clone();
        refreshed_auth.insert(AUTHORIZATION, HeaderValue::from_static("Bearer token-b"));
        assert_eq!(
            key,
            stable_prompt_cache_key(
                "route-a",
                "https://api.example/v1/responses",
                "model-a",
                &refreshed_auth,
            ),
            "account identity should keep the key stable across token refreshes"
        );
        assert_ne!(
            key,
            stable_prompt_cache_key(
                "route-b",
                "https://api.example/v1/responses",
                "model-a",
                &headers,
            )
        );
        assert_ne!(
            key,
            stable_prompt_cache_key(
                "route-a",
                "https://api.example/v1/responses",
                "model-b",
                &headers,
            )
        );
        let mut changed_account = headers;
        changed_account.insert(
            HeaderName::from_static(CHATGPT_ACCOUNT_ID_HEADER),
            HeaderValue::from_static("acct-b"),
        );
        assert_ne!(
            key,
            stable_prompt_cache_key(
                "route-a",
                "https://api.example/v1/responses",
                "model-a",
                &changed_account,
            )
        );
        let mut api_key_headers = HeaderMap::new();
        api_key_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer token-a"));
        let api_key_identity = stable_prompt_cache_key(
            "route-a",
            "https://api.example/v1/responses",
            "model-a",
            &api_key_headers,
        );
        api_key_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer token-b"));
        assert_ne!(
            api_key_identity,
            stable_prompt_cache_key(
                "route-a",
                "https://api.example/v1/responses",
                "model-a",
                &api_key_headers,
            )
        );
    }

    #[test]
    fn generated_prompt_cache_key_never_overrides_caller_input() {
        let body = json!({"model":"model-a","input":"hello"});
        let mut generated_headers = HeaderMap::new();
        assert!(ensure_native_prompt_cache_key(
            &mut generated_headers,
            &body,
            "route-a",
            "https://api.example/v1/responses",
            "model-a",
        ));
        assert!(generated_headers.contains_key(PROMPT_CACHE_KEY_HEADER));

        let mut caller_headers = HeaderMap::new();
        caller_headers.insert(
            HeaderName::from_static(PROMPT_CACHE_KEY_HEADER),
            HeaderValue::from_static("caller-key"),
        );
        assert!(!ensure_native_prompt_cache_key(
            &mut caller_headers,
            &body,
            "route-a",
            "https://api.example/v1/responses",
            "model-a",
        ));
        assert_eq!(
            caller_headers[PROMPT_CACHE_KEY_HEADER],
            HeaderValue::from_static("caller-key")
        );

        let mut body_key_headers = HeaderMap::new();
        assert!(!ensure_native_prompt_cache_key(
            &mut body_key_headers,
            &json!({"model":"model-a","prompt_cache_key":"body-key"}),
            "route-a",
            "https://api.example/v1/responses",
            "model-a",
        ));
        assert!(!body_key_headers.contains_key(PROMPT_CACHE_KEY_HEADER));
    }

    #[test]
    fn local_router_bearer_token_is_not_reused_as_openai_oauth() {
        let request = HttpRequest {
            method: "POST".to_string(),
            path: "/v1/responses".to_string(),
            headers: vec![
                (
                    "authorization".to_string(),
                    "Bearer codey-router-token".to_string(),
                ),
                (
                    CHATGPT_ACCOUNT_ID_HEADER.to_string(),
                    "acct-stale-downstream".to_string(),
                ),
            ],
            body: Vec::new(),
            _body_budget_permit: None,
        };

        assert_eq!(
            incoming_openai_authorization(&request, "Bearer codey-router-token"),
            None
        );
        assert_eq!(
            incoming_openai_authorization(&request, "Bearer another-router-token"),
            Some("Bearer codey-router-token")
        );
    }

    #[tokio::test]
    async fn official_upstream_auth_prefers_incoming_oauth_over_auth_json() {
        let request = HttpRequest {
            method: "POST".to_string(),
            path: "/v1/responses".to_string(),
            headers: vec![
                (
                    "authorization".to_string(),
                    "Bearer chatgpt-oauth".to_string(),
                ),
                (
                    CHATGPT_ACCOUNT_ID_HEADER.to_string(),
                    "acct-incoming".to_string(),
                ),
            ],
            body: Vec::new(),
            _body_budget_permit: None,
        };

        let auth_cache = Mutex::new(crate::account_usage::OfficialAuthCache::default());
        let auth = resolve_official_upstream_auth(
            &request,
            "Bearer codey-router-token",
            Path::new("/missing-auth.json"),
            &auth_cache,
        )
        .await
        .unwrap();
        assert_eq!(auth.authorization, "Bearer chatgpt-oauth");
        assert_eq!(auth.account_id.as_deref(), Some("acct-incoming"));
    }

    #[tokio::test]
    async fn official_upstream_auth_loads_codex_auth_json_when_codex_uses_the_router_bearer() {
        let directory = tempfile::tempdir().unwrap();
        let auth_path = directory.path().join("auth.json");
        std::fs::write(
            &auth_path,
            r#"{"auth_mode":"chatgpt","tokens":{"access_token":"chatgpt-access","account_id":"acct-9"}}"#,
        )
        .unwrap();
        let request = HttpRequest {
            method: "POST".to_string(),
            path: "/v1/responses".to_string(),
            headers: vec![(
                "authorization".to_string(),
                "Bearer codey-router-token".to_string(),
            )],
            body: Vec::new(),
            _body_budget_permit: None,
        };

        let auth_cache = Mutex::new(crate::account_usage::OfficialAuthCache::default());
        let auth = resolve_official_upstream_auth(
            &request,
            "Bearer codey-router-token",
            &auth_path,
            &auth_cache,
        )
        .await
        .unwrap();
        assert_eq!(auth.authorization, "Bearer chatgpt-access");
        assert_eq!(auth.account_id.as_deref(), Some("acct-9"));
    }

    #[tokio::test]
    async fn official_upstream_auth_is_missing_without_oauth_or_auth_json() {
        let request = HttpRequest {
            method: "POST".to_string(),
            path: "/v1/responses".to_string(),
            headers: vec![(
                "authorization".to_string(),
                "Bearer codey-router-token".to_string(),
            )],
            body: Vec::new(),
            _body_budget_permit: None,
        };

        let auth_cache = Mutex::new(crate::account_usage::OfficialAuthCache::default());
        assert!(
            resolve_official_upstream_auth(
                &request,
                "Bearer codey-router-token",
                Path::new("/missing-auth.json"),
                &auth_cache,
            )
            .await
            .is_none()
        );
    }

    #[test]
    fn protocol_tokens_are_case_insensitive_without_rewriting_endpoint_paths() {
        assert!(is_hop_by_hop_header("Transfer-Encoding"));
        assert!(!should_forward_incoming_header("ConNection", true));
        assert!(!should_forward_incoming_header("Content-Encoding", true));
        assert!(!should_forward_incoming_header("Content-Type", true));
        assert!(is_sse_content_type("Text/Event-Stream; Charset=UTF-8"));

        let base_url = "https://relay.example/API/V1/Responses?token=private#debug";
        assert_eq!(
            responses_endpoint(base_url).unwrap(),
            "https://relay.example/API/V1/Responses"
        );
        assert_eq!(
            chat_completions_endpoint(base_url).unwrap(),
            "https://relay.example/API/V1/chat/completions"
        );
        assert_eq!(
            anthropic_messages_endpoint(base_url).unwrap(),
            "https://relay.example/API/V1/messages"
        );
    }

    #[test]
    fn router_snapshot_prepares_upstream_url_and_headers() {
        let (mut config, provider_id, model) =
            router_config("https://relay.example/v1".to_string());
        config.profiles[0].upstream_protocol =
            crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
        config.profiles[0]
            .model_request_headers
            .insert("authorization".into(), "Bearer custom".into());
        config.profiles[0]
            .model_request_headers
            .insert("accept".into(), "application/x-codey-test".into());

        let resolved = RouterSnapshot::from_config(&config)
            .target_for_model(&model_alias(&provider_id, &model))
            .unwrap();
        let headers = resolved.route.upstream_headers.as_ref().unwrap();

        assert_eq!(resolved.protocol, UpstreamProtocol::OpenAiChatCompletions);
        assert_eq!(
            resolved.route.upstream_url.as_ref().unwrap(),
            "https://relay.example/v1/chat/completions"
        );
        assert_eq!(
            headers.get(AUTHORIZATION).unwrap().to_str().unwrap(),
            "Bearer custom"
        );
        assert_eq!(
            headers.get("accept").unwrap().to_str().unwrap(),
            "application/x-codey-test"
        );
    }

    #[test]
    fn route_resolver_keeps_provider_protocol_and_model_selection_explicit() {
        let (mut config, provider_a, model) =
            router_config("https://responses.example/v1".to_string());
        config.profiles[0].upstream_protocol =
            crate::config::UPSTREAM_PROTOCOL_OPENAI_RESPONSES.into();

        let mut route_b = config.profiles[0].clone();
        route_b.id = "route-b".into();
        route_b.name = "Chat Relay".into();
        route_b.base_url = "https://chat.example/v1".into();
        route_b.upstream_protocol = crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
        route_b.normalize();
        let provider_b = route_b.provider_id().to_string();

        let mut route_c = config.profiles[0].clone();
        route_c.id = "route-c".into();
        route_c.name = "Anthropic Relay".into();
        route_c.base_url = "https://anthropic.example/v1".into();
        route_c.upstream_protocol = crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES.into();
        route_c.normalize();
        let provider_c = route_c.provider_id().to_string();

        config.profiles.extend([route_b, route_c]);
        config
            .selected_models_by_provider
            .insert(provider_b.clone(), vec![model.clone()]);
        config
            .selected_models_by_provider
            .insert(provider_c.clone(), vec![model.clone()]);

        let snapshot = RouterSnapshot::from_config(&config);
        let selections = [
            (
                provider_a.as_str(),
                UpstreamProtocol::OpenAiResponses,
                "https://responses.example/v1/responses",
            ),
            (
                provider_b.as_str(),
                UpstreamProtocol::OpenAiChatCompletions,
                "https://chat.example/v1/chat/completions",
            ),
            (
                provider_c.as_str(),
                UpstreamProtocol::AnthropicMessages,
                "https://anthropic.example/v1/messages",
            ),
        ];

        for (provider_id, protocol, upstream_url) in selections {
            let requested_model = model_alias(provider_id, &model);
            let resolved = RouteResolver::new(&snapshot)
                .resolve(RouteRequest {
                    requested_model: &requested_model,
                    route_hint: None,
                    bound_route: None,
                })
                .unwrap();

            assert_eq!(resolved.provider_id, provider_id);
            assert_eq!(resolved.protocol, protocol);
            assert_eq!(resolved.requested_model, requested_model);
            assert_eq!(resolved.upstream_model, model);
            assert_eq!(resolved.route.upstream_url.as_ref().unwrap(), upstream_url);
        }

        assert!(snapshot.target_for_model(&model).is_err());
    }

    #[test]
    fn protocol_bridge_converts_only_when_the_upstream_protocol_differs() {
        assert_eq!(
            ProtocolBridge::from_upstream_protocol(UpstreamProtocol::OpenAiResponses),
            ProtocolBridge::NativeResponses
        );
        assert_eq!(
            ProtocolBridge::from_upstream_protocol(UpstreamProtocol::OpenAiChatCompletions),
            ProtocolBridge::ResponsesToChatCompletions
        );
        assert_eq!(
            ProtocolBridge::from_upstream_protocol(UpstreamProtocol::AnthropicMessages),
            ProtocolBridge::ResponsesToAnthropicMessages
        );

        let native = ProtocolBridge::NativeResponses
            .convert_responses_body(&json!({
                "model":"provider-model",
                "input":"search",
                "tools":[{"type":"web_search"}]
            }))
            .unwrap();

        assert!(native.is_none());
    }

    #[test]
    fn router_snapshot_rejects_unsafe_saved_headers_without_per_request_parsing() {
        let (mut config, provider_id, model) =
            router_config("https://relay.example/v1".to_string());
        config.profiles[0]
            .model_request_headers
            .insert("connection".into(), "keep-alive".into());

        let resolved = RouterSnapshot::from_config(&config)
            .target_for_model(&model_alias(&provider_id, &model))
            .unwrap();

        assert!(
            resolved
                .route
                .upstream_headers
                .as_ref()
                .unwrap_err()
                .contains("不允许覆盖")
        );
    }

    #[test]
    fn codey_route_metadata_is_removed_without_dropping_other_turn_metadata() {
        let mut request = HttpRequest {
            method: "POST".into(),
            path: "/v1/responses".into(),
            headers: vec![(
                TURN_METADATA_HEADER.into(),
                json!({ROUTE_METADATA_KEY:"route-a","keep":"header"}).to_string(),
            )],
            body: Vec::new(),
            _body_budget_permit: None,
        };
        let mut body = json!({
            "client_metadata": {
                "x-codex-turn-metadata": json!({
                    ROUTE_METADATA_KEY: "route-a",
                    "keep": "body"
                }).to_string()
            }
        });

        let (route, body_mutated) = take_codey_route_metadata(&mut request, &mut body).unwrap();

        assert_eq!(route.as_deref(), Some("route-a"));
        assert!(body_mutated);
        let header = serde_json::from_str::<Value>(&request.headers[0].1).unwrap();
        assert!(header.get(ROUTE_METADATA_KEY).is_none());
        assert_eq!(header["keep"], "header");
        let nested = serde_json::from_str::<Value>(
            body["client_metadata"][TURN_METADATA_HEADER]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert!(nested.get(ROUTE_METADATA_KEY).is_none());
        assert_eq!(nested["keep"], "body");
    }

    #[test]
    fn codey_route_metadata_header_only_does_not_mark_the_body_mutated() {
        let mut request = HttpRequest {
            method: "POST".into(),
            path: "/v1/responses".into(),
            headers: vec![(
                TURN_METADATA_HEADER.into(),
                json!({ROUTE_METADATA_KEY:"route-a","keep":"header"}).to_string(),
            )],
            body: Vec::new(),
            _body_budget_permit: None,
        };
        let mut body = json!({
            "model": "gpt-5.4",
            "client_metadata": {
                "keep": "body"
            }
        });

        let (route, body_mutated) = take_codey_route_metadata(&mut request, &mut body).unwrap();

        assert_eq!(route.as_deref(), Some("route-a"));
        assert!(!body_mutated);
        assert_eq!(body["client_metadata"]["keep"], "body");
    }

    #[test]
    fn native_responses_passthrough_skips_reserialize_when_unmodified() {
        assert!(should_passthrough_native_responses(
            ProtocolBridge::NativeResponses,
            "gpt-5.4",
            "gpt-5.4",
            false,
        ));
        assert!(!should_passthrough_native_responses(
            ProtocolBridge::NativeResponses,
            "alias/gpt-5.4",
            "gpt-5.4",
            false,
        ));
        assert!(!should_passthrough_native_responses(
            ProtocolBridge::NativeResponses,
            "gpt-5.4",
            "gpt-5.4",
            true,
        ));
        assert!(!should_passthrough_native_responses(
            ProtocolBridge::ResponsesToChatCompletions,
            "gpt-5.4",
            "gpt-5.4",
            false,
        ));
    }

    #[test]
    fn codey_synthetic_previous_response_ids_are_not_forwardable() {
        assert!(is_codey_synthetic_response_id("resp_codey_local"));
        assert!(is_codey_synthetic_response_id(" resp_codey_local "));
        assert!(!is_codey_synthetic_response_id("resp_real_upstream"));
        assert!(!is_codey_synthetic_response_id("resp-1"));

        let mut synthetic = json!({
            "model": "gpt-5.4",
            "input": "continue",
            "previous_response_id": "resp_codey_wrapped",
        });
        assert!(remove_codey_synthetic_previous_response_id(&mut synthetic));
        assert!(synthetic.get("previous_response_id").is_none());

        let mut upstream = json!({
            "model": "gpt-5.4",
            "input": "continue",
            "previous_response_id": "resp_upstream",
        });
        assert!(!remove_codey_synthetic_previous_response_id(&mut upstream));
        assert_eq!(upstream["previous_response_id"], "resp_upstream");
    }

    #[test]
    fn adapted_websocket_continuation_reuses_the_previous_response_context() {
        let mut history = AdaptedResponsesHistory::default();
        let mut first = json!({"input":"hello"});
        assert!(!history.prepare(&mut first));
        let first_output = vec![json!({
            "type":"function_call",
            "call_id":"call-1",
            "name":"lookup",
            "arguments":"{}"
        })];
        history.remember("resp_codey_first", &first_output);

        let tool_output = json!({
            "type":"function_call_output",
            "call_id":"call-1",
            "output":"done"
        });
        let mut continuation = json!({
            "model":"provider-model",
            "input":[tool_output.clone()],
            "previous_response_id":"resp_codey_first"
        });
        assert!(history.prepare(&mut continuation));
        assert!(continuation.get("previous_response_id").is_none());
        assert_eq!(
            continuation["input"],
            json!(["hello", first_output[0].clone(), tool_output])
        );
        let converted = responses_to_chat_completions_body(&continuation).unwrap();
        assert_eq!(converted["messages"][0]["content"], "hello");
        assert_eq!(converted["messages"][1]["tool_calls"][0]["id"], "call-1");
        assert_eq!(converted["messages"][2]["tool_call_id"], "call-1");
    }

    #[test]
    fn native_responses_rewrite_preserves_large_raw_fields() {
        let original = br#"{
            "model" : "route-a/gpt-5.4",
            "input" : [ { "role" : "user", "content" : "keep raw spacing" } ],
            "client_metadata" : {"codey_route":"route-a","keep":"yes"},
            "tools" : [ { "type" : "function", "name" : "lookup" } ]
        }"#;
        let updated = json!({
            "model":"gpt-5.4",
            "input":[{"role":"user","content":"keep raw spacing"}],
            "client_metadata":{"keep":"yes"},
            "tools":[{"type":"function","name":"lookup"}],
        });

        let rewritten = rewrite_native_responses_encoded_body(original, &updated).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&rewritten).unwrap(),
            updated
        );
        let rewritten = String::from_utf8(rewritten).unwrap();
        assert!(rewritten.contains(r#"[ { "role" : "user", "content" : "keep raw spacing" } ]"#));
        assert!(rewritten.contains(r#"[ { "type" : "function", "name" : "lookup" } ]"#));
        assert!(!rewritten.contains(ROUTE_METADATA_KEY));
    }

    #[test]
    fn native_responses_rewrite_adds_a_defaulted_model_and_can_remove_metadata() {
        let original =
            br#"{"input":"hello","previous_response_id":"resp_codey_old","client_metadata":{"codey_route":"route-a"}}"#;
        let updated = json!({"model":"gpt-5.6-sol","input":"hello"});

        let rewritten = rewrite_native_responses_encoded_body(original, &updated).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&rewritten).unwrap(),
            updated
        );
        assert!(
            !String::from_utf8(rewritten)
                .unwrap()
                .contains(ROUTE_METADATA_KEY)
        );
    }

    #[test]
    fn native_responses_rewrite_can_update_previous_response_id() {
        let original = br#"{"model":"gpt-5.4","input":"hello"}"#;
        let updated = json!({
            "model":"gpt-5.4",
            "input":"hello",
            "previous_response_id":"resp_upstream",
        });

        let rewritten = rewrite_native_responses_encoded_body(original, &updated).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&rewritten).unwrap(),
            updated
        );
    }

    #[test]
    fn responses_endpoint_reuses_explicit_responses_url() {
        assert_eq!(
            responses_endpoint("https://relay.example/v1/responses").unwrap(),
            "https://relay.example/v1/responses"
        );
        assert_eq!(
            responses_endpoint("https://relay.example/v1").unwrap(),
            "https://relay.example/v1/responses"
        );
        assert_eq!(
            responses_compact_endpoint("https://relay.example/v1/responses").unwrap(),
            "https://relay.example/v1/responses/compact"
        );
        assert_eq!(
            responses_compact_endpoint("https://relay.example/v1").unwrap(),
            "https://relay.example/v1/responses/compact"
        );
        assert_eq!(
            image_generation_endpoint("https://relay.example/v1/responses").unwrap(),
            "https://relay.example/v1/images/generations"
        );
        assert_eq!(
            image_generation_endpoint("https://relay.example/v1/chat/completions").unwrap(),
            "https://relay.example/v1/images/generations"
        );
    }

    #[test]
    fn chat_completions_endpoint_accepts_root_version_and_explicit_paths() {
        assert_eq!(
            chat_completions_endpoint("https://relay.example").unwrap(),
            "https://relay.example/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_endpoint("https://relay.example/v1").unwrap(),
            "https://relay.example/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_endpoint("https://relay.example/v1/responses").unwrap(),
            "https://relay.example/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_endpoint("https://relay.example/v1/chat/completions").unwrap(),
            "https://relay.example/v1/chat/completions"
        );
    }

    #[test]
    fn anthropic_messages_endpoint_accepts_root_version_and_explicit_paths() {
        assert_eq!(
            anthropic_messages_endpoint("https://api.anthropic.com").unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            anthropic_messages_endpoint("https://relay.example/v1").unwrap(),
            "https://relay.example/v1/messages"
        );
        assert_eq!(
            anthropic_messages_endpoint("https://relay.example/v1/responses").unwrap(),
            "https://relay.example/v1/messages"
        );
        assert_eq!(
            anthropic_messages_endpoint("https://relay.example/v1/messages").unwrap(),
            "https://relay.example/v1/messages"
        );
    }

    #[test]
    fn responses_request_converts_messages_tools_images_and_structured_output_to_chat() {
        let chat = responses_to_chat_completions_body(&json!({
            "model": "provider-model",
            "instructions": "Be concise",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [
                        {"type":"input_text","text":"inspect"},
                        {"type":"input_image","image_url":"https://example.invalid/a.png","detail":"low"}
                    ]
                },
                {"type":"function_call","call_id":"call-1","name":"lookup","arguments":"{\"q\":1}"},
                {"type":"function_call_output","call_id":"call-1","output":"done"}
            ],
            "tools": [{
                "type": "function",
                "name": "lookup",
                "description": "lookup data",
                "parameters": {"type":"object"},
                "strict": true
            }],
            "tool_choice": {"type":"function","name":"lookup"},
            "parallel_tool_calls": true,
            "reasoning": {"effort":"high"},
            "text": {"format": {
                "type": "json_schema",
                "name": "answer",
                "schema": {"type":"object"},
                "strict": true
            }},
            "stream": true
        }))
        .unwrap();

        assert_eq!(chat["model"], "provider-model");
        assert_eq!(chat["messages"][0]["role"], "system");
        assert_eq!(chat["messages"][1]["content"][1]["type"], "image_url");
        assert_eq!(chat["messages"][2]["tool_calls"][0]["id"], "call-1");
        assert_eq!(chat["messages"][3]["tool_call_id"], "call-1");
        assert_eq!(chat["tools"][0]["function"]["name"], "lookup");
        assert_eq!(chat["tool_choice"]["function"]["name"], "lookup");
        assert_eq!(chat["reasoning_effort"], "high");
        assert_eq!(chat["response_format"]["type"], "json_schema");
        assert_eq!(chat["response_format"]["json_schema"]["name"], "answer");
        assert_eq!(chat["stream_options"]["include_usage"], true);
    }

    #[test]
    fn responses_tool_output_images_remain_visible_in_fallback_protocols() {
        let body = json!({
            "model":"provider-model",
            "input":[
                {"type":"function_call","call_id":"call-1","name":"inspect","arguments":"{}"},
                {
                    "type":"function_call_output",
                    "call_id":"call-1",
                    "output":[
                        {"type":"input_text","text":"rendered image:"},
                        {"type":"input_image","image_url":"data:image/png;base64,aGVsbG8="}
                    ]
                }
            ],
            "tools":[{
                "type":"function",
                "name":"inspect",
                "parameters":{"type":"object"}
            }]
        });

        let chat = responses_to_chat_completions_body(&body).unwrap();
        assert_eq!(chat["messages"][1]["role"], "tool");
        assert_eq!(chat["messages"][1]["content"], "rendered image:");
        assert_eq!(chat["messages"][2]["role"], "user");
        assert_eq!(chat["messages"][2]["content"][0]["type"], "image_url");
        assert_eq!(
            chat["messages"][2]["content"][0]["image_url"]["url"],
            "data:image/png;base64,aGVsbG8="
        );

        let anthropic = responses_to_anthropic_messages_body(&body).unwrap();
        assert_eq!(anthropic["messages"][1]["role"], "user");
        assert_eq!(
            anthropic["messages"][1]["content"][0]["type"],
            "tool_result"
        );
        assert_eq!(
            anthropic["messages"][1]["content"][0]["content"],
            "rendered image:"
        );
        assert_eq!(anthropic["messages"][1]["content"][1]["type"], "image");
        assert_eq!(
            anthropic["messages"][1]["content"][1]["source"]["data"],
            "aGVsbG8="
        );
    }

    #[test]
    fn configured_duplicate_function_tools_are_deduplicated_without_merging_conflicts() {
        let string_lookup = json!({
            "type":"function",
            "name":"lookup",
            "description":"lookup data",
            "parameters":{"type":"object","properties":{"id":{"type":"string"}}},
            "strict":true
        });
        let number_lookup = json!({
            "type":"function",
            "name":"lookup",
            "description":"lookup data",
            "parameters":{"type":"object","properties":{"id":{"type":"number"}}},
            "strict":true
        });
        let body = json!({
            "model":"provider-model",
            "input":"hello",
            "tools":[string_lookup.clone(), string_lookup, number_lookup]
        });

        let chat = responses_to_chat_completions_body(&body).unwrap();
        let anthropic = responses_to_anthropic_messages_body(&body).unwrap();

        assert_eq!(chat["tools"].as_array().unwrap().len(), 2);
        assert_eq!(anthropic["tools"].as_array().unwrap().len(), 2);
        assert_eq!(
            chat["tools"][0]["function"]["parameters"]["properties"]["id"]["type"],
            "string"
        );
        assert_eq!(
            chat["tools"][1]["function"]["parameters"]["properties"]["id"]["type"],
            "number"
        );
    }

    #[test]
    fn additional_tools_are_merged_without_becoming_chat_or_anthropic_messages() {
        let body = json!({
            "model":"provider-model",
            "tools":[{
                "type":"function",
                "name":"always_available",
                "parameters":{"type":"object","properties":{}}
            }],
            "input":[
                {"type":"message","role":"user","content":"hello"},
                {
                    "type":"additional_tools",
                    "role":"developer",
                    "tools":[{
                        "type":"function",
                        "name":"loaded_later",
                        "description":"Loaded at this point in the Responses input",
                        "parameters":{"type":"object","properties":{"q":{"type":"string"}}}
                    }]
                }
            ]
        });

        let chat = responses_to_chat_completions_body(&body).unwrap();
        let anthropic = responses_to_anthropic_messages_body(&body).unwrap();

        assert_eq!(chat["messages"].as_array().unwrap().len(), 1);
        assert_eq!(chat["messages"][0]["content"], "hello");
        assert_eq!(chat["tools"].as_array().unwrap().len(), 2);
        assert_eq!(chat["tools"][1]["function"]["name"], "loaded_later");
        assert_eq!(anthropic["messages"].as_array().unwrap().len(), 1);
        assert_eq!(anthropic["messages"][0]["content"][0]["text"], "hello");
        assert_eq!(anthropic["tools"].as_array().unwrap().len(), 2);
        assert_eq!(anthropic["tools"][1]["name"], "loaded_later");
    }

    #[test]
    fn additional_tools_reject_conflicting_function_definitions() {
        let error = responses_to_anthropic_messages_body(&json!({
            "model":"provider-model",
            "tools":[{
                "type":"function",
                "name":"lookup",
                "parameters":{"type":"object","properties":{"id":{"type":"string"}}}
            }],
            "input":[
                {"role":"user","content":"hello"},
                {
                    "type":"additional_tools",
                    "role":"developer",
                    "tools":[{
                        "type":"function",
                        "name":"lookup",
                        "parameters":{"type":"object","properties":{"id":{"type":"number"}}}
                    }]
                }
            ]
        }))
        .unwrap_err();

        assert!(error.to_string().contains("定义冲突的工具 function/lookup"));
    }

    #[test]
    fn additional_tools_require_the_developer_role() {
        let error = responses_to_chat_completions_body(&json!({
            "model":"provider-model",
            "input":[
                {"role":"user","content":"hello"},
                {"type":"additional_tools","role":"user","tools":[]}
            ]
        }))
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("additional_tools.role 必须是 developer")
        );
    }

    #[test]
    fn agent_message_items_convert_to_assistant_history() {
        let body = json!({
            "model":"provider-model",
            "input":[
                {"role":"user","content":"inspect the screenshot"},
                {"type":"agent_message","message":"The visual worker saw an error banner."},
                {"type":"agent_message","content":[{"type":"output_text","text":"The selected route is Chat Completions."}]}
            ]
        });

        let chat = responses_to_chat_completions_body(&body).unwrap();
        assert_eq!(
            chat["messages"],
            json!([
                {"role":"user","content":"inspect the screenshot"},
                {"role":"assistant","content":"The visual worker saw an error banner."},
                {"role":"assistant","content":"The selected route is Chat Completions."}
            ])
        );

        let anthropic = responses_to_anthropic_messages_body(&body).unwrap();
        assert_eq!(anthropic["messages"][1]["role"], "assistant");
        assert_eq!(
            anthropic["messages"][1]["content"],
            json!([
                {"type":"text","text":"The visual worker saw an error banner."},
                {"type":"text","text":"The selected route is Chat Completions."}
            ])
        );
    }

    #[test]
    fn opaque_responses_content_parts_are_ignored_during_chat_fallback_conversion() {
        let body = json!({
            "model":"provider-model",
            "input":[
                {"role":"user","content":"inspect the screenshot"},
                {
                    "type":"agent_message",
                    "message":"The agent kept visible fallback text.",
                    "content":[
                        {"type":"encrypted_content","encrypted_content":"opaque-only-agent-state"}
                    ]
                },
                {
                    "type":"agent_message",
                    "message":"Single object fallback text.",
                    "content":{"type":"encrypted_content","encrypted_content":"opaque-single-agent-state"}
                },
                {
                    "type":"agent_message",
                    "content":[
                        {"type":"encrypted_content","encrypted_content":"opaque-agent-state"},
                        {"type":"output_text","text":"The visual worker saw an error banner."}
                    ]
                },
                {
                    "type":"message",
                    "role":"assistant",
                    "content":[
                        {"type":"encrypted_content","encrypted_content":"opaque-assistant-state"},
                        {"type":"refusal","refusal":"I cannot inspect that file."}
                    ]
                },
                {
                    "role":"user",
                    "content":[
                        {"encrypted_content":"opaque-user-state"},
                        {"type":"reasoning","encrypted_content":"opaque-reasoning-part"},
                        {"type":"compaction","encrypted_content":"opaque-compaction-part"},
                        {"type":"input_text","text":"try again"}
                    ]
                },
                {
                    "role":"user",
                    "content":{"type":"encrypted_content","encrypted_content":"opaque-single-user-state"}
                }
            ]
        });

        let chat = responses_to_chat_completions_body(&body).unwrap();
        assert_eq!(
            chat["messages"],
            json!([
                {"role":"user","content":"inspect the screenshot"},
                {"role":"assistant","content":"The agent kept visible fallback text."},
                {"role":"assistant","content":"Single object fallback text."},
                {"role":"assistant","content":"The visual worker saw an error banner."},
                {"role":"assistant","content":"I cannot inspect that file."},
                {"role":"user","content":[{"type":"text","text":"try again"}]}
            ])
        );
        assert!(!chat.to_string().contains("opaque"));

        let anthropic = responses_to_anthropic_messages_body(&body).unwrap();
        assert!(!anthropic.to_string().contains("opaque"));
        assert_eq!(anthropic["messages"][1]["content"][0]["type"], "text");
        assert_eq!(
            anthropic["messages"][1]["content"][0]["text"],
            "The agent kept visible fallback text."
        );
        assert_eq!(
            anthropic["messages"][1]["content"][1]["text"],
            "Single object fallback text."
        );
        assert_eq!(
            anthropic["messages"][1]["content"][2]["text"],
            "The visual worker saw an error banner."
        );
        assert_eq!(
            anthropic["messages"][1]["content"][3]["text"],
            "I cannot inspect that file."
        );
    }

    #[test]
    fn nonportable_responses_history_items_are_ignored_during_chat_fallback_conversion() {
        let body = json!({
            "model":"provider-model",
            "input":[
                {"role":"user","content":"continue"},
                {"type":"reasoning","id":"rs_1","encrypted_content":"opaque-reasoning"},
                {"type":"compaction","id":"cmp_1","encrypted_content":"opaque-window"},
                {
                    "type":"web_search_call",
                    "id":"ws_1",
                    "status":"completed",
                    "action":{"type":"search","query":"provider-side query"}
                }
            ]
        });

        let chat = responses_to_chat_completions_body(&body).unwrap();
        assert_eq!(
            chat["messages"],
            json!([{"role":"user","content":"continue"}])
        );
        assert!(!chat.to_string().contains("opaque"));
        assert!(!chat.to_string().contains("provider-side query"));

        let missing = responses_to_chat_completions_body(&json!({
            "model":"provider-model",
            "input":[{"type":"compaction","encrypted_content":"opaque-window"}]
        }))
        .unwrap_err();
        assert!(missing.to_string().contains("缺少可转换"));

        let trigger = responses_to_chat_completions_body(&json!({
            "model":"provider-model",
            "input":[
                {"role":"user","content":"full context"},
                {"type":"compaction_trigger"}
            ]
        }))
        .unwrap_err();
        assert!(trigger.to_string().contains("compaction_trigger"));

        let active_search = responses_to_chat_completions_body(&json!({
            "model":"provider-model",
            "input":[
                {"role":"user","content":"continue"},
                {"type":"web_search_call","status":"in_progress"}
            ]
        }))
        .unwrap_err();
        assert!(active_search.to_string().contains("web_search_call"));
    }

    #[test]
    fn namespace_tools_expand_to_stable_unique_function_names_and_choices() {
        let body = json!({
            "model":"provider-model",
            "input":"hello",
            "tools":[{
                "type":"namespace",
                "name":"mcp__codey_fastctx",
                "tools":[{
                    "type":"function",
                    "name":"grep",
                    "description":"search content",
                    "parameters":{"type":"object","properties":{"pattern":{"type":"string"}}}
                }],
                "children":[{
                    "type":"namespace",
                    "name":"files",
                    "tools":[{
                        "type":"function",
                        "name":"inspect_local_file_with_an_extremely_long_leaf_name",
                        "parameters":{"type":"object","properties":{"path":{"type":"string"}}}
                    }]
                }]
            }],
            "tool_choice":{
                "type":"function",
                "namespace":"mcp__codey_fastctx.files",
                "name":"inspect_local_file_with_an_extremely_long_leaf_name"
            },
            "function_call":{"namespace":"mcp__codey_fastctx","name":"grep"}
        });

        let converted = responses_to_chat_completions_request(&body).unwrap();
        let repeated = responses_to_chat_completions_request(&body).unwrap();

        let grep_name = converted.body["tools"][0]["function"]["name"]
            .as_str()
            .unwrap();
        let inspect_name = converted.body["tools"][1]["function"]["name"]
            .as_str()
            .unwrap();
        assert!(grep_name.starts_with(NAMESPACE_UPSTREAM_TOOL_PREFIX));
        assert!(inspect_name.starts_with(NAMESPACE_UPSTREAM_TOOL_PREFIX));
        assert_ne!(grep_name, inspect_name);
        assert_ne!(grep_name, "grep");
        assert!(inspect_name.len() <= UPSTREAM_FUNCTION_NAME_MAX_BYTES);
        assert_eq!(
            repeated.body["tools"][0]["function"]["name"],
            converted.body["tools"][0]["function"]["name"]
        );
        assert_eq!(
            converted.body["tool_choice"]["function"]["name"],
            inspect_name
        );
        assert_eq!(converted.body["function_call"]["name"], grep_name);
    }

    #[test]
    fn namespace_children_alias_accepts_function_tools() {
        let converted = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"hello",
            "tools":[{
                "type":"namespace",
                "name":"mcp_files",
                "children":[{
                    "type":"function",
                    "name":"read",
                    "parameters":{"type":"object","properties":{"path":{"type":"string"}}}
                }]
            }]
        }))
        .unwrap();

        let upstream_name = converted.body["tools"][0]["function"]["name"]
            .as_str()
            .unwrap();
        assert!(upstream_name.starts_with(NAMESPACE_UPSTREAM_TOOL_PREFIX));
        assert_eq!(
            converted
                .tool_bridge
                .restore_upstream_name(upstream_name)
                .unwrap(),
            ResponsesToolName {
                kind: ResponsesToolKind::Function,
                namespace: vec!["mcp_files".to_string()],
                name: "read".to_string(),
            }
        );
    }

    #[test]
    fn namespace_additional_tools_rewrite_historical_function_calls() {
        let converted = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":[
                {"role":"user","content":"hello"},
                {
                    "type":"additional_tools",
                    "role":"developer",
                    "tools":[{
                        "type":"namespace",
                        "name":"fs",
                        "tools":[{
                            "type":"function",
                            "name":"read_file",
                            "parameters":{"type":"object","properties":{"path":{"type":"string"}}}
                        }]
                    }]
                },
                {"type":"function_call","call_id":"call-1","namespace":"fs","name":"read_file","arguments":{"path":"a.txt"}},
                {"type":"message","role":"assistant","tool_calls":[{
                    "id":"call-2",
                    "type":"function",
                    "namespace":"fs",
                    "function":{"name":"read_file","arguments":"{}"}
                }]},
                {"type":"message","role":"assistant","function_call":{"namespace":["fs"],"name":"read_file"}}
            ]
        }))
        .unwrap();

        let upstream_name = converted.body["tools"][0]["function"]["name"]
            .as_str()
            .unwrap();
        assert_eq!(
            converted.body["messages"][1]["tool_calls"][0]["function"]["name"],
            upstream_name
        );
        assert_eq!(
            converted.body["messages"][2]["tool_calls"][0]["function"]["name"],
            upstream_name
        );
        assert_eq!(
            converted.body["messages"][3]["function_call"]["name"],
            upstream_name
        );
    }

    #[test]
    fn custom_tools_wrap_definition_choice_history_and_result() {
        let patch = "*** Begin Patch\n*** Update File: README.md\n@@\n-old\n+new\n*** End Patch";
        let converted = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":[
                {"role":"user","content":"apply it"},
                {"type":"custom_tool_call","call_id":"call-patch","name":"apply_patch","input":patch},
                {"type":"custom_tool_call_output","call_id":"call-patch","output":"Done!"}
            ],
            "tools":[{
                "type":"custom",
                "name":"apply_patch",
                "description":"Apply a patch",
                "format":{"type":"grammar","syntax":"lark","definition":"start: /[\\s\\S]+/"}
            }],
            "tool_choice":{"type":"custom","name":"apply_patch"}
        }))
        .unwrap();

        let upstream_name = converted.body["tools"][0]["function"]["name"]
            .as_str()
            .unwrap();
        assert!(upstream_name.starts_with(CUSTOM_UPSTREAM_TOOL_PREFIX));
        assert!(upstream_name.len() <= UPSTREAM_FUNCTION_NAME_MAX_BYTES);
        assert_eq!(
            converted.body["tools"][0]["function"]["parameters"]["required"],
            json!(["input"])
        );
        assert_eq!(
            converted.body["tools"][0]["function"]["parameters"]["additionalProperties"],
            false
        );
        assert!(
            converted.body["tools"][0]["function"]["description"]
                .as_str()
                .unwrap()
                .contains("\"syntax\":\"lark\"")
        );
        assert_eq!(
            converted.body["tool_choice"]["function"]["name"],
            upstream_name
        );
        assert_eq!(
            converted.body["messages"][1]["tool_calls"][0]["function"]["name"],
            upstream_name
        );
        let wrapped = serde_json::from_str::<Value>(
            converted.body["messages"][1]["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(wrapped["input"], patch);
        assert_eq!(converted.body["messages"][2]["role"], "tool");
        assert_eq!(converted.body["messages"][2]["tool_call_id"], "call-patch");
        assert_eq!(converted.body["messages"][2]["content"], "Done!");

        let anthropic = responses_to_anthropic_messages_body(&json!({
            "model":"provider-model",
            "input":[
                {"role":"user","content":"apply it"},
                {"type":"custom_tool_call","call_id":"call-patch","name":"apply_patch","input":patch},
                {"type":"custom_tool_call_output","call_id":"call-patch","output":"Done!"}
            ],
            "tools":[{"type":"custom","name":"apply_patch"}]
        }))
        .unwrap();
        assert!(
            anthropic["tools"][0]["name"]
                .as_str()
                .unwrap()
                .starts_with(CUSTOM_UPSTREAM_TOOL_PREFIX)
        );
        assert_eq!(
            anthropic["messages"][1]["content"][0]["input"]["input"],
            patch
        );
        assert_eq!(
            anthropic["messages"][2]["content"][0]["type"],
            "tool_result"
        );
        assert_eq!(
            anthropic["messages"][2]["content"][0]["tool_use_id"],
            "call-patch"
        );
    }

    #[test]
    fn custom_response_calls_restore_for_chat_and_anthropic() {
        let converted = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"hello",
            "tools":[{"type":"custom","name":"apply_patch","description":"Apply a patch"}]
        }))
        .unwrap();
        let upstream_name = converted.body["tools"][0]["function"]["name"]
            .as_str()
            .unwrap();
        let raw_input = "*** Begin Patch\n*** End Patch";
        let wrapped = wrap_custom_tool_input(raw_input).unwrap();

        let chat = chat_completion_to_responses_body_with_tool_bridge(
            json!({
                "choices":[{"message":{"role":"assistant","tool_calls":[{
                    "id":"call-chat",
                    "type":"function",
                    "function":{"name":upstream_name,"arguments":wrapped}
                }]},"finish_reason":"tool_calls"}]
            }),
            "provider-model",
            &converted.tool_bridge,
        )
        .unwrap();
        assert_eq!(chat["output"][0]["type"], "custom_tool_call");
        assert_eq!(chat["output"][0]["name"], "apply_patch");
        assert_eq!(chat["output"][0]["input"], raw_input);
        assert!(
            chat["output"][0]["id"]
                .as_str()
                .unwrap()
                .starts_with("ctc_codey_")
        );

        let anthropic = anthropic_message_to_responses_body_with_tool_bridge(
            &json!({
                "type":"message",
                "content":[{
                    "type":"tool_use",
                    "id":"call-anthropic",
                    "name":upstream_name,
                    "input":{"input":raw_input}
                }]
            }),
            "provider-model",
            &converted.tool_bridge,
        )
        .unwrap();
        assert_eq!(anthropic["output"][0]["type"], "custom_tool_call");
        assert_eq!(anthropic["output"][0]["name"], "apply_patch");
        assert_eq!(anthropic["output"][0]["input"], raw_input);
    }

    #[test]
    fn namespace_custom_tools_bridge_for_anthropic_requests_and_responses() {
        let raw_input = "*** Begin Patch\n*** End Patch";
        let converted = responses_to_anthropic_messages_request(&json!({
            "model":"provider-model",
            "input":[
                "apply it",
                {
                    "type":"custom_tool_call",
                    "call_id":"call-patch",
                    "namespace":"workspace",
                    "name":"apply_patch",
                    "input":raw_input
                }
            ],
            "tools":[{
                "type":"namespace",
                "name":"workspace",
                "tools":[{
                    "type":"custom",
                    "name":"apply_patch",
                    "description":"Apply a patch"
                }]
            }],
            "tool_choice":{
                "type":"custom",
                "namespace":"workspace",
                "name":"apply_patch"
            }
        }))
        .unwrap();

        let upstream_name = converted.body["tools"][0]["name"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(upstream_name.starts_with(CUSTOM_UPSTREAM_TOOL_PREFIX));
        assert_eq!(
            converted.body["tool_choice"]["name"],
            Value::String(upstream_name.clone())
        );
        assert_eq!(
            converted.body["messages"][1]["content"][0]["name"],
            Value::String(upstream_name.clone())
        );
        assert_eq!(
            converted.body["messages"][1]["content"][0]["input"]["input"],
            raw_input
        );

        let restored = anthropic_message_to_responses_body_with_tool_bridge(
            &json!({
                "type":"message",
                "content":[{
                    "type":"tool_use",
                    "id":"call-result",
                    "name":upstream_name,
                    "input":{"input":raw_input}
                }]
            }),
            "provider-model",
            &converted.tool_bridge,
        )
        .unwrap();
        assert_eq!(restored["output"][0]["type"], "custom_tool_call");
        assert_eq!(restored["output"][0]["namespace"], "workspace");
        assert_eq!(restored["output"][0]["name"], "apply_patch");
        assert_eq!(restored["output"][0]["input"], raw_input);
    }

    #[test]
    fn custom_tools_deduplicate_identical_definitions_and_reject_conflicts() {
        let definition = json!({
            "type":"custom",
            "name":"apply_patch",
            "format":{"type":"grammar","syntax":"lark","definition":"start: /.+/"}
        });
        let converted = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"hello",
            "tools":[definition.clone(), definition]
        }))
        .unwrap();
        assert_eq!(converted.body["tools"].as_array().unwrap().len(), 1);

        let conflict = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"hello",
            "tools":[
                {"type":"custom","name":"apply_patch","description":"first"},
                {"type":"custom","name":"apply_patch","description":"second"}
            ]
        }))
        .unwrap_err();
        assert!(conflict.to_string().contains("定义冲突"));

        let generated = custom_upstream_tool_name(&[], "apply_patch");
        let collision = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"hello",
            "tools":[
                {"type":"function","name":generated,"parameters":{"type":"object"}},
                {"type":"custom","name":"apply_patch"}
            ]
        }))
        .unwrap_err();
        assert!(collision.to_string().contains("冲突"));
    }

    #[test]
    fn custom_streaming_waits_for_the_complete_json_wrapper() {
        let converted = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"hello",
            "tools":[{"type":"custom","name":"apply_patch"}]
        }))
        .unwrap();
        let upstream_name = converted.body["tools"][0]["function"]["name"]
            .as_str()
            .unwrap();
        let mut stream = ResponsesSseState::new("provider-model", &converted.tool_bridge);

        let events = stream
            .tool_delta(
                0,
                Some("call-patch"),
                Some(upstream_name),
                Some("{\"input\":\"*** Begin"),
                None,
            )
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "response.output_item.added");
        assert_eq!(events[0]["item"]["type"], "custom_tool_call");
        assert_eq!(events[0]["item"]["name"], "apply_patch");
        assert_eq!(events[0]["item"]["input"], "");

        let events = stream
            .tool_delta(0, None, None, Some(" Patch\"}"), None)
            .unwrap();
        assert!(events.is_empty());
        assert_eq!(
            custom_tool_input_from_arguments(
                &stream.tools.get(&0).unwrap().arguments,
                "test custom arguments"
            )
            .unwrap(),
            "*** Begin Patch"
        );
    }

    #[test]
    fn client_tool_search_bridges_to_stable_chat_and_anthropic_functions() {
        let body = json!({
            "model":"provider-model",
            "input":"find a filesystem tool",
            "tools":[client_tool_search_definition()],
            "tool_choice":{"type":"tool_search"}
        });

        let chat = responses_to_chat_completions_request(&body).unwrap();
        let repeated = responses_to_chat_completions_request(&body).unwrap();
        assert_eq!(
            chat.body["tools"][0]["function"]["name"],
            TOOL_SEARCH_UPSTREAM_TOOL_NAME
        );
        assert_eq!(
            repeated.body["tools"][0]["function"]["name"],
            chat.body["tools"][0]["function"]["name"]
        );
        assert_eq!(
            chat.body["tools"][0]["function"]["description"],
            "Search the client tool catalog"
        );
        assert_eq!(
            chat.body["tools"][0]["function"]["parameters"],
            client_tool_search_definition()["parameters"]
        );
        assert_eq!(
            chat.body["tool_choice"]["function"]["name"],
            TOOL_SEARCH_UPSTREAM_TOOL_NAME
        );
        assert_eq!(
            chat.tool_bridge
                .restore_upstream_name(TOOL_SEARCH_UPSTREAM_TOOL_NAME)
                .unwrap(),
            ResponsesToolName::tool_search()
        );

        let anthropic = responses_to_anthropic_messages_request(&body).unwrap();
        assert_eq!(
            anthropic.body["tools"][0]["name"],
            TOOL_SEARCH_UPSTREAM_TOOL_NAME
        );
        assert_eq!(
            anthropic.body["tools"][0]["description"],
            "Search the client tool catalog"
        );
        assert_eq!(
            anthropic.body["tools"][0]["input_schema"],
            client_tool_search_definition()["parameters"]
        );
        assert_eq!(
            anthropic.body["tool_choice"]["name"],
            TOOL_SEARCH_UPSTREAM_TOOL_NAME
        );
    }

    #[test]
    fn client_tool_search_requires_description_and_object_parameters() {
        for tool in [
            json!({
                "type":"tool_search",
                "execution":"client",
                "parameters":{"type":"object"}
            }),
            json!({
                "type":"tool_search",
                "execution":"client",
                "description":"Search the client tool catalog"
            }),
            json!({
                "type":"tool_search",
                "execution":"client",
                "description":"Search the client tool catalog",
                "parameters":[]
            }),
        ] {
            let error = responses_to_chat_completions_request(&json!({
                "model":"provider-model",
                "input":"find a tool",
                "tools":[tool]
            }))
            .unwrap_err();
            assert!(
                error.to_string().contains("tool_search.description")
                    || error.to_string().contains("tool_search.parameters")
            );
        }

        let conflict = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"find a tool",
            "tools":[
                client_tool_search_definition(),
                {
                    "type":"tool_search",
                    "execution":"client",
                    "description":"A conflicting search definition",
                    "parameters":{"type":"object","properties":{}}
                }
            ]
        }))
        .unwrap_err();
        assert!(conflict.to_string().contains("tool_search 存在定义冲突"));
    }

    #[test]
    fn client_tool_search_history_requires_client_execution_and_object_arguments() {
        for output in [
            json!({
                "type":"tool_search_output",
                "call_id":"call-search-history",
                "tools":[]
            }),
            json!({
                "type":"tool_search_output",
                "execution":"server",
                "call_id":"call-search-history",
                "tools":[]
            }),
        ] {
            let error = responses_to_chat_completions_request(&json!({
                "model":"provider-model",
                "tools":[client_tool_search_definition()],
                "input":[output]
            }))
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("tool_search_output 缺少 execution=client")
                    || error
                        .to_string()
                        .contains("tool_search_output.execution=server 不受支持")
            );
        }

        for arguments in [json!([]), json!("not an object")] {
            let error = responses_to_chat_completions_request(&json!({
                "model":"provider-model",
                "tools":[client_tool_search_definition()],
                "input":[{
                    "type":"tool_search_call",
                    "execution":"client",
                    "call_id":"call-search-history",
                    "arguments":arguments
                }]
            }))
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("tool_search_call.arguments 必须是 JSON 对象")
            );
        }
    }

    #[test]
    fn client_tool_search_calls_restore_for_chat_and_anthropic() {
        let converted = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"find a tool",
            "tools":[client_tool_search_definition()]
        }))
        .unwrap();
        let arguments = "{\"goal\":\"filesystem access\"}";

        let chat = chat_completion_to_responses_body_with_tool_bridge(
            json!({
                "choices":[{"message":{"role":"assistant","tool_calls":[{
                    "id":"call-search-chat",
                    "type":"function",
                    "function":{"name":TOOL_SEARCH_UPSTREAM_TOOL_NAME,"arguments":arguments}
                }]},"finish_reason":"tool_calls"}]
            }),
            "provider-model",
            &converted.tool_bridge,
        )
        .unwrap();
        assert_eq!(chat["output"][0]["type"], "tool_search_call");
        assert_eq!(chat["output"][0]["execution"], "client");
        assert_eq!(chat["output"][0]["call_id"], "call-search-chat");
        assert_eq!(chat["output"][0]["status"], "completed");
        assert_eq!(
            chat["output"][0]["arguments"],
            json!({"goal":"filesystem access"})
        );
        assert!(chat["output"][0].get("name").is_none());

        let anthropic = anthropic_message_to_responses_body_with_tool_bridge(
            &json!({
                "type":"message",
                "content":[{
                    "type":"tool_use",
                    "id":"call-search-anthropic",
                    "name":TOOL_SEARCH_UPSTREAM_TOOL_NAME,
                    "input":{"goal":"calendar tools"}
                }]
            }),
            "provider-model",
            &converted.tool_bridge,
        )
        .unwrap();
        assert_eq!(anthropic["output"][0]["type"], "tool_search_call");
        assert_eq!(anthropic["output"][0]["execution"], "client");
        assert_eq!(anthropic["output"][0]["call_id"], "call-search-anthropic");
        assert_eq!(
            anthropic["output"][0]["arguments"],
            json!({"goal":"calendar tools"})
        );
    }

    #[test]
    fn client_tool_search_history_round_trips_and_promotes_loaded_tools() {
        let converted = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "tools":[client_tool_search_definition()],
            "input":[
                {"type":"message","role":"user","content":"find a tool"},
                {
                    "type":"tool_search_call",
                    "execution":"client",
                    "call_id":"call-search-history",
                    "status":"completed",
                    "arguments":{"goal":"read files"}
                },
                {
                    "type":"tool_search_output",
                    "execution":"client",
                    "call_id":"call-search-history",
                    "status":"completed",
                    "tools":[{
                        "type":"function",
                        "name":"read_file",
                        "description":"Read a file",
                        "defer_loading":true,
                        "parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}
                    },{
                        "type":"namespace",
                        "name":"filesystem",
                        "tools":[{
                            "type":"function",
                            "name":"list_files",
                            "description":"List files",
                            "defer_loading":true,
                            "parameters":{"type":"object","properties":{}}
                        }]
                    }]
                }
            ]
        }))
        .unwrap();

        assert_eq!(converted.body["tools"].as_array().unwrap().len(), 3);
        assert_eq!(
            converted.body["tools"][0]["function"]["name"],
            TOOL_SEARCH_UPSTREAM_TOOL_NAME
        );
        assert_eq!(converted.body["tools"][1]["function"]["name"], "read_file");
        assert!(
            converted.body["tools"][1]["function"]
                .get("defer_loading")
                .is_none()
        );
        assert_eq!(
            converted.body["tools"][2]["function"]["name"],
            namespaced_upstream_tool_name(&["filesystem".to_string()], "list_files")
        );
        assert!(
            converted.body["tools"][2]["function"]
                .get("defer_loading")
                .is_none()
        );
        assert_eq!(
            converted.body["messages"][1]["tool_calls"][0]["function"]["name"],
            TOOL_SEARCH_UPSTREAM_TOOL_NAME
        );
        assert_eq!(
            converted.body["messages"][1]["tool_calls"][0]["id"],
            "call-search-history"
        );
        assert_eq!(
            converted.body["messages"][1]["tool_calls"][0]["function"]["arguments"],
            "{\"goal\":\"read files\"}"
        );
        assert_eq!(converted.body["messages"][2]["role"], "tool");
        assert_eq!(
            converted.body["messages"][2]["tool_call_id"],
            "call-search-history"
        );
        let result = serde_json::from_str::<Value>(
            converted.body["messages"][2]["content"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(result["tools"][0]["name"], "read_file");

        let anthropic = responses_to_anthropic_messages_request(&json!({
            "model":"provider-model",
            "tools":[client_tool_search_definition()],
            "input":[
                {"role":"user","content":"find a tool"},
                {"type":"tool_search_call","execution":"client","call_id":"call-search-history","arguments":{"goal":"read files"}},
                {"type":"tool_search_output","execution":"client","call_id":"call-search-history","tools":[{"type":"function","name":"read_file","defer_loading":true,"parameters":{"type":"object"}}]}
            ]
        }))
        .unwrap();
        assert_eq!(anthropic.body["tools"].as_array().unwrap().len(), 2);
        assert_eq!(
            anthropic.body["messages"][1]["content"][0]["type"],
            "tool_use"
        );
        assert_eq!(
            anthropic.body["messages"][1]["content"][0]["input"],
            json!({"goal":"read files"})
        );
        assert_eq!(
            anthropic.body["messages"][2]["content"][0]["type"],
            "tool_result"
        );
    }

    #[test]
    fn client_tool_search_streaming_restores_native_items_without_function_events() {
        let converted = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"find a tool",
            "tools":[client_tool_search_definition()]
        }))
        .unwrap();
        let mut stream = ResponsesSseState::new("provider-model", &converted.tool_bridge);

        let first = stream
            .tool_delta(
                0,
                Some("call-search-stream"),
                Some(TOOL_SEARCH_UPSTREAM_TOOL_NAME),
                Some("{\"goal\":"),
                None,
            )
            .unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0]["type"], "response.output_item.added");
        assert_eq!(first[0]["item"]["type"], "tool_search_call");
        assert_eq!(first[0]["item"]["execution"], "client");
        assert_eq!(first[0]["item"]["arguments"], json!({}));
        assert!(
            !serde_json::to_string(&first)
                .unwrap()
                .contains("function_call_arguments")
        );

        let second = stream
            .tool_delta(0, None, None, Some("\"filesystem\"}"), None)
            .unwrap();
        assert!(second.is_empty());
        let done = stream_tool_item(stream.tools.get(&0).unwrap()).unwrap();
        assert_eq!(done["type"], "tool_search_call");
        assert_eq!(done["execution"], "client");
        assert_eq!(done["call_id"], "call-search-stream");
        assert_eq!(done["arguments"], json!({"goal":"filesystem"}));
    }

    #[test]
    fn responses_web_search_drops_ambient_auto_but_rejects_omitted_selection() {
        let omitted = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"normal conversation",
            "tools":[{"type":"web_search"}]
        }))
        .unwrap_err();
        assert!(omitted.to_string().contains("可选 web_search"));

        let auto = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"normal conversation",
            "tools":[
                {"type":"web_search"},
                {"type":"function","name":"lookup","parameters":{"type":"object","properties":{}}}
            ],
            "tool_choice":"auto"
        }))
        .unwrap();
        assert!(auto.body.get("web_search_options").is_none());
        assert_eq!(auto.body["tool_choice"], "auto");
        assert_eq!(auto.body["tools"].as_array().unwrap().len(), 1);
        assert_eq!(auto.body["tools"][0]["function"]["name"], "lookup");

        let additional = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":[
                {"role":"user","content":"search the web"},
                {"type":"additional_tools","role":"developer","tools":[{"type":"web_search"}]}
            ]
        }))
        .unwrap_err();
        assert!(additional.to_string().contains("可选 web_search"));

        let chat = responses_to_chat_completions_request(&json!({
            "model":"gpt-5-search-api",
            "input":"search the web",
            "tools":[{
                "type":"web_search_preview",
                "search_context_size":"low",
                "user_location":{
                    "type":"approximate",
                    "country":"US",
                    "city":"San Francisco",
                    "region":"California",
                    "timezone":"America/Los_Angeles"
                }
            }],
            "tool_choice":{"type":"web_search_preview"}
        }))
        .unwrap();

        assert!(chat.body.get("tools").is_none());
        assert!(chat.body.get("tool_choice").is_none());
        assert_eq!(
            chat.body["web_search_options"],
            json!({
                "search_context_size":"low",
                "user_location":{
                    "type":"approximate",
                    "approximate":{
                        "country":"US",
                        "city":"San Francisco",
                        "region":"California",
                        "timezone":"America/Los_Angeles"
                    }
                }
            })
        );
    }

    #[test]
    fn responses_web_search_required_only_maps_and_required_mixed_fails_closed() {
        let required = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"search the web",
            "tools":[{"type":"web_search","search_context_size":"medium"}],
            "tool_choice":"required"
        }))
        .unwrap();
        assert_eq!(
            required.body["web_search_options"],
            json!({"search_context_size":"medium"})
        );
        assert!(required.body.get("tools").is_none());
        assert!(required.body.get("tool_choice").is_none());

        let error = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"use a required tool",
            "tools":[
                {"type":"web_search"},
                {"type":"function","name":"lookup","parameters":{"type":"object","properties":{}}}
            ],
            "tool_choice":"required"
        }))
        .unwrap_err();
        assert!(error.to_string().contains("tool_choice=required"));
        assert!(error.to_string().contains("无法无损表达"));

        let explicit_mixed = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"search the web",
            "tools":[
                {"type":"web_search"},
                {"type":"function","name":"lookup","parameters":{"type":"object","properties":{}}}
            ],
            "tool_choice":{"type":"web_search"}
        }))
        .unwrap_err();
        assert!(explicit_mixed.to_string().contains("仍包含其他工具"));

        let sources = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"search the web",
            "tools":[{"type":"web_search"}],
            "tool_choice":{"type":"web_search"},
            "include":["web_search_call.action.sources"]
        }))
        .unwrap_err();
        assert!(sources.to_string().contains("action.sources"));
    }

    #[test]
    fn responses_explicit_web_search_requires_a_declared_search_tool() {
        let error = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"search the web",
            "tool_choice":{"type":"web_search"}
        }))
        .unwrap_err();
        assert!(error.to_string().contains("未在 tools 中声明的 web_search"));
    }

    #[test]
    fn responses_web_search_respects_none_and_function_tool_choice() {
        let tools = json!([
            {"type":"web_search"},
            {"type":"function","name":"lookup","parameters":{"type":"object","properties":{}}}
        ]);
        let none = responses_to_chat_completions_request(&json!({
            "model":"gpt-5-search-api",
            "input":"search disabled",
            "tools":tools,
            "tool_choice":"none"
        }))
        .unwrap();
        assert!(none.body.get("web_search_options").is_none());
        assert_eq!(none.body["tool_choice"], "none");
        assert_eq!(none.body["tools"].as_array().unwrap().len(), 1);

        let function = responses_to_chat_completions_request(&json!({
            "model":"gpt-5-search-api",
            "input":"call lookup",
            "tools":tools,
            "tool_choice":{"type":"function","name":"lookup"}
        }))
        .unwrap();
        assert!(function.body.get("web_search_options").is_none());
        assert_eq!(function.body["tool_choice"]["function"]["name"], "lookup");
    }

    #[test]
    fn responses_web_search_rejects_unsupported_chat_options_and_anthropic() {
        for tool in [
            json!({"type":"web_search","filters":{"allowed_domains":["example.com"]}}),
            json!({"type":"web_search","return_token_budget":2048}),
            json!({"type":"web_search","external_web_access":false}),
            json!({"type":"web_search","search_context_size":"tiny"}),
        ] {
            let error = responses_to_chat_completions_request(&json!({
                "model":"gpt-5-search-api",
                "input":"search",
                "tools":[tool]
            }))
            .unwrap_err();
            assert!(
                error.to_string().contains("web_search")
                    || error.to_string().contains("external_web_access")
            );
        }

        let error = responses_to_anthropic_messages_request(&json!({
            "model":"claude-test",
            "input":"search",
            "tools":[{"type":"web_search"}],
            "tool_choice":{"type":"web_search"}
        }))
        .unwrap_err();
        assert!(
            error.to_string().contains("web_search_options")
                || error.to_string().contains("支持 Responses 的线路")
        );

        let error = responses_to_anthropic_messages_request(&json!({
            "model":"claude-test",
            "input":"search",
            "tools":[{"type":"web_search"}]
        }))
        .unwrap_err();
        assert!(error.to_string().contains("可选 web_search"));

        let ambient = responses_to_anthropic_messages_request(&json!({
            "model":"claude-test",
            "input":"normal conversation",
            "tools":[{"type":"web_search"}],
            "tool_choice":"auto"
        }))
        .unwrap();
        assert!(ambient.body.get("tools").is_none());
        assert!(ambient.body.get("tool_choice").is_none());
    }

    #[test]
    fn chat_search_annotations_are_preserved_in_responses_output_text() {
        let body = chat_completion_to_responses_body(
            json!({
                "id":"chatcmpl-search",
                "created":123,
                "choices":[{
                    "message":{
                        "role":"assistant",
                        "content":"OpenAI released a search model.",
                        "annotations":[{
                            "type":"url_citation",
                            "url_citation":{
                                "url":"https://openai.com/",
                                "title":"OpenAI",
                                "start_index":0,
                                "end_index":6
                            }
                        }]
                    },
                    "finish_reason":"stop"
                }]
            }),
            "gpt-5-search-api",
        )
        .unwrap();

        assert_eq!(
            body["output"][0]["content"][0]["annotations"][0]["url_citation"]["url"],
            "https://openai.com/"
        );
    }

    #[test]
    fn namespace_tools_fail_closed_on_conflicts_and_unsupported_tool_kinds() {
        let duplicate = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"hello",
            "tools":[{
                "type":"namespace",
                "name":"fs",
                "tools":[
                    {"type":"function","name":"read","parameters":{"type":"object","properties":{"path":{"type":"string"}}}},
                    {"type":"function","name":"read","parameters":{"type":"object","properties":{"path":{"type":"number"}}}}
                ]
            }]
        }))
        .unwrap_err();
        assert!(duplicate.to_string().contains("存在定义冲突"));

        for tool in [
            json!({"type":"tool_search"}),
            json!({"type":"tool_search","execution":"server"}),
        ] {
            let error = responses_to_chat_completions_request(&json!({
                "model":"provider-model",
                "input":"hello",
                "tools":[tool]
            }))
            .unwrap_err();
            assert!(
                error.to_string().contains("支持 Responses 的线路")
                    || error.to_string().contains("仅 execution=client 可桥接")
            );
        }
    }

    #[test]
    fn namespace_expansion_rejects_collisions_with_plain_function_names() {
        let generated = namespaced_upstream_tool_name(&["fs".to_string()], "read");

        let error = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"hello",
            "tools":[
                {"type":"function","name":generated,"parameters":{"type":"object","properties":{}}},
                {"type":"namespace","name":"fs","tools":[
                    {"type":"function","name":"read","parameters":{"type":"object","properties":{}}}
                ]}
            ]
        }))
        .unwrap_err();

        assert!(error.to_string().contains("冲突"));
    }

    #[test]
    fn namespace_response_names_restore_for_chat_and_anthropic_outputs() {
        let converted = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"hello",
            "tools":[{"type":"namespace","name":"fs","tools":[
                {"type":"function","name":"read","parameters":{"type":"object","properties":{}}}
            ]}]
        }))
        .unwrap();
        let upstream_name = converted.body["tools"][0]["function"]["name"]
            .as_str()
            .unwrap();

        let chat = chat_completion_to_responses_body_with_tool_bridge(
            json!({
                "choices":[{"message":{"role":"assistant","tool_calls":[{
                    "id":"call-chat",
                    "type":"function",
                    "function":{"name":upstream_name,"arguments":"{\"path\":\"a.txt\"}"}
                }]},"finish_reason":"tool_calls"}]
            }),
            "provider-model",
            &converted.tool_bridge,
        )
        .unwrap();
        assert_eq!(chat["output"][0]["name"], "read");
        assert_eq!(chat["output"][0]["namespace"], "fs");

        let anthropic = anthropic_message_to_responses_body_with_tool_bridge(
            &json!({
                "type":"message",
                "content":[{"type":"tool_use","id":"call-anthropic","name":upstream_name,"input":{"path":"a.txt"}}]
            }),
            "provider-model",
            &converted.tool_bridge,
        )
        .unwrap();
        assert_eq!(anthropic["output"][0]["name"], "read");
        assert_eq!(anthropic["output"][0]["namespace"], "fs");
    }

    #[test]
    fn namespace_streaming_restores_added_and_done_items_after_complete_name() {
        let converted = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"hello",
            "tools":[{"type":"namespace","name":"fs","tools":[
                {"type":"function","name":"read","parameters":{"type":"object","properties":{}}}
            ]}]
        }))
        .unwrap();
        let upstream_name = converted.body["tools"][0]["function"]["name"]
            .as_str()
            .unwrap();
        let split = 5;
        let mut stream = ResponsesSseState::new("provider-model", &converted.tool_bridge);

        let first = stream
            .tool_delta(
                0,
                Some("call-stream"),
                Some(&upstream_name[..split]),
                Some("{"),
                None,
            )
            .unwrap();
        assert!(first.is_empty());

        let second = stream
            .tool_delta(
                0,
                None,
                Some(&upstream_name[split..]),
                Some("\"path\":\"a.txt\"}"),
                None,
            )
            .unwrap();
        assert_eq!(second[0]["type"], "response.output_item.added");
        assert_eq!(second[0]["item"]["name"], "read");
        assert_eq!(second[0]["item"]["namespace"], "fs");
        assert_eq!(second[1]["delta"], "{\"path\":\"a.txt\"}");

        let done = stream_tool_item(stream.tools.get(&0).unwrap()).unwrap();
        assert_eq!(done["name"], "read");
        assert_eq!(done["namespace"], "fs");
        assert_eq!(done["arguments"], "{\"path\":\"a.txt\"}");
    }

    #[test]
    fn streaming_waits_for_a_complete_declared_function_name() {
        let converted = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"hello",
            "tools":[
                {"type":"function","name":"look","parameters":{"type":"object"}},
                {"type":"function","name":"lookup","parameters":{"type":"object"}}
            ]
        }))
        .unwrap();
        let mut stream = ResponsesSseState::new("provider-model", &converted.tool_bridge);

        let first = stream
            .tool_delta(0, Some("call-stream"), Some("look"), None, None)
            .unwrap();
        assert!(first.is_empty());

        let second = stream
            .tool_delta(0, None, Some("up"), Some("{}"), None)
            .unwrap();
        assert_eq!(second[0]["type"], "response.output_item.added");
        assert_eq!(second[0]["item"]["name"], "lookup");
        assert_eq!(second[1]["delta"], "{}");
    }

    #[test]
    fn chat_response_converts_parallel_tools_and_usage_to_responses() {
        let responses = chat_completion_to_responses_body(
            json!({
                "id": "chatcmpl-1",
                "created": 123,
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "checking",
                        "tool_calls": [
                            {"id":"call-a","type":"function","function":{"name":"first","arguments":"{\"a\":1}"}},
                            {"id":"call-b","type":"function","function":{"name":"second","arguments":"{\"b\":2}"}}
                        ]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5,
                    "total_tokens": 15,
                    "prompt_tokens_details": {"cached_tokens": 3},
                    "completion_tokens_details": {"reasoning_tokens": 2}
                }
            }),
            "provider-model",
        )
        .unwrap();

        assert_eq!(responses["id"], "chatcmpl-1");
        assert_eq!(responses["model"], "provider-model");
        assert_eq!(responses["output"][0]["content"][0]["text"], "checking");
        assert_eq!(responses["output"][1]["type"], "function_call");
        assert_eq!(responses["output"][1]["call_id"], "call-a");
        assert_eq!(responses["output"][2]["name"], "second");
        assert_eq!(responses["usage"]["input_tokens"], 10);
        assert_eq!(
            responses["usage"]["input_tokens_details"]["cached_tokens"],
            3
        );
        assert_eq!(
            responses["usage"]["output_tokens_details"]["reasoning_tokens"],
            2
        );
    }

    #[test]
    fn responses_request_converts_messages_images_tools_and_results_to_anthropic() {
        let anthropic = responses_to_anthropic_messages_body(&json!({
            "model":"claude-sonnet-test",
            "instructions":"Be concise",
            "input":[
                {
                    "type":"message",
                    "role":"user",
                    "content":[
                        {"type":"input_text","text":"inspect"},
                        {"type":"input_image","image_url":"data:image/png;base64,aGVsbG8="}
                    ]
                },
                {"type":"function_call","call_id":"call-1","name":"lookup","arguments":"{\"q\":1}"},
                {"type":"function_call_output","call_id":"call-1","output":"done"}
            ],
            "tools":[{
                "type":"function",
                "name":"lookup",
                "description":"lookup data",
                "parameters":{"type":"object","properties":{"q":{"type":"number"}}}
            }],
            "tool_choice":{"type":"function","name":"lookup"},
            "parallel_tool_calls":false,
            "reasoning":{"effort":"high"},
            "max_output_tokens":2048,
            "stream":true
        }))
        .unwrap();

        assert_eq!(anthropic["system"], "Be concise");
        assert_eq!(anthropic["model"], "claude-sonnet-test");
        assert_eq!(anthropic["max_tokens"], 2048);
        assert_eq!(anthropic["messages"][0]["role"], "user");
        assert_eq!(
            anthropic["messages"][0]["content"][1]["source"]["type"],
            "base64"
        );
        assert_eq!(anthropic["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(anthropic["messages"][1]["content"][0]["input"]["q"], 1);
        assert_eq!(
            anthropic["messages"][2]["content"][0]["type"],
            "tool_result"
        );
        assert_eq!(anthropic["tools"][0]["name"], "lookup");
        assert_eq!(anthropic["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(anthropic["tool_choice"]["type"], "tool");
        assert_eq!(anthropic["tool_choice"]["name"], "lookup");
        assert_eq!(anthropic["tool_choice"]["disable_parallel_tool_use"], true);
        assert_eq!(anthropic["output_config"]["effort"], "high");
        assert_eq!(anthropic["stream"], true);
    }

    #[test]
    fn anthropic_response_converts_text_tools_and_cache_usage_to_responses() {
        let responses = anthropic_message_to_responses_body(
            &json!({
                "id":"msg-anthropic",
                "type":"message",
                "role":"assistant",
                "model":"claude-sonnet-test",
                "content":[
                    {"type":"thinking","thinking":"must stay private"},
                    {"type":"text","text":"checking"},
                    {"type":"tool_use","id":"tool-1","name":"lookup","input":{"q":1}}
                ],
                "stop_reason":"tool_use",
                "usage":{
                    "input_tokens":10,
                    "cache_creation_input_tokens":2,
                    "cache_read_input_tokens":3,
                    "output_tokens":5
                }
            }),
            "fallback-model",
        )
        .unwrap();

        assert_eq!(responses["model"], "claude-sonnet-test");
        assert_eq!(responses["output_text"], "checking");
        assert_eq!(responses["output"][0]["content"][0]["text"], "checking");
        assert_eq!(responses["output"][1]["type"], "function_call");
        assert_eq!(responses["output"][1]["call_id"], "tool-1");
        assert_eq!(responses["output"][1]["arguments"], "{\"q\":1}");
        assert!(!responses.to_string().contains("must stay private"));
        assert_eq!(responses["usage"]["input_tokens"], 15);
        assert_eq!(
            responses["usage"]["input_tokens_details"]["cached_tokens"],
            3
        );
        assert_eq!(responses["usage"]["total_tokens"], 20);
    }

    #[test]
    fn anthropic_sse_accumulates_text_tool_arguments_and_usage() {
        let sse = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-stream\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-sonnet-test\",\"content\":[],\"usage\":{\"input_tokens\":4}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool-stream\",\"name\":\"lookup\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":1}\"}}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":3}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );

        let message = parse_anthropic_message_sse_bytes(sse.as_bytes(), "fallback").unwrap();
        let responses = anthropic_message_to_responses_body(&message, "fallback").unwrap();

        assert_eq!(responses["output_text"], "hello");
        assert_eq!(responses["output"][1]["call_id"], "tool-stream");
        assert_eq!(responses["output"][1]["arguments"], "{\"q\":1}");
        assert_eq!(responses["usage"]["input_tokens"], 4);
        assert_eq!(responses["usage"]["output_tokens"], 3);
    }

    #[test]
    fn upstream_error_summary_extracts_context_and_redacts_route_credentials() {
        let (config, provider_id, _) = router_config("https://relay.example/v1".into());
        let snapshot = RouterSnapshot::from_config(&config);
        let route = snapshot.routes.get(&provider_id).unwrap();
        let summary = upstream_error_summary(
            &json!({
                "error": {
                    "message":"image is too large for sk-upstream\nplease resize it",
                    "type":"invalid_request_error",
                    "code":"image_too_large"
                }
            }),
            route,
        );

        assert_eq!(
            summary.message.as_deref(),
            Some("image is too large for *** please resize it")
        );
        assert_eq!(summary.error_type.as_deref(), Some("invalid_request_error"));
        assert_eq!(summary.code.as_deref(), Some("image_too_large"));
        assert_eq!(
            upstream_error_detail(&summary).as_deref(),
            Some(
                "image is too large for *** please resize it（类型：invalid_request_error；代码：image_too_large）"
            )
        );
    }

    #[test]
    fn websocket_failure_event_keeps_provider_codes_and_adds_route_context() {
        let (config, provider_id, _) = router_config("https://relay.example/v1".into());
        let snapshot = RouterSnapshot::from_config(&config);
        let route = snapshot.routes.get(&provider_id).unwrap();
        let mut event = json!({
            "type":"response.failed",
            "response":{
                "error":{
                    "message":"openai_error",
                    "type":"bad_response_status_code",
                    "code":"bad_response_status_code"
                }
            }
        });

        let error_summary = annotate_upstream_websocket_failure(
            &mut event,
            route,
            "provider-model",
            "wss://relay.example/v1/responses",
        );

        let error = &event["response"]["error"];
        assert_eq!(error["type"], "bad_response_status_code");
        assert_eq!(error["code"], "bad_response_status_code");
        let message = error["message"].as_str().unwrap();
        assert!(message.contains("线路「Relay」"));
        assert!(message.contains("provider-model"));
        assert!(message.contains("openai_error"));
        assert!(message.contains("bad_response_status_code"));
        assert_eq!(
            error_summary.as_deref(),
            Some("openai_error（类型：bad_response_status_code；代码：bad_response_status_code）")
        );
    }

    #[tokio::test]
    async fn native_responses_upstream_http_error_is_preserved_as_safe_text() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let body = r#"{"error":{"message":"image exceeds the provider limit for sk-upstream","type":"invalid_request_error","code":"image_too_large"}}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\nx-request-id: upstream-request-123\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            request.path
        });
        let (config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1/responses"));
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();

        let response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({
                "model": model_alias(&provider_id, &model),
                "input":"inspect the image",
                "stream":true
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
        assert!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("text/plain")
        );
        let body = response.text().await.unwrap();
        assert!(body.contains("线路「Relay」"));
        assert!(body.contains("provider-model"));
        assert!(body.contains("HTTP 429"));
        assert!(body.contains("image exceeds the provider limit for ***"));
        assert!(body.contains("image_too_large"));
        assert!(body.contains("上游请求 ID：upstream-request-123"));
        assert!(!body.contains("sk-upstream"));
        assert_eq!(upstream_task.await.unwrap(), "/v1/responses");
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn responses_compact_restores_route_identity_and_proxies_the_window_unchanged() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let compacted_window = br#"{ "model":"provider-model", "input":[{"type":"compaction","encrypted_content":"opaque-window"}] }"#.to_vec();
        let expected_window = compacted_window.clone();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let authorization = incoming_header(&request, "authorization").map(str::to_string);
            let account_id =
                incoming_header(&request, CHATGPT_ACCOUNT_ID_HEADER).map(str::to_string);
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        compacted_window.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            stream.write_all(&compacted_window).await.unwrap();
            (request.path, authorization, account_id, body)
        });
        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1/responses"));
        config.profiles[0].supports_remote_compaction = true;
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        assert!(endpoint.supports_remote_compaction);

        let response = reqwest::Client::new()
            .post(format!("{}/responses/compact", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .header(CHATGPT_ACCOUNT_ID_HEADER, "acct-must-not-leak")
            .json(&json!({
                "model": model_alias(&provider_id, &model),
                "input": [{"role":"user","content":"full context"}],
                "client_metadata": {
                    ROUTE_METADATA_KEY: provider_id,
                    "keep": "metadata"
                }
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.bytes().await.unwrap().as_ref(), expected_window);
        let (path, authorization, account_id, body) = upstream_task.await.unwrap();
        assert_eq!(path, "/v1/responses/compact");
        assert_eq!(authorization.as_deref(), Some("Bearer sk-upstream"));
        assert!(account_id.is_none());
        assert_eq!(body["model"], model);
        assert_eq!(body["client_metadata"]["keep"], "metadata");
        assert!(body["client_metadata"].get(ROUTE_METADATA_KEY).is_none());
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn responses_v2_compaction_trigger_passes_through_the_native_route() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let sse = concat!(
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"compaction\",\"id\":\"cmp_1\",\"encrypted_content\":\"opaque-window\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_compact\",\"object\":\"response\",\"output\":[{\"type\":\"compaction\",\"id\":\"cmp_1\",\"encrypted_content\":\"opaque-window\"}]}}\n\n"
        );
        let expected_sse = sse.to_string();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}",
                        sse.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            (request.path, body)
        });
        let (config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1/responses"));
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();

        let response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({
                "model": model_alias(&provider_id, &model),
                "input": [
                    {"role":"user","content":"full context"},
                    {"type":"compaction_trigger"}
                ],
                "stream": true
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), expected_sse);
        let (path, body) = upstream_task.await.unwrap();
        assert_eq!(path, "/v1/responses");
        assert_eq!(body["model"], model);
        assert_eq!(body["input"][1]["type"], "compaction_trigger");
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn responses_compact_uses_the_chat_conversion_pipeline() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            write_json_response(
                &mut stream,
                200,
                &json!({
                    "id":"chatcmpl-compact",
                    "created":123,
                    "model":body["model"],
                    "choices":[{
                        "message":{"role":"assistant","content":"compacted context"},
                        "finish_reason":"stop"
                    }],
                    "usage":{"prompt_tokens":4,"completion_tokens":2,"total_tokens":6}
                }),
            )
            .await
            .unwrap();
            (request.path, body)
        });
        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].upstream_protocol =
            crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
        config.profiles[0].normalize();
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        assert!(!endpoint.supports_remote_compaction);

        let response = reqwest::Client::new()
            .post(format!("{}/responses/compact", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({
                "model": model_alias(&provider_id, &model),
                "input": "full context"
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            response.json::<Value>().await.unwrap()["output_text"],
            "compacted context"
        );
        let (path, body) = upstream_task.await.unwrap();
        assert_eq!(path, "/v1/chat/completions");
        assert_eq!(body["model"], model);
        assert_eq!(body["messages"][0]["content"], "full context");
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn responses_route_normalizes_tool_schemas_and_passes_web_search_natively() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            let response = json!({
                "id":"resp-search",
                "object":"response",
                "output":[{
                    "type":"message",
                    "role":"assistant",
                    "content":[{
                        "type":"output_text",
                        "text":"search result",
                        "annotations":[]
                    }]
                }]
            })
            .to_string();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            (request.path, body)
        });
        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].upstream_protocol =
            crate::config::UPSTREAM_PROTOCOL_OPENAI_RESPONSES.into();
        config.profiles[0].normalize();
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();

        let response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({
                "model":model_alias(&provider_id, &model),
                "input":"search the web",
                "tools":[
                    {
                        "type":"web_search",
                        "search_context_size":"high",
                        "filters":{"allowed_domains":["example.com"]},
                        "return_token_budget":2048,
                        "external_web_access":false
                    },
                    {
                        "type":"function",
                        "name":"automation_update",
                        "parameters":{
                            "anyOf":[
                                {"type":"object","properties":{"mode":{"const":"view"}},"required":["mode"]},
                                {"oneOf":[{}, {"type":"null"}]},
                                {"type":"null"}
                            ]
                        }
                    },
                    {
                        "type":"function",
                        "name":"read_file",
                        "parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}
                    }
                ]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let (path, body) = upstream_task.await.unwrap();
        assert_eq!(path, "/v1/responses");
        assert_eq!(body["model"], model);
        assert_eq!(body["tools"][0]["type"], "web_search");
        let union = &body["tools"][1]["parameters"];
        assert_eq!(union["type"], "object");
        let branches = union["anyOf"].as_array().unwrap();
        assert_eq!(branches.len(), 2);
        assert!(branches.iter().all(|branch| branch["type"] == "object"));
        assert_eq!(branches[1]["oneOf"].as_array().unwrap().len(), 1);
        assert_eq!(branches[1]["oneOf"][0]["type"], "object");
        assert_eq!(
            body["tools"][2]["parameters"],
            json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]})
        );
        assert_eq!(
            body["tools"][0]["filters"]["allowed_domains"][0],
            "example.com"
        );
        assert_eq!(body["tools"][0]["return_token_budget"], 2048);
        assert_eq!(body["tools"][0]["external_web_access"], false);
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn chat_completions_route_uses_its_path_key_and_returns_responses_sse() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let authorization = incoming_header(&request, "authorization").map(str::to_string);
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            let sse = concat!(
                "data: {\"id\":\"chatcmpl-stream\",\"created\":123,\"model\":\"provider-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hello \",\"tool_calls\":null},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"chatcmpl-stream\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-stream\",\"type\":\"function\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{\\\"q\\\":\"}}]},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"chatcmpl-stream\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"1}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: {\"id\":\"chatcmpl-stream\",\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":3,\"total_tokens\":7}}\n\n",
                "data: [DONE]\n\n"
            );
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}",
                        sse.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            (request.path, authorization, body)
        });
        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].upstream_protocol =
            crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
        config.profiles[0].normalize();
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();

        let response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({
                "model": model_alias(&provider_id, &model),
                "input": "hello",
                "stream": true
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("text/event-stream")
        );
        let events = response.text().await.unwrap();
        assert!(events.contains("response.output_text.delta"));
        assert!(events.contains("response.function_call_arguments.delta"));
        assert!(events.contains("\"call_id\":\"call-stream\""));
        assert!(events.contains("\"input_tokens\":4"));
        assert!(events.contains("response.completed"));

        let (path, authorization, body) = upstream_task.await.unwrap();
        assert_eq!(path, "/v1/chat/completions");
        assert_eq!(authorization.as_deref(), Some("Bearer sk-upstream"));
        assert_eq!(body["model"], "provider-model");
        assert_eq!(body["messages"][0]["content"], "hello");
        assert_eq!(body["stream_options"]["include_usage"], true);
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn chat_route_maps_web_search_for_provider_scoped_model_without_model_name_gate() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            write_json_response(
                &mut stream,
                200,
                &json!({
                    "id":"chatcmpl-search",
                    "created":123,
                    "model":body["model"],
                    "choices":[{
                        "message":{"role":"assistant","content":"search result"},
                        "finish_reason":"stop"
                    }],
                    "usage":{"prompt_tokens":4,"completion_tokens":2,"total_tokens":6}
                }),
            )
            .await
            .unwrap();
            (request.path, body)
        });
        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].upstream_protocol =
            crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
        config.profiles[0].normalize();
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();

        let response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({
                "model":model_alias(&provider_id, &model),
                "input":"search the web",
                "tools":[{
                    "type":"web_search",
                    "search_context_size":"high"
                }],
                "tool_choice":{"type":"web_search"}
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            response.json::<Value>().await.unwrap()["output_text"],
            "search result"
        );
        let (path, body) = upstream_task.await.unwrap();
        assert_eq!(path, "/v1/chat/completions");
        assert_eq!(body["model"], "provider-model");
        assert_eq!(
            body["web_search_options"],
            json!({"search_context_size":"high"})
        );
        assert!(body.get("tools").is_none());
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn chat_route_drops_ambient_auto_web_search_before_contacting_upstream() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            write_json_response(
                &mut stream,
                200,
                &json!({
                    "id":"chatcmpl-ambient-search",
                    "created":123,
                    "model":body["model"],
                    "choices":[{
                        "message":{"role":"assistant","content":"normal answer"},
                        "finish_reason":"stop"
                    }],
                    "usage":{"prompt_tokens":2,"completion_tokens":2,"total_tokens":4}
                }),
            )
            .await
            .unwrap();
            (request.path, body)
        });
        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].upstream_protocol =
            crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
        config.profiles[0].normalize();
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();

        let response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({
                "model":model_alias(&provider_id, &model),
                "input":"1",
                "tools":[{"type":"web_search"}],
                "tool_choice":"auto"
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            response.json::<Value>().await.unwrap()["output_text"],
            "normal answer"
        );
        let (path, body) = upstream_task.await.unwrap();
        assert_eq!(path, "/v1/chat/completions");
        assert_eq!(body["model"], "provider-model");
        assert!(body.get("web_search_options").is_none());
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn responses_route_passes_web_search_through_without_chat_conversion() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            write_json_response(
                &mut stream,
                200,
                &json!({
                    "id":"resp-search",
                    "object":"response",
                    "model":body["model"],
                    "output_text":"native search"
                }),
            )
            .await
            .unwrap();
            (request.path, body)
        });
        let (config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();

        let response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({
                "model":model_alias(&provider_id, &model),
                "input":"search the web",
                "tools":[{
                    "type":"web_search",
                    "search_context_size":"high"
                }]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            response.json::<Value>().await.unwrap()["output_text"],
            "native search"
        );
        let (path, body) = upstream_task.await.unwrap();
        assert_eq!(path, "/v1/responses");
        assert_eq!(body["model"], "provider-model");
        assert_eq!(body["tools"][0]["type"], "web_search");
        assert!(body.get("web_search_options").is_none());
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn native_responses_route_drops_only_codey_synthetic_previous_response_id() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let mut forwarded = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut stream).await.unwrap();
                let body = serde_json::from_slice::<Value>(&request.body).unwrap();
                write_json_response(
                    &mut stream,
                    200,
                    &json!({
                        "id":"resp-upstream",
                        "object":"response",
                        "model":body["model"],
                        "output_text":"ok"
                    }),
                )
                .await
                .unwrap();
                forwarded.push(body);
            }
            forwarded
        });
        let (config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let client = reqwest::Client::new();
        let alias = model_alias(&provider_id, &model);

        for previous_response_id in ["resp_codey_wrapped", "resp_upstream"] {
            let response = client
                .post(format!("{}/responses", endpoint.base_url))
                .bearer_auth(&endpoint.token)
                .json(&json!({
                    "model": alias,
                    "input": "continue",
                    "previous_response_id": previous_response_id,
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::OK);
        }

        let forwarded = upstream_task.await.unwrap();
        assert_eq!(forwarded.len(), 2);
        assert_eq!(forwarded[0]["model"], "provider-model");
        assert!(forwarded[0].get("previous_response_id").is_none());
        assert_eq!(forwarded[1]["previous_response_id"], "resp_upstream");
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn non_stream_chat_adapter_requests_upstream_stream_and_returns_json() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let (request_body_sent, request_body_received) = oneshot::channel();
        let (first_event_sent, first_event_observed) = oneshot::channel();
        let (release_upstream, wait_for_release) = oneshot::channel();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            request_body_sent.send(body.clone()).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            write_test_http_chunk(
                &mut stream,
                "data: {\"id\":\"chatcmpl-internal-stream\",\"model\":\"provider-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"first-token\"},\"finish_reason\":null}]}\n\n",
            )
            .await;
            first_event_sent.send(()).unwrap();
            wait_for_release.await.unwrap();
            write_test_http_chunk(
                &mut stream,
                "data: {\"id\":\"chatcmpl-internal-stream\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" done\"},\"finish_reason\":\"stop\"}]}\n\ndata: {\"id\":\"chatcmpl-internal-stream\",\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":3,\"total_tokens\":7}}\n\ndata: [DONE]\n\n",
            )
            .await;
            stream.write_all(b"0\r\n\r\n").await.unwrap();
            body
        });
        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].upstream_protocol =
            crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
        config.profiles[0].normalize();
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();

        let mut client_task = tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{}/responses", endpoint.base_url))
                .bearer_auth(&endpoint.token)
                .json(&json!({
                    "model": model_alias(&provider_id, &model),
                    "input": "hello"
                }))
                .send()
                .await
                .unwrap()
        });
        let upstream_body = request_body_received.await.unwrap();
        assert_eq!(upstream_body["stream"], true);
        assert_eq!(upstream_body["stream_options"]["include_usage"], true);
        first_event_observed.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut client_task)
                .await
                .is_err(),
            "non-stream downstream response must wait for the complete upstream stream"
        );

        release_upstream.send(()).unwrap();
        let response = tokio::time::timeout(Duration::from_secs(2), client_task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let response = response.json::<Value>().await.unwrap();
        assert_eq!(response["output_text"], "first-token done");
        assert_eq!(response["usage"]["total_tokens"], 7);
        assert_eq!(upstream_task.await.unwrap()["stream"], true);
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn chat_adapter_streams_mislabeled_sse_before_upstream_completes() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let (first_event_sent, first_event_observed) = oneshot::channel();
        let (release_upstream, wait_for_release) = oneshot::channel();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let _request = read_http_request(&mut stream).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            write_test_http_chunk(
                &mut stream,
                "data: {\"id\":\"chatcmpl-progressive\",\"model\":\"provider-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"first-token\"},\"finish_reason\":null}]}\n\n",
            )
            .await;
            first_event_sent.send(()).unwrap();
            wait_for_release.await.unwrap();
            write_test_http_chunk(
                &mut stream,
                "data: {\"id\":\"chatcmpl-progressive\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            )
            .await;
            stream.write_all(b"0\r\n\r\n").await.unwrap();
        });
        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].upstream_protocol =
            crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
        config.profiles[0].normalize();
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();

        let mut response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({
                "model":model_alias(&provider_id, &model),
                "input":"hello",
                "stream":true,
            }))
            .send()
            .await
            .unwrap();
        first_event_observed.await.unwrap();
        let first_payload = tokio::time::timeout(Duration::from_secs(2), async {
            let mut payload = Vec::new();
            loop {
                let chunk = response.chunk().await.unwrap().unwrap();
                payload.extend_from_slice(&chunk);
                if String::from_utf8_lossy(&payload).contains("first-token") {
                    break payload;
                }
            }
        })
        .await
        .unwrap();
        let first_payload = String::from_utf8_lossy(&first_payload);
        assert!(first_payload.contains("response.output_text.delta"));
        assert!(!first_payload.contains("response.completed"));

        release_upstream.send(()).unwrap();
        let mut remaining = Vec::new();
        while let Some(chunk) = response.chunk().await.unwrap() {
            remaining.extend_from_slice(&chunk);
        }
        assert!(String::from_utf8_lossy(&remaining).contains("response.completed"));

        upstream_task.await.unwrap();
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn anthropic_route_uses_messages_key_headers_and_returns_responses_sse() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let authorization = incoming_header(&request, "authorization").map(str::to_string);
            let api_key = incoming_header(&request, "x-api-key").map(str::to_string);
            let version = incoming_header(&request, "anthropic-version").map(str::to_string);
            let account_id = incoming_header(&request, "chatgpt-account-id").map(str::to_string);
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            let upstream_tool_name = body["tools"][0]["name"].as_str().unwrap();
            assert!(upstream_tool_name.starts_with(NAMESPACE_UPSTREAM_TOOL_PREFIX));
            let sse = [
                (
                    "message_start",
                    json!({
                        "type":"message_start",
                        "message":{
                            "id":"msg-stream",
                            "type":"message",
                            "role":"assistant",
                            "model":"claude-sonnet-test",
                            "content":[],
                            "usage":{"input_tokens":4}
                        }
                    }),
                ),
                (
                    "content_block_start",
                    json!({
                        "type":"content_block_start",
                        "index":0,
                        "content_block":{"type":"text","text":""}
                    }),
                ),
                (
                    "content_block_delta",
                    json!({
                        "type":"content_block_delta",
                        "index":0,
                        "delta":{"type":"text_delta","text":"hello"}
                    }),
                ),
                (
                    "content_block_start",
                    json!({
                        "type":"content_block_start",
                        "index":1,
                        "content_block":{
                            "type":"tool_use",
                            "id":"call-read",
                            "name":upstream_tool_name,
                            "input":{"path":"a.txt"}
                        }
                    }),
                ),
                (
                    "message_delta",
                    json!({
                        "type":"message_delta",
                        "delta":{"stop_reason":"tool_use"},
                        "usage":{"output_tokens":2}
                    }),
                ),
                ("message_stop", json!({"type":"message_stop"})),
            ]
            .into_iter()
            .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
            .collect::<String>();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}",
                        sse.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            (
                request.path,
                authorization,
                api_key,
                version,
                account_id,
                body,
            )
        });
        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].upstream_protocol =
            crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES.into();
        config.profiles[0].normalize();
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();

        let response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .header("chatgpt-account-id", "must-not-leak")
            .json(&json!({
                "model":model_alias(&provider_id, &model),
                "input":"hello",
                "stream":true,
                "tools":[{
                    "type":"namespace",
                    "name":"fs",
                    "tools":[{
                        "type":"function",
                        "name":"read",
                        "description":"read a file",
                        "parameters":{"type":"object","properties":{"path":{"type":"string"}}}
                    }]
                }]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let events = response.text().await.unwrap();
        assert!(events.contains("response.output_text.delta"));
        assert!(events.contains("\"delta\":\"hello\""));
        assert!(events.contains("\"call_id\":\"call-read\""));
        assert!(events.contains("\"namespace\":\"fs\""));
        assert!(events.contains("\"name\":\"read\""));
        assert!(events.contains("\"input_tokens\":4"));
        assert!(events.contains("response.completed"));

        let (path, authorization, api_key, version, account_id, body) =
            upstream_task.await.unwrap();
        assert_eq!(path, "/v1/messages");
        assert_eq!(authorization, None);
        assert_eq!(api_key.as_deref(), Some("sk-upstream"));
        assert_eq!(version.as_deref(), Some("2023-06-01"));
        assert_eq!(account_id, None);
        assert_eq!(body["model"], "provider-model");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hello");
        assert!(
            body["tools"][0]["name"]
                .as_str()
                .unwrap()
                .starts_with(NAMESPACE_UPSTREAM_TOOL_PREFIX)
        );
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn non_stream_anthropic_adapter_requests_upstream_stream_and_returns_json() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let (request_body_sent, request_body_received) = oneshot::channel();
        let (first_event_sent, first_event_observed) = oneshot::channel();
        let (release_upstream, wait_for_release) = oneshot::channel();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            request_body_sent.send(body.clone()).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            write_test_http_chunk(
                &mut stream,
                concat!(
                    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-internal-stream\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"provider-model\",\"content\":[],\"usage\":{\"input_tokens\":4}}}\n\n",
                    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"first-token\"}}\n\n"
                ),
            )
            .await;
            first_event_sent.send(()).unwrap();
            wait_for_release.await.unwrap();
            write_test_http_chunk(
                &mut stream,
                concat!(
                    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" done\"}}\n\n",
                    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
                    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
                ),
            )
            .await;
            stream.write_all(b"0\r\n\r\n").await.unwrap();
            body
        });
        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].upstream_protocol =
            crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES.into();
        config.profiles[0].normalize();
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();

        let mut client_task = tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{}/responses", endpoint.base_url))
                .bearer_auth(&endpoint.token)
                .json(&json!({
                    "model": model_alias(&provider_id, &model),
                    "input": "hello"
                }))
                .send()
                .await
                .unwrap()
        });
        let upstream_body = request_body_received.await.unwrap();
        assert_eq!(upstream_body["stream"], true);
        first_event_observed.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut client_task)
                .await
                .is_err(),
            "non-stream downstream response must wait for the complete upstream stream"
        );

        release_upstream.send(()).unwrap();
        let response = tokio::time::timeout(Duration::from_secs(2), client_task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let response = response.json::<Value>().await.unwrap();
        assert_eq!(response["output_text"], "first-token done");
        assert_eq!(response["usage"]["total_tokens"], 7);
        assert_eq!(upstream_task.await.unwrap()["stream"], true);
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn anthropic_route_bridges_apply_patch_custom_tool_and_stream_events() {
        const RAW_PATCH: &str =
            "*** Begin Patch\n*** Update File: README.md\n@@\n-old\n+new\n*** End Patch";

        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            let upstream_tool_name = body["tools"][0]["name"].as_str().unwrap().to_string();
            assert!(upstream_tool_name.starts_with(CUSTOM_UPSTREAM_TOOL_PREFIX));
            let wrapped_input = json!({"input":RAW_PATCH}).to_string();
            let sse = [
                (
                    "message_start",
                    json!({
                        "type":"message_start",
                        "message":{
                            "id":"msg-custom-stream",
                            "type":"message",
                            "role":"assistant",
                            "model":"claude-sonnet-test",
                            "content":[],
                            "usage":{"input_tokens":5}
                        }
                    }),
                ),
                (
                    "content_block_start",
                    json!({
                        "type":"content_block_start",
                        "index":0,
                        "content_block":{
                            "type":"tool_use",
                            "id":"call-apply-patch",
                            "name":upstream_tool_name,
                            "input":{}
                        }
                    }),
                ),
                (
                    "content_block_delta",
                    json!({
                        "type":"content_block_delta",
                        "index":0,
                        "delta":{
                            "type":"input_json_delta",
                            "partial_json":wrapped_input
                        }
                    }),
                ),
                (
                    "message_delta",
                    json!({
                        "type":"message_delta",
                        "delta":{"stop_reason":"tool_use"},
                        "usage":{"output_tokens":3}
                    }),
                ),
                ("message_stop", json!({"type":"message_stop"})),
            ]
            .into_iter()
            .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
            .collect::<String>();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}",
                        sse.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            body
        });
        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].upstream_protocol =
            crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES.into();
        config.profiles[0].normalize();
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();

        let response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({
                "model":model_alias(&provider_id, &model),
                "input":"apply the patch",
                "stream":true,
                "tools":[{
                    "type":"custom",
                    "name":"apply_patch",
                    "description":"Apply a patch to the workspace",
                    "format":{
                        "type":"grammar",
                        "syntax":"lark",
                        "definition":"start: /[\\s\\S]+/"
                    }
                }]
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let events = response.text().await.unwrap();
        assert!(events.contains("response.custom_tool_call_input.delta"));
        assert!(events.contains("response.custom_tool_call_input.done"));
        assert!(events.contains("\"type\":\"custom_tool_call\""));
        assert!(events.contains("\"name\":\"apply_patch\""));
        assert!(events.contains("*** Begin Patch"));
        assert!(!events.contains("response.function_call_arguments"));
        assert!(!events.contains(CUSTOM_UPSTREAM_TOOL_PREFIX));
        assert!(events.contains("response.completed"));

        let body = upstream_task.await.unwrap();
        assert_eq!(
            body["tools"][0]["input_schema"]["required"],
            json!(["input"])
        );
        assert_eq!(
            body["tools"][0]["input_schema"]["additionalProperties"],
            false
        );
        assert!(
            body["tools"][0]["description"]
                .as_str()
                .unwrap()
                .contains("\"syntax\":\"lark\"")
        );
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn anthropic_adapter_streams_mislabeled_sse_before_upstream_completes() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let (first_event_sent, first_event_observed) = oneshot::channel();
        let (release_upstream, wait_for_release) = oneshot::channel();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let _request = read_http_request(&mut stream).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            write_test_http_chunk(
                &mut stream,
                concat!(
                    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-progressive\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"provider-model\",\"content\":[],\"usage\":{\"input_tokens\":1}}}\n\n",
                    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"first-token\"}}\n\n"
                ),
            )
            .await;
            first_event_sent.send(()).unwrap();
            wait_for_release.await.unwrap();
            write_test_http_chunk(
                &mut stream,
                concat!(
                    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
                    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
                ),
            )
            .await;
            stream.write_all(b"0\r\n\r\n").await.unwrap();
        });
        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1"));
        config.profiles[0].upstream_protocol =
            crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES.into();
        config.profiles[0].normalize();
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();

        let mut response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({
                "model":model_alias(&provider_id, &model),
                "input":"hello",
                "stream":true,
            }))
            .send()
            .await
            .unwrap();
        first_event_observed.await.unwrap();
        let first_payload = tokio::time::timeout(Duration::from_secs(2), async {
            let mut payload = Vec::new();
            loop {
                let chunk = response.chunk().await.unwrap().unwrap();
                payload.extend_from_slice(&chunk);
                if String::from_utf8_lossy(&payload).contains("first-token") {
                    break payload;
                }
            }
        })
        .await
        .unwrap();
        let first_payload = String::from_utf8_lossy(&first_payload);
        assert!(first_payload.contains("response.output_text.delta"));
        assert!(!first_payload.contains("response.completed"));

        release_upstream.send(()).unwrap();
        let mut remaining = Vec::new();
        while let Some(chunk) = response.chunk().await.unwrap() {
            remaining.extend_from_slice(&chunk);
        }
        assert!(String::from_utf8_lossy(&remaining).contains("response.completed"));

        upstream_task.await.unwrap();
        router.stop().await.unwrap();
    }

    #[test]
    fn model_aliases_do_not_collapse_distinct_provider_ids() {
        assert_ne!(
            model_alias("team/relay", "shared-model"),
            model_alias("team_relay", "shared-model")
        );
        assert_eq!(
            model_alias("team/relay", "shared-model"),
            "team%2Frelay/shared-model"
        );
    }

    #[tokio::test]
    async fn historical_alias_http_requests_survive_hot_route_removal() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let mut models = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut stream).await.unwrap();
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                models.push(body["model"].clone());
                write_json_response(&mut stream, 200, &json!({"model":body["model"]}))
                    .await
                    .unwrap();
            }
            models
        });
        let (mut config, _, model) = router_config(format!("http://{address}/v1"));
        let mut old = config.profiles[0].clone();
        old.id = "retired/route".into();
        config.profiles.push(old);
        config
            .selected_models_by_provider
            .insert("retired/route".into(), vec![model.clone()]);
        config = config.normalize();
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let client = reqwest::Client::new();
        for remove in [false, true] {
            if remove {
                config
                    .profiles
                    .retain(|profile| profile.id != "retired/route");
                config.selected_models_by_provider.remove("retired/route");
                router.update_config(&config);
            }
            let response = client
                .post(format!("{}/responses", endpoint.base_url))
                .bearer_auth(&endpoint.token)
                .header("thread-id", "historical-thread")
                .json(&json!({"model":model_alias("retired/route", &model), "input":"hello"}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            assert_eq!(response.json::<Value>().await.unwrap()["model"], model);
        }
        assert_eq!(
            upstream_task.await.unwrap(),
            vec![json!(model), json!(model)]
        );
        let error = client
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({"model":"codey/missing-model", "input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(error.status(), reqwest::StatusCode::NOT_FOUND);
        let error: Value = error.json().await.unwrap();
        assert_eq!(error["error"]["code"], "model_not_enabled");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("请为模型")
        );
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn router_rewrites_alias_and_keeps_upstream_credentials_private() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let authorization = request
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                .map(|(_, value)| value.clone());
            let user_agent = incoming_header(&request, "user-agent").map(str::to_string);
            let originator = incoming_header(&request, "originator").map(str::to_string);
            let codex_window_id =
                incoming_header(&request, "x-codex-window-id").map(str::to_string);
            let account_id = incoming_header(&request, "chatgpt-account-id").map(str::to_string);
            let router_token = incoming_header(&request, ROUTER_AUTH_HEADER).map(str::to_string);
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            write_json_response(
                &mut stream,
                200,
                &json!({"object":"response","model":body["model"]}),
            )
            .await
            .unwrap();
            (
                request.path,
                authorization,
                user_agent,
                originator,
                codex_window_id,
                account_id,
                router_token,
                body,
            )
        });
        let (config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let alias = model_alias(&provider_id, &model);

        let response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .header("user-agent", "codex_cli_rs/0.114.0")
            .header("originator", "codex_cli_rs")
            .header("x-codex-window-id", "window-123")
            .header("chatgpt-account-id", "must-not-leak")
            .json(&json!({"model":alias,"input":"hello","stream":true}))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.json::<Value>().await.unwrap()["model"], model);
        let (
            path,
            authorization,
            user_agent,
            originator,
            codex_window_id,
            account_id,
            router_token,
            body,
        ) = upstream_task.await.unwrap();
        assert_eq!(path, "/v1/responses");
        assert_eq!(authorization.as_deref(), Some("Bearer sk-upstream"));
        assert_eq!(user_agent.as_deref(), Some("codex_cli_rs/0.114.0"));
        assert_eq!(originator.as_deref(), Some("codex_cli_rs"));
        assert_eq!(codex_window_id.as_deref(), Some("window-123"));
        assert_eq!(account_id, None);
        assert_eq!(router_token, None);
        assert_eq!(body["model"], "provider-model");
        assert!(!body.to_string().contains(&endpoint.token));
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn raw_model_metadata_selects_an_ambiguous_route_and_binds_the_thread() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let mut bodies = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = upstream.accept().await.unwrap();
                let request = read_http_request(&mut stream).await.unwrap();
                let body = serde_json::from_slice::<Value>(&request.body).unwrap();
                assert_eq!(body["model"], "provider-model");
                write_json_response(
                    &mut stream,
                    200,
                    &json!({"object":"response","model":body["model"]}),
                )
                .await
                .unwrap();
                bodies.push(body);
            }
            bodies
        });
        let (mut config, _, model) = router_config("http://127.0.0.1:9/v1".into());
        let mut route_b = config.profiles[0].clone();
        route_b.id = "route-b".into();
        route_b.name = "Relay B".into();
        route_b.base_url = format!("http://{upstream_address}/v1");
        route_b.api_key = "sk-route-b".into();
        route_b.normalize();
        let provider_b = route_b.provider_id().to_string();
        config.profiles.push(route_b);
        config
            .selected_models_by_provider
            .insert(provider_b.clone(), vec![model.clone()]);
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let client = reqwest::Client::new();
        let turn_metadata = json!({
            ROUTE_METADATA_KEY: provider_b,
            "preserved": "yes"
        })
        .to_string();

        let first = client
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .header("thread-id", "thread-route-b")
            .header(TURN_METADATA_HEADER, &turn_metadata)
            .json(&json!({
                "model": model,
                "input": "first",
                "client_metadata": {
                    "x-codex-turn-metadata": turn_metadata,
                    "preserved": "body"
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(first.status(), reqwest::StatusCode::OK);

        let second = client
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .header("thread-id", "thread-route-b")
            .json(&json!({"model":"provider-model","input":"second"}))
            .send()
            .await
            .unwrap();
        assert_eq!(second.status(), reqwest::StatusCode::OK);

        let ambiguous = client
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .header("thread-id", "unbound-thread")
            .json(&json!({"model":"provider-model","input":"ambiguous"}))
            .send()
            .await
            .unwrap();
        assert_eq!(ambiguous.status(), reqwest::StatusCode::NOT_FOUND);
        assert!(
            ambiguous.json::<Value>().await.unwrap()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("缺少明确")
        );

        let bodies = upstream_task.await.unwrap();
        let nested = bodies[0]["client_metadata"][TURN_METADATA_HEADER]
            .as_str()
            .and_then(|metadata| serde_json::from_str::<Value>(metadata).ok())
            .unwrap();
        assert!(nested.get(ROUTE_METADATA_KEY).is_none());
        assert_eq!(nested["preserved"], "yes");
        assert_eq!(bodies[0]["client_metadata"]["preserved"], "body");
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn route_updates_affect_only_later_requests_and_keep_inflight_streams_pinned() {
        let upstream_a = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_a_address = upstream_a.local_addr().unwrap();
        let upstream_b = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_b_address = upstream_b.local_addr().unwrap();
        let (release_a, wait_for_release_a) = tokio::sync::oneshot::channel();
        let upstream_a_task = tokio::spawn(async move {
            let (mut stream, _) = upstream_a.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            assert_eq!(body["model"], "provider-model");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            let first = b"data: {\"source\":\"a\",\"phase\":1}\n\n";
            stream
                .write_all(format!("{:x}\r\n", first.len()).as_bytes())
                .await
                .unwrap();
            stream.write_all(first).await.unwrap();
            stream.write_all(b"\r\n").await.unwrap();
            stream.flush().await.unwrap();
            wait_for_release_a.await.unwrap();
            let second = b"data: {\"source\":\"a\",\"phase\":2}\n\n";
            stream
                .write_all(format!("{:x}\r\n", second.len()).as_bytes())
                .await
                .unwrap();
            stream.write_all(second).await.unwrap();
            stream.write_all(b"\r\n0\r\n\r\n").await.unwrap();
        });
        let upstream_b_task = tokio::spawn(async move {
            let (mut stream, _) = upstream_b.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            write_json_response(
                &mut stream,
                200,
                &json!({"object":"response","source":"b","model":body["model"]}),
            )
            .await
            .unwrap();
        });
        let (config, provider_id, model) = router_config(format!("http://{upstream_a_address}/v1"));
        let alias = model_alias(&provider_id, &model);
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let client = reqwest::Client::new();

        let mut first_response = client
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({"model":alias,"input":"first","stream":true}))
            .send()
            .await
            .unwrap();
        assert_eq!(first_response.status(), reqwest::StatusCode::OK);
        let first_payload = tokio::time::timeout(Duration::from_secs(5), async {
            let mut payload = Vec::new();
            loop {
                let chunk = first_response.chunk().await.unwrap().unwrap();
                payload.extend_from_slice(&chunk);
                if String::from_utf8_lossy(&payload).contains("\"phase\":1") {
                    break payload;
                }
            }
        })
        .await
        .unwrap();
        assert!(String::from_utf8_lossy(&first_payload).contains("\"source\":\"a\""));

        let mut updated = config.clone();
        updated.profiles[0].base_url = format!("http://{upstream_b_address}/v1");
        router.update_config(&updated);
        let second_response = client
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({"model":alias,"input":"second","stream":false}))
            .send()
            .await
            .unwrap();
        assert_eq!(second_response.status(), reqwest::StatusCode::OK);
        let second_body = second_response.json::<Value>().await.unwrap();
        assert_eq!(second_body["source"], "b");
        assert_eq!(second_body["model"], "provider-model");

        release_a.send(()).unwrap();
        let mut remaining_first_payload = Vec::new();
        while let Some(chunk) = first_response.chunk().await.unwrap() {
            remaining_first_payload.extend_from_slice(&chunk);
        }
        let remaining_first_payload = String::from_utf8_lossy(&remaining_first_payload);
        assert!(remaining_first_payload.contains("\"source\":\"a\""));
        assert!(remaining_first_payload.contains("\"phase\":2"));
        assert!(!remaining_first_payload.contains("\"source\":\"b\""));

        upstream_a_task.await.unwrap();
        upstream_b_task.await.unwrap();
        router.stop().await.unwrap();
    }

    #[test]
    fn upstream_authority_omits_credentials_paths_and_queries() {
        assert_eq!(
            upstream_authority("https://user:secret@relay.example:8443/private/v1?token=hidden"),
            "relay.example:8443"
        );
    }

    #[tokio::test]
    async fn transport_error_response_is_plain_text_and_non_retryable() {
        let (mut reader, mut writer) = tokio::io::duplex(4096);

        write_text_error_response(
            &mut writer,
            424,
            "upstream_unreachable",
            "Codey 线路「Relay」无法连接上游 relay.example:8443",
        )
        .await
        .unwrap();
        drop(writer);

        let mut response = String::new();
        reader.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 424 Failed Dependency\r\n"));
        assert!(response.contains("content-type: text/plain; charset=utf-8\r\n"));
        assert!(response.contains("Codey 线路「Relay」无法连接上游 relay.example:8443"));
        assert!(response.contains("错误码：upstream_unreachable"));
    }

    #[tokio::test]
    async fn router_proxies_image_generation_to_the_default_openai_route() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let authorization = incoming_header(&request, "authorization").map(str::to_string);
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            let response = r#"{"created":1,"data":[{"b64_json":"aGVsbG8="}]}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            (request.path, authorization, body)
        });
        let (mut config, provider_id, model) =
            router_config(format!("http://{upstream_address}/v1/responses"));
        config.default_model = model_alias(&provider_id, &model);
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();

        let response = reqwest::Client::new()
            .post(format!("{}/images/generations", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({"model":"gpt-image-2","prompt":"draw an otter"}))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.json::<Value>().await.unwrap()["created"], 1);
        let (path, authorization, body) = upstream_task.await.unwrap();
        assert_eq!(path, "/v1/images/generations");
        assert_eq!(authorization.as_deref(), Some("Bearer sk-upstream"));
        assert_eq!(body["model"], "gpt-image-2");
        assert_eq!(body["prompt"], "draw an otter");
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn router_rejects_unknown_raw_models_instead_of_guessing_a_route() {
        let (config, _, _) = router_config("http://127.0.0.1:9/v1".to_string());
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();

        let response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({"model":"unknown-model","input":"hello"}))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
        let body = response.json::<Value>().await.unwrap();
        assert_eq!(body["error"]["code"], "model_not_enabled");
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn router_rejects_requests_without_the_launch_token() {
        let (config, provider_id, model) = router_config("http://127.0.0.1:9/v1".to_string());
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();

        let response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .json(&json!({"model":model_alias(&provider_id, &model),"input":"hello"}))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
        let body = response.json::<Value>().await.unwrap();
        assert_eq!(body["error"]["code"], "invalid_router_token");

        let gateway_root = endpoint.base_url.trim_end_matches("/v1");
        let legacy_capability_response = reqwest::Client::new()
            .post(format!("{gateway_root}/{}/v1/responses", endpoint.token))
            .json(&json!({"model":model_alias(&provider_id, &model),"input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            legacy_capability_response.status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn router_rejects_unauthorized_request_before_reading_its_body() {
        let (config, _, _) = router_config("http://127.0.0.1:9/v1".to_string());
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let url = reqwest::Url::parse(&endpoint.base_url).unwrap();
        let mut stream = TcpStream::connect((url.host_str().unwrap(), url.port().unwrap()))
            .await
            .unwrap();
        stream
            .write_all(
                b"POST /v1/responses HTTP/1.1\r\nhost: localhost\r\ncontent-length: 1048576\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();

        let mut response = String::new();
        tokio::time::timeout(Duration::from_secs(1), stream.read_to_string(&mut response))
            .await
            .expect("unauthorized request must not wait for its declared body")
            .unwrap();
        assert!(response.starts_with("HTTP/1.1 401 Unauthorized\r\n"));
        assert!(response.contains("invalid_router_token"));
        router.stop().await.unwrap();
    }

    #[tokio::test]
    async fn request_log_page_is_public_but_its_api_requires_the_launch_token() {
        let (config, provider_id, model) = router_config("http://127.0.0.1:9/v1".to_string());
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let gateway_root = endpoint.base_url.trim_end_matches("/v1");
        let client = reqwest::Client::new();

        let page = client
            .get(format!("{gateway_root}{REQUEST_LOG_PAGE_PATH}"))
            .send()
            .await
            .unwrap();
        assert_eq!(page.status(), reqwest::StatusCode::OK);
        assert!(page.text().await.unwrap().contains(REQUEST_LOG_SCRIPT_PATH));
        let script = client
            .get(format!("{gateway_root}{REQUEST_LOG_SCRIPT_PATH}"))
            .send()
            .await
            .unwrap();
        assert_eq!(script.status(), reqwest::StatusCode::OK);
        assert!(script.text().await.unwrap().contains(REQUEST_LOG_PAGE_PATH));
        assert_eq!(
            endpoint.request_log_url(),
            format!("{gateway_root}{REQUEST_LOG_PAGE_PATH}#{}", endpoint.token)
        );

        let unauthorized = client
            .post(format!("{gateway_root}/codey/api/load_codey_config"))
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);

        let catalog = client
            .post(format!("{gateway_root}/codey/api/load_codey_config"))
            .header(ROUTER_AUTH_HEADER, &endpoint.token)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(catalog.status(), reqwest::StatusCode::OK);
        let catalog = catalog.json::<Value>().await.unwrap();
        assert_eq!(catalog["config"]["profiles"][0]["id"], "route-a");
        assert_eq!(
            catalog["config"]["selectedModelsByProvider"][&provider_id][0],
            model
        );
        assert!(catalog["config"]["profiles"][0].get("apiKey").is_none());
        assert!(catalog["config"]["profiles"][0].get("baseUrl").is_none());

        router.stop().await.unwrap();
    }
}
