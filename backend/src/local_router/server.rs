use super::*;

pub(crate) fn record_router_failure_nonblocking(
    event: &'static str,
    operation: &'static str,
    error: impl Into<String>,
    context: Value,
) {
    let error = error.into();
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        static LOG_BUDGET: std::sync::OnceLock<Arc<Semaphore>> = std::sync::OnceLock::new();
        let budget = LOG_BUDGET.get_or_init(|| Arc::new(Semaphore::new(128)));
        let Ok(permit) = Arc::clone(budget).try_acquire_owned() else {
            return;
        };
        // Drop excess diagnostics instead of queuing unbounded blocking work.
        drop(runtime.spawn_blocking(move || {
            let _permit = permit;
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
    /// 至少一条线路支持原生远程压缩；不影响统一 Provider 的选择。
    pub supports_remote_compaction: bool,
    /// Official ChatGPT routes are available this launch. Codex keeps its
    /// native OpenAI login for this provider; the independent router header
    /// authenticates the loopback hop and the gateway isolates upstream auth.
    pub requires_openai_auth: bool,
}

impl RuntimeRouterEndpoint {
    pub(crate) fn request_log_url(&self, theme: Option<&str>) -> String {
        let query = match theme {
            Some("light") => "?theme=light",
            Some("dark") => "?theme=dark",
            _ => "",
        };
        format!(
            "{}/codey/request-logs{query}#{}",
            self.base_url.trim_end_matches("/v1"),
            self.token
        )
    }
}

#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestLogCatalog {
    pub(crate) official_account_available: bool,
    pub(crate) profiles: Vec<RequestLogProfile>,
    pub(crate) selected_models_by_provider: BTreeMap<String, Vec<String>>,
    pub(crate) declared_official_models_by_provider: BTreeMap<String, Vec<String>>,
    pub(crate) upstream_models_by_provider: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestLogProfile {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) short_name: String,
    pub(crate) official_account: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) official_account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source_provider_id: Option<String>,
}

impl RequestLogCatalog {
    pub(crate) fn from_config(config: &CodeyConfig) -> Self {
        Self {
            official_account_available: config.official_account_available_this_launch,
            profiles: config
                .profiles
                .iter()
                .map(|profile| RequestLogProfile {
                    id: profile.id.clone(),
                    name: profile.name.clone(),
                    short_name: profile.display_short_name(),
                    official_account: profile.official_account,
                    official_account_id: profile.official_account_id.clone(),
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

/// 本地路由固定优先占用的端口，被占用才顺序向上探测。
/// 连续候选均不可用时由内核分配，实际端口可能在该端口之后。
pub(crate) const ROUTER_PORT_START: u16 = 26_958;
/// 从起始端口最多连续探测多少个候选端口，全都不可用时回退由内核分配。
pub(crate) const ROUTER_PORT_PROBES: u16 = 64;

const ROUTER_READY_TIMEOUT: Duration = Duration::from_secs(5);
const ROUTER_RESTART_INITIAL_DELAY: Duration = Duration::from_millis(100);
const ROUTER_RESTART_MAX_DELAY: Duration = Duration::from_secs(5);
const ROUTER_RESTART_STABLE_PERIOD: Duration = Duration::from_secs(60);

/// 从 `ROUTER_PORT_START` 开始顺序探测可用端口，候选被占用就换下一个。
/// `bind` 成功即独占该端口，所以并发启动的多个实例不会拿到同一个端口。
pub(super) async fn bind_router_listener_with<T, F, Fut>(mut bind: F) -> Result<T>
where
    F: FnMut(std::net::SocketAddrV4) -> Fut,
    Fut: std::future::Future<Output = std::io::Result<T>>,
{
    let mut occupied = Vec::new();
    for step in 0..ROUTER_PORT_PROBES {
        let port = ROUTER_PORT_START + step;
        match bind(std::net::SocketAddrV4::new(
            std::net::Ipv4Addr::LOCALHOST,
            port,
        ))
        .await
        {
            Ok(listener) => return Ok(listener),
            Err(error) => occupied.push((port, error)),
        }
    }
    // 连续候选都被占用时不能让路由起不来，退回到内核分配并留下诊断。
    record_router_failure_nonblocking(
        "local_router_port_candidates_unavailable",
        "bind_local_router",
        format!(
            "固定端口 {ROUTER_PORT_START} 起连续 {ROUTER_PORT_PROBES} 个候选端口均不可用，回退由内核分配端口"
        ),
        serde_json::json!({
            "probes": occupied
                .iter()
                .take(8)
                .map(|(port, error)| serde_json::json!({
                    "port": port,
                    "error": error.to_string(),
                }))
                .collect::<Vec<_>>(),
        }),
    );
    bind(std::net::SocketAddrV4::new(
        std::net::Ipv4Addr::LOCALHOST,
        0,
    ))
    .await
    .context("启动 Codey 本地路由失败")
}

pub(crate) async fn bind_router_listener() -> Result<TcpListener> {
    bind_router_listener_with(TcpListener::bind).await
}

/// 路由访问令牌持久化在应用状态目录下，重启后沿用同一个令牌，
/// 已打开的日志页面刷新后仍能通过鉴权。
fn router_token_path() -> PathBuf {
    codey_runtime_core::paths::default_app_state_dir().join("router-token")
}

/// 读取或生成路由令牌：已有持久化令牌沿用，文件缺失或内容损坏时生成新令牌
/// 并尽量写回；写盘失败退回本次启动的临时令牌，不阻塞路由启动。
fn load_or_create_router_token() -> String {
    load_or_create_router_token_at(&router_token_path())
}

pub(super) fn load_or_create_router_token_at(path: &Path) -> String {
    if let Ok(bytes) = crate::fs_util::read_bounded(path, 256) {
        let token = String::from_utf8_lossy(&bytes).trim().to_string();
        if token.starts_with("codey-router-") {
            return token;
        }
    }
    let token = format!("codey-router-{}", Uuid::new_v4());
    if let Err(error) = crate::fs_util::atomic_write_private_with_parent(path, token.as_bytes()) {
        record_router_failure_nonblocking(
            "router_token_persist_failed",
            "persist_router_token",
            format!("{error:#}"),
            serde_json::json!({ "path": path.display().to_string() }),
        );
    }
    token
}

pub(crate) struct LocalRouter {
    #[cfg(test)]
    pub(crate) portable_history_cache: std::sync::Weak<Mutex<PortableHistoryCache>>,
    #[cfg(test)]
    pub(super) listener_worker: Arc<Mutex<Option<tokio::task::AbortHandle>>>,
    pub(crate) endpoint: RuntimeRouterEndpoint,
    pub(crate) bindings: Arc<Mutex<RouteBindings>>,
    pub(crate) snapshot: Arc<RwLock<Arc<RouterSnapshot>>>,
    pub(crate) official_concurrency: Arc<OfficialConcurrency>,
    pub(crate) websocket_backoffs: Arc<Mutex<UpstreamWebSocketBackoffs>>,
    pub(crate) shutdown: Mutex<Option<oneshot::Sender<()>>>,
    pub(crate) task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub(crate) request_log: Arc<RouteRequestLogController>,
}

impl LocalRouter {
    #[cfg(test)]
    pub(crate) async fn start(config: &CodeyConfig) -> Result<Self> {
        Self::start_with_logger(config, Arc::new(RouteRequestLogController::new())).await
    }

    #[cfg(test)]
    pub(super) async fn start_with_logger(
        config: &CodeyConfig,
        request_log: Arc<RouteRequestLogController>,
    ) -> Result<Self> {
        Self::start_with_logger_and_usage(config, request_log, Arc::default()).await
    }

    pub(crate) async fn start_with_usage(
        config: &CodeyConfig,
        account_usage_cache: Arc<tokio::sync::Mutex<crate::account_usage::AccountUsageCaches>>,
    ) -> Result<Self> {
        Self::start_with_logger_and_usage(
            config,
            Arc::new(RouteRequestLogController::new()),
            account_usage_cache,
        )
        .await
    }

    async fn start_with_logger_and_usage(
        config: &CodeyConfig,
        request_log: Arc<RouteRequestLogController>,
        account_usage_cache: Arc<tokio::sync::Mutex<crate::account_usage::AccountUsageCaches>>,
    ) -> Result<Self> {
        let listener = bind_router_listener().await?;
        Self::start_with_bound_listener(config, request_log, account_usage_cache, listener).await
    }

    pub(super) async fn start_with_bound_listener(
        config: &CodeyConfig,
        request_log: Arc<RouteRequestLogController>,
        account_usage_cache: Arc<tokio::sync::Mutex<crate::account_usage::AccountUsageCaches>>,
        listener: TcpListener,
    ) -> Result<Self> {
        let address = listener
            .local_addr()
            .context("读取 Codey 本地路由监听地址失败")?;
        let token = load_or_create_router_token();
        let endpoint = RuntimeRouterEndpoint {
            base_url: format!("http://{address}/v1"),
            token,
            supports_websockets: config.runtime_supports_websockets(),
            supports_remote_compaction: config.runtime_supports_remote_compaction(),
            requires_openai_auth: config.router_requires_openai_auth(),
        };
        let snapshot = Arc::new(RwLock::new(Arc::new(RouterSnapshot::from_config(config))));
        let official_concurrency = Arc::new(OfficialConcurrency::default());
        official_concurrency.update(&snapshot.read().unwrap());
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let official_auth_path = crate::codex_config::codex_home().join("auth.json");
        let websocket_backoffs = Arc::new(Mutex::new(UpstreamWebSocketBackoffs::default()));
        websocket_backoffs
            .lock()
            .unwrap()
            .update_routes(&snapshot.read().unwrap());
        if let Err(error) = request_log.reconfigure(&config.route_request_log).await {
            record_router_failure_nonblocking(
                "route_request_log_start_failed",
                "start_route_request_log",
                format!("{error:#}"),
                serde_json::json!({}),
            );
        }
        let bindings = Arc::new(Mutex::new(RouteBindings::default()));
        let server = RouterServer {
            endpoint: endpoint.clone(),
            token: endpoint.token.clone(),
            bearer_token: format!("Bearer {}", endpoint.token),
            snapshot: Arc::clone(&snapshot),
            official_concurrency: Arc::clone(&official_concurrency),
            admitted_connections: Arc::new(Semaphore::new(MAX_ADMITTED_CONNECTIONS)),
            connection_limit: Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS)),
            rejection_limit: Arc::new(Semaphore::new(MAX_CONCURRENT_REJECTIONS)),
            request_body_budget: Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS)),
            bindings: Arc::clone(&bindings),
            websocket_backoffs: Arc::clone(&websocket_backoffs),
            native_history_cache: Arc::new(Mutex::new(NativeHistoryCache::default())),
            portable_history_cache: Arc::new(Mutex::new(PortableHistoryCache::default())),
            idle_downstreams: Arc::new(Mutex::new(IdleDownstreamRegistry::default())),
            // 系统证书与代理配置加载是同步操作，不占用路由的异步工作线程。
            client: tokio::task::spawn_blocking(|| upstream_http_client_builder().build())
                .await
                .context("创建 Codey 本地路由 HTTP 客户端任务异常退出")?
                .context("创建 Codey 本地路由 HTTP 客户端失败")?,
            proxied_clients: Mutex::new(HashMap::new()),
            official_auth_path,
            account_usage_cache,
            official_auth_cache: Arc::new(Mutex::new(
                crate::account_usage::OfficialAuthCaches::default(),
            )),
            siwc_auth_cache: Arc::new(Mutex::new(SiwcAuthCaches::default())),
            request_log: Arc::clone(&request_log),
        };
        let server = Arc::new(server);
        #[cfg(test)]
        let portable_history_cache = Arc::downgrade(&server.portable_history_cache);
        #[cfg(test)]
        let listener_worker = Arc::new(Mutex::new(None));
        let task = tokio::spawn(supervise_router_listener(
            listener,
            address,
            server,
            shutdown_rx,
            #[cfg(test)]
            Arc::clone(&listener_worker),
        ));
        let router = Self {
            #[cfg(test)]
            portable_history_cache,
            #[cfg(test)]
            listener_worker,
            endpoint,
            bindings,
            snapshot,
            official_concurrency,
            websocket_backoffs,
            shutdown: Mutex::new(Some(shutdown_tx)),
            task: Mutex::new(Some(task)),
            request_log,
        };
        router.confirm_ready(ROUTER_READY_TIMEOUT).await
    }

    pub(super) async fn confirm_ready(self, wait: Duration) -> Result<Self> {
        if let Err(error) = verify_router_endpoint(&self.endpoint, wait).await {
            record_router_failure_nonblocking(
                "local_router_start_failed",
                "confirm_local_router_ready",
                format!("{error:#}"),
                serde_json::json!({ "baseUrl": self.endpoint.base_url }),
            );
            // 就绪失败时等待监听释放，再把错误交给启动流程，避免注入失效地址。
            if let Err(cleanup_error) = self.stop().await {
                record_router_failure_nonblocking(
                    "local_router_start_cleanup_failed",
                    "stop_unready_local_router",
                    format!("{cleanup_error:#}"),
                    serde_json::json!({ "baseUrl": self.endpoint.base_url }),
                );
            }
            return Err(error);
        }
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "local_router.listener_ready",
            serde_json::json!({ "baseUrl": self.endpoint.base_url }),
        );
        Ok(self)
    }

    pub(crate) fn endpoint(&self) -> RuntimeRouterEndpoint {
        self.endpoint.clone()
    }
}

pub(super) async fn verify_router_endpoint(
    endpoint: &RuntimeRouterEndpoint,
    wait: Duration,
) -> Result<()> {
    tokio::time::timeout(wait, async {
        // 本地探测不使用系统代理，不跟随重定向，也不访问任何上游线路。
        let client = tokio::task::spawn_blocking(|| {
            reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
        })
        .await
        .context("创建本地路由就绪检查任务失败")?
        .context("创建本地路由就绪检查客户端失败")?;
        let response = client
            .get(format!(
                "{}/codey/concurrency",
                endpoint.base_url.trim_end_matches("/v1"),
            ))
            .header(ROUTER_AUTH_HEADER, &endpoint.token)
            .send()
            .await
            .context("本地路由尚未响应就绪检查")?;
        anyhow::ensure!(
            response.status() == reqwest::StatusCode::OK,
            "本地路由就绪检查返回状态 {}",
            response.status(),
        );
        let body = response
            .json::<Value>()
            .await
            .context("本地路由就绪检查响应无效")?;
        anyhow::ensure!(body.is_array(), "本地路由就绪检查未返回并发状态",);
        Ok(())
    })
    .await
    .context("等待 Codey 本地路由就绪超时")?
}

fn router_restart_delay(failures: u32) -> Duration {
    ROUTER_RESTART_INITIAL_DELAY
        .saturating_mul(1_u32 << failures.saturating_sub(1).min(16))
        .min(ROUTER_RESTART_MAX_DELAY)
}

async fn supervise_router_listener(
    mut listener: TcpListener,
    address: std::net::SocketAddr,
    server: Arc<RouterServer>,
    mut shutdown_rx: oneshot::Receiver<()>,
    #[cfg(test)] listener_worker: Arc<Mutex<Option<tokio::task::AbortHandle>>>,
) {
    // JoinSet 随监督任务一起销毁，Drop 或显式取消不会留下仍在监听的子任务。
    let mut workers = JoinSet::new();
    let mut failures = 0_u32;
    let mut last_recovery_log: Option<Instant> = None;
    'supervision: loop {
        let (worker_shutdown, worker_shutdown_rx) = oneshot::channel();
        let started = Instant::now();
        let _worker_abort = workers.spawn(run_router_listener(
            listener,
            Arc::clone(&server),
            worker_shutdown_rx,
        ));
        #[cfg(test)]
        {
            *listener_worker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(_worker_abort);
        }
        let ended = tokio::select! {
            biased;
            _ = &mut shutdown_rx => {
                let _ = worker_shutdown.send(());
                // worker 保留原有连接 drain 时限；等待其完成后才报告关闭。
                while let Some(joined) = workers.join_next().await {
                    if let Err(error) = joined {
                        record_router_failure_nonblocking(
                            "local_router_listener_shutdown_failed",
                            "stop_local_router_listener",
                            error.to_string(),
                            serde_json::json!({ "address": address.to_string() }),
                        );
                    }
                }
                break;
            }
            joined = workers.join_next() => joined,
        };
        if started.elapsed() >= ROUTER_RESTART_STABLE_PERIOD {
            failures = 0;
        }
        failures = failures.saturating_add(1);
        // 持续失败按次数限流，避免相同故障占满诊断日志。
        if failures.is_power_of_two() {
            let error = match ended {
                Some(Err(error)) => error.to_string(),
                _ => "本地路由监听任务在未关闭时结束".to_string(),
            };
            record_router_failure_nonblocking(
                "local_router_listener_exited",
                "supervise_local_router_listener",
                error,
                serde_json::json!({ "address": address.to_string(), "failures": failures }),
            );
        }
        listener = loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => break 'supervision,
                _ = tokio::time::sleep(router_restart_delay(failures)) => {}
            }
            // 已注入的地址不能改变；原端口被占用时继续等待，并允许关闭打断。
            let rebound = tokio::select! {
                biased;
                _ = &mut shutdown_rx => break 'supervision,
                result = TcpListener::bind(address) => result,
            };
            match rebound {
                Ok(listener) => {
                    if last_recovery_log
                        .is_none_or(|last| last.elapsed() >= ROUTER_RESTART_STABLE_PERIOD)
                    {
                        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                            "local_router.listener_rebound",
                            serde_json::json!({
                                "address": address.to_string(),
                                "failures": failures,
                            }),
                        );
                        last_recovery_log = Some(Instant::now());
                    }
                    break listener;
                }
                Err(error) => {
                    failures = failures.saturating_add(1);
                    if failures.is_power_of_two() {
                        record_router_failure_nonblocking(
                            "local_router_listener_rebind_failed",
                            "rebind_local_router_listener",
                            error.to_string(),
                            serde_json::json!({
                                "address": address.to_string(),
                                "failures": failures,
                            }),
                        );
                    }
                }
            }
        };
    }
    server
        .portable_history_cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

async fn run_router_listener(
    listener: TcpListener,
    server: Arc<RouterServer>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let mut connections = JoinSet::new();
    let mut consecutive_accept_failures = 0_u32;
    let mut history_sweep = tokio::time::interval(PORTABLE_HISTORY_SWEEP_INTERVAL);
    history_sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,
            _ = history_sweep.tick() => {
                server.portable_history_cache.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner).prune();
            }
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
                        consecutive_accept_failures = 0;
                        // Chunked SSE writes each event as three small
                        // writes (size line, payload, CRLF). Nagle would
                        // hold those back waiting on delayed ACKs and add
                        // latency to every streamed token.
                        let _ = stream.set_nodelay(true);
                        // 休眠或断网后对端经常不发 FIN。keepalive 失败时，正在读的
                        // 连接会退出，不再一直占着任务。
                        enable_downstream_keepalive(&stream);
                        let server = Arc::clone(&server);
                        let admitted = match Arc::clone(&server.admitted_connections)
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
                                let _admitted = admitted;
                                // 客户端提前断开（取消请求、关闭会话）是正常结束，不记为失败。
                                if let Err(error) = server.handle_connection(stream).await
                                    && !error.is::<DownstreamClosed>()
                                    && !error.is::<CompactionInterrupted>()
                                {
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
                        consecutive_accept_failures =
                            consecutive_accept_failures.saturating_add(1);
                        // 同一轮连续失败只记一次，句柄耗尽时不会刷满错误日志。
                        if consecutive_accept_failures == 1 {
                            record_router_failure_nonblocking(
                                "local_router_accept_failed",
                                "accept_local_router_connection",
                                error.to_string(),
                                serde_json::json!({}),
                            );
                        }
                        tokio::select! {
                            _ = &mut shutdown_rx => break,
                            _ = tokio::time::sleep(accept_retry_delay(
                                consecutive_accept_failures,
                            )) => {}
                        }
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
}

impl LocalRouter {
    pub(crate) fn update_config(&self, config: &CodeyConfig) -> RouterSnapshotSwap {
        let installed = Arc::new(RouterSnapshot::from_config(config));
        let mut current = self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::mem::replace(&mut *current, Arc::clone(&installed));
        self.official_concurrency.update(&installed);
        drop(current);
        self.websocket_backoffs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .update_routes(&installed);
        RouterSnapshotSwap {
            previous,
            installed,
        }
    }

    /// Puts back the snapshot a failed delivery replaced. A snapshot that a
    /// later reload installed in the meantime stays in place.
    pub(crate) fn revert_config(&self, swap: RouterSnapshotSwap) -> bool {
        let mut current = self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !Arc::ptr_eq(&current, &swap.installed) {
            return false;
        }
        *current = Arc::clone(&swap.previous);
        self.official_concurrency.update(&swap.previous);
        drop(current);
        self.websocket_backoffs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .update_routes(&swap.previous);
        true
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

    pub(crate) async fn request_log_health(
        &self,
    ) -> crate::route_request_log::RouteRequestLogHealth {
        self.request_log.health().await
    }

    pub(crate) async fn stop(&self) -> Result<()> {
        if let Some(shutdown) = self
            .shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = shutdown.send(());
        }
        let task = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        crate::appserver_call::shutdown().await;
        task_result
    }
}

/// 上游 HTTP 客户端的统一构造参数；默认客户端和线路代理客户端共用，
/// 避免两者的连接与协议行为出现差异。
pub(crate) fn upstream_http_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(UPSTREAM_CONNECT_TIMEOUT)
        // Reuse a warm TLS connection across normal tool turns while
        // TCP probes evict half-open sockets before the next request.
        .pool_idle_timeout(Some(UPSTREAM_HTTP_POOL_IDLE_TIMEOUT))
        // 自适应窗口与下面的固定窗口互斥：它会覆盖两者并把初始连接窗口重置为
        // 65535，使 h2 的小 DATA 帧预算降到 32 KiB，见
        // UPSTREAM_HTTP2_INITIAL_STREAM_WINDOW_BYTES 附近的说明。
        .http2_initial_stream_window_size(UPSTREAM_HTTP2_INITIAL_STREAM_WINDOW_BYTES)
        .http2_initial_connection_window_size(UPSTREAM_HTTP2_INITIAL_CONNECTION_WINDOW_BYTES)
        // Pooled HTTP/2 connections can die silently behind NAT or
        // provider load balancers. PING frames while idle detect that
        // before the next request instead of spending its first
        // seconds on a dead socket. TCP keepalive below still covers
        // HTTP/1.1 upstreams.
        .http2_keep_alive_interval(Some(UPSTREAM_HTTP2_KEEPALIVE_INTERVAL))
        .http2_keep_alive_timeout(UPSTREAM_HTTP2_KEEPALIVE_TIMEOUT)
        .http2_keep_alive_while_idle(true)
        .tcp_nodelay(true)
        .tcp_keepalive(Some(UPSTREAM_TCP_KEEPALIVE_IDLE))
        .tcp_keepalive_interval(Some(UPSTREAM_TCP_KEEPALIVE_INTERVAL))
        .tcp_keepalive_retries(Some(UPSTREAM_TCP_KEEPALIVE_RETRIES))
        .redirect(reqwest::redirect::Policy::none())
}

#[cfg(not(test))]
pub(crate) fn outbound_proxy_applies_to_route(profile: &ProviderProfile) -> bool {
    if !profile.upstream_proxy.trim().is_empty() {
        return true;
    }
    let base_url = if profile.official_account {
        crate::codex_provider::official_route_base_url(profile)
    } else {
        profile.base_url.clone()
    };
    let base_url = base_url.as_str();
    outbound_proxy_applies_to_url_with_matcher(base_url, &SystemProxyMatcher::from_system())
}

#[cfg(test)]
pub(crate) fn outbound_proxy_applies_to_route(profile: &ProviderProfile) -> bool {
    // 测试不读系统代理，但线路级代理的行为（禁用上游 WebSocket）保持一致。
    !profile.upstream_proxy.trim().is_empty()
}

pub(crate) fn outbound_proxy_applies_to_url_with_matcher(
    url: &str,
    matcher: &SystemProxyMatcher,
) -> bool {
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

/// The snapshot a hot reload replaced, kept until the renderer confirms the
/// matching model list.
#[derive(Debug)]
pub(crate) struct RouterSnapshotSwap {
    previous: Arc<RouterSnapshot>,
    installed: Arc<RouterSnapshot>,
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) enum ConnectionPermitError {
    Busy,
    Closed,
}

/// HTTP 连接在名额用尽时立刻返回 503。已建立的 WebSocket 再等一小段时间，
/// 仍然没有名额就同样返回 503，而不是一直占着这条连接。
#[cfg(test)]
pub(crate) async fn acquire_connection_permit_within(
    limit: &Arc<Semaphore>,
    wait: Duration,
) -> std::result::Result<OwnedSemaphorePermit, ConnectionPermitError> {
    match tokio::time::timeout(wait, Arc::clone(limit).acquire_owned()).await {
        Ok(Ok(permit)) => Ok(permit),
        Ok(Err(_)) => Err(ConnectionPermitError::Closed),
        Err(_) => Err(ConnectionPermitError::Busy),
    }
}

pub(crate) fn accept_retry_delay(consecutive_failures: u32) -> Duration {
    let shift = consecutive_failures.saturating_sub(1).min(16);
    ACCEPT_RETRY_INITIAL_DELAY
        .saturating_mul(1_u32 << shift)
        .min(ACCEPT_RETRY_MAX_DELAY)
}

fn enable_downstream_keepalive(stream: &TcpStream) {
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(UPSTREAM_TCP_KEEPALIVE_IDLE)
        .with_interval(UPSTREAM_TCP_KEEPALIVE_INTERVAL)
        .with_retries(UPSTREAM_TCP_KEEPALIVE_RETRIES);
    let _ = socket2::SockRef::from(stream).set_tcp_keepalive(&keepalive);
}

pub(crate) struct RouterServer {
    pub(crate) endpoint: RuntimeRouterEndpoint,
    pub(crate) token: String,
    pub(crate) bearer_token: String,
    pub(crate) snapshot: Arc<RwLock<Arc<RouterSnapshot>>>,
    pub(crate) official_concurrency: Arc<OfficialConcurrency>,
    pub(crate) admitted_connections: Arc<Semaphore>,
    pub(crate) connection_limit: Arc<Semaphore>,
    pub(crate) rejection_limit: Arc<Semaphore>,
    pub(crate) request_body_budget: Arc<Semaphore>,
    pub(crate) bindings: Arc<Mutex<RouteBindings>>,
    pub(crate) websocket_backoffs: Arc<Mutex<UpstreamWebSocketBackoffs>>,
    pub(crate) native_history_cache: Arc<Mutex<NativeHistoryCache>>,
    pub(crate) portable_history_cache: Arc<Mutex<PortableHistoryCache>>,
    pub(crate) idle_downstreams: Arc<Mutex<IdleDownstreamRegistry>>,
    pub(crate) client: reqwest::Client,
    /// 按代理地址缓存的线路专用客户端，保留连接池复用。上限内缓存，
    /// 超出后清空重建（代理地址变更是罕见操作，代价仅为重建连接）。
    pub(crate) proxied_clients: Mutex<HashMap<String, reqwest::Client>>,
    pub(crate) official_auth_path: PathBuf,
    pub(crate) account_usage_cache:
        Arc<tokio::sync::Mutex<crate::account_usage::AccountUsageCaches>>,
    pub(crate) official_auth_cache: Arc<Mutex<crate::account_usage::OfficialAuthCaches>>,
    /// SIWC 直连凭据的读取缓存；与 `official_auth_cache` 分开，两套
    /// 凭据互不覆盖。
    pub(crate) siwc_auth_cache: Arc<Mutex<SiwcAuthCaches>>,
    pub(crate) request_log: Arc<RouteRequestLogController>,
}

const MAX_PROXIED_CLIENTS: usize = 8;

impl RouterServer {
    /// 选择线路的上游 HTTP 客户端：未配置代理的线路共用默认客户端（遵循
    /// 系统代理），配置了上游代理的线路使用仅走该代理的专用客户端。
    pub(crate) fn upstream_client(
        &self,
        route: &RouteTarget,
    ) -> std::result::Result<reqwest::Client, String> {
        let Some(proxy) = route.upstream_proxy.as_deref() else {
            return Ok(self.client.clone());
        };
        let mut clients = self
            .proxied_clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(client) = clients.get(proxy) {
            return Ok(client.clone());
        }
        let proxy_target = reqwest::Proxy::all(proxy).map_err(|_| {
            format!(
                "线路「{}」的上游代理地址无效，请检查线路设置",
                route.route_name
            )
        })?;
        let client = upstream_http_client_builder()
            .proxy(proxy_target)
            .build()
            .map_err(|_| {
                format!(
                    "线路「{}」无法创建上游代理客户端，请检查代理地址",
                    route.route_name
                )
            })?;
        if clients.len() >= MAX_PROXIED_CLIENTS {
            clients.clear();
        }
        clients.insert(proxy.to_string(), client.clone());
        Ok(client)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResponsesRequestKind {
    Create,
    Compact,
}

impl ResponsesRequestKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Create => "responses",
            Self::Compact => "responses_compact",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RouteBindings {
    pub(crate) portable_source_retries: HashMap<String, PortableSourceRetry>,
    pub(crate) portable_tickets: HashMap<String, PortableTicket>,
    pub(crate) compacting: HashSet<String>,
    pub(crate) compaction_operations: HashMap<Uuid, Arc<CompactionOperation>>,
    pub(crate) routes: HashMap<String, RouteBinding>,
    pub(crate) order: VecDeque<(String, u64)>,
    pub(crate) next_generation: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct RouteBinding {
    pub(crate) provider_id: String,
    pub(crate) generation: u64,
    pub(crate) upstream_model: String,
    pub(crate) context_config: Option<[u8; 32]>,
    pub(crate) portable: bool,
}

impl RouteBindings {
    pub(crate) fn route_for_keys(&self, keys: &[String]) -> Option<String> {
        // A concrete thread binding wins over its session-tree fallback. This
        // lets subagents self-route without changing the parent thread while
        // still giving metadata-free child/compaction requests a safe fallback.
        keys.iter().find_map(|key| {
            self.routes
                .get(key)
                .map(|binding| binding.provider_id.clone())
        })
    }

    pub(crate) fn remember(
        &mut self,
        keys: &[String],
        provider_id: &str,
        refresh_session_binding: bool,
    ) {
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
                    upstream_model: String::new(),
                    context_config: None,
                    portable: false,
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
pub(crate) struct RouterSnapshot {
    pub(crate) settings_revision: u64,
    pub(crate) declared_model_contexts: BTreeMap<String, crate::config::ModelContextConfig>,
    pub(crate) routes: HashMap<String, Arc<RouteTarget>>,
    pub(crate) aliases: HashMap<String, AliasTarget>,
    pub(crate) raw_models: HashMap<String, Vec<AliasTarget>>,
    pub(crate) model_alias_history: BTreeMap<String, String>,
    pub(crate) model_ids: Vec<String>,
    /// 默认账号所在官方线路。线路列表按默认账号在前的顺序派生，这里记住它
    /// 是为了让页头额度、审批复核这类没有账号信息的请求落到固定账号，而不是
    /// 在哈希表里随机挑一条官方线路。判定依据是凭据文档：默认账号的凭据就是
    /// Codex 登录本身，用户调整线路顺序不会改变这里的选择。
    pub(crate) default_official_provider: Option<String>,
    pub(crate) default_model: String,
    /// 杂事模型。仅在没有任何线路支持 `codex-auto-review` 时用于自动复核
    /// 请求。构建快照时解析当前线路，避免请求时通过历史别名改换上游。
    pub(crate) misc_model: Option<AliasTarget>,
    pub(crate) request_log_backend: RouteRequestLogBackend,
    pub(crate) request_log_catalog: RequestLogCatalog,
}

impl RouterSnapshot {
    pub(crate) fn from_config(config: &CodeyConfig) -> Self {
        let mut routes = HashMap::new();
        let mut aliases = HashMap::new();
        let mut raw_models = HashMap::<String, Vec<AliasTarget>>::new();
        let mut default_official_provider = None;
        let mut default_official_rank = None;
        for profile in config.runtime_profiles() {
            let provider_id = profile.provider_id().trim();
            // 凭据平面先于上游地址决定：SIWC 直连时对话走官方公开 API，
            // 账号保存的自定义网关只对 Codex backend-api 平面生效。
            let official_auth = official_route_auth(profile);
            let uses_siwc = official_auth
                .as_ref()
                .is_some_and(|auth| auth.siwc_path.is_some());
            let base_url = if profile.official_account {
                if uses_siwc {
                    crate::official_accounts::SIWC_BASE_URL.to_string()
                } else {
                    crate::codex_provider::official_route_base_url(profile)
                }
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
                upstream_proxy: Some(profile.upstream_proxy.trim().to_string())
                    .filter(|proxy| !proxy.is_empty()),
                protocol,
                official_account: profile.official_account,
                concurrency_key: profile.official_account.then(|| {
                    profile
                        .official_account_id
                        .as_deref()
                        .filter(|id| !id.trim().is_empty())
                        .map(|id| format!("account:{id}"))
                        .unwrap_or_else(|| format!("provider:{provider_id}"))
                }),
                max_concurrency: profile.max_concurrency,
                // Each stored account reads its own credential document, so
                // several official routes never share one login.
                official_auth,
                plugin_transport: profile.plugin_owner_id.as_ref().and_then(|id| {
                    profile
                        .plugin_route_spec
                        .as_ref()?
                        .transport
                        .as_ref()
                        .map(|options| plugin_transport::Target::new(id.clone(), options.clone()))
                }),
                // SIWC 上游没有持久会话端点；无论配置快照如何都不开 WS。
                supports_websockets: protocol == UpstreamProtocol::OpenAiResponses
                    && !uses_siwc
                    && config.route_supports_websockets_this_launch(profile),
                supports_remote_compaction: !uses_siwc
                    && config.route_supports_remote_compaction_this_launch(profile),
                remote_compaction_protocol: profile.remote_compaction_protocol,
                models: HashSet::new(),
                websocket_config: [0; 32],
                context_config: [0; 32],
            };
            target.context_config = target.context_config_fingerprint();
            target.websocket_config = target.websocket_config_fingerprint();
            for model in config.routable_profile_models(profile) {
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
                    .push(alias_target);
                target.models.insert(model);
            }
            let route_rank = target
                .official_account
                .then(|| default_route_rank(target.official_auth.as_ref()));
            routes.insert(provider_id.to_string(), Arc::new(target));
            if let Some(rank) = route_rank
                && default_official_rank.is_none_or(|best| rank < best)
            {
                default_official_rank = Some(rank);
                default_official_provider = Some(provider_id.to_string());
            }
        }
        let mut model_ids = raw_models
            .values()
            .filter_map(|models| models.first().map(|model| model.model.clone()))
            .collect::<Vec<_>>();
        model_ids.sort_unstable();
        Self {
            declared_model_contexts: config.runtime_enabled_model_contexts(),
            settings_revision: config.settings_revision,
            routes,
            aliases,
            raw_models,
            model_alias_history: config.model_alias_history.clone(),
            model_ids,
            default_official_provider,
            default_model: config.default_model().unwrap_or_default().to_string(),
            misc_model: config.misc_model_target().map(|target| AliasTarget {
                provider_id: target.provider_id,
                model: target.upstream_model,
            }),
            request_log_backend: config.route_request_log.backend,
            request_log_catalog: RequestLogCatalog::from_config(config),
        }
    }

    pub(crate) fn target_for_request(
        &self,
        requested_model: &str,
        route_hint: Option<&str>,
        bound_route: Option<&str>,
    ) -> Result<RouteSelection> {
        self.resolve_request(RouteRequest {
            requested_model,
            route_hint,
            bound_route,
        })
    }

    /// 图片请求不沿用对话线程绑定。当前模型别名明确指定线路，其余请求优先用线路提示；
    /// 无提示时用模型唯一对应的线路，模型未登记才使用默认线路，同名模型归属不明仍拒绝。
    #[cfg(test)]
    pub(crate) fn route_for_image_request(
        &self,
        model: &str,
        route_hint: Option<&str>,
    ) -> Result<Arc<RouteTarget>> {
        self.route_for_image_request_observed(model, route_hint, None)
            .map(|selection| selection.route)
    }

    pub(crate) fn route_for_image_request_observed(
        &self,
        model: &str,
        route_hint: Option<&str>,
        mut decision: Option<&mut crate::route_request_log::RouteDecision>,
    ) -> Result<RouteSelection> {
        use crate::route_request_log::RouteDecisionReason as Reason;
        let requested_model = model;
        let upstream_model = self.request_model_name(model);
        if let Some(decision) = decision.as_deref_mut() {
            decision.settings_revision = self.settings_revision;
            let key = model_id::key(model.trim());
            decision.historical_alias = !self.aliases.contains_key(&key)
                && !self.raw_models.contains_key(&key)
                && self.model_alias_history.contains_key(&key);
        }
        if let Some(alias) = self.active_model_alias(model) {
            record_decision(&mut decision, Reason::ExplicitAlias, 1);
            return self.target_for_route_model(&alias.provider_id, &alias.model, requested_model);
        }
        if let Some(route_hint) = route_hint.map(str::trim).filter(|hint| !hint.is_empty())
            && let Some(route) = self.routes.get(route_hint)
        {
            record_decision(&mut decision, Reason::RouteHint, 1);
            return Ok(RouteSelection {
                provider_id: route.provider_id.clone(),
                protocol: route.protocol,
                requested_model: requested_model.to_string(),
                route: Arc::clone(route),
                upstream_model: upstream_model.to_string(),
                fallback_reason: None,
            });
        }
        if route_hint.is_some()
            && let Some(decision) = decision.as_deref_mut()
        {
            decision.rejected_hint =
                Some(crate::route_request_log::RouteCheckRejection::RouteMissing);
        }
        let model = model.trim();
        if !model.is_empty()
            && let Some(candidates) = self.raw_models.get(&model_id::key(upstream_model))
        {
            if candidates.len() > 1 {
                record_decision(&mut decision, Reason::AmbiguousModel, candidates.len());
                anyhow::bail!("模型 {model} 同时存在于多条线路，缺少明确的 Codey 线路元数据");
            }
            if let Some(candidate) = candidates.first() {
                record_decision(&mut decision, Reason::UniqueCandidate, 1);
                return self.target_for_route_model(
                    &candidate.provider_id,
                    &candidate.model,
                    requested_model,
                );
            }
        }
        if let Some(decision) = decision.as_deref_mut() {
            decision.defaulted_model = true;
        }
        self.resolve_request_observed(
            RouteRequest {
                requested_model: self.default_model.trim(),
                route_hint: None,
                bound_route: None,
            },
            decision,
        )
        .map(|mut selection| {
            // 默认文本模型只确定图片请求所用线路，不能替换图片模型。
            selection.requested_model = requested_model.to_string();
            selection.upstream_model = upstream_model.to_string();
            selection
        })
    }

    /// 只根据可信目录恢复一层别名；原始模型本身可以含有斜杠。
    pub(crate) fn request_model_name<'a>(&'a self, requested_model: &'a str) -> &'a str {
        let key = model_id::key(requested_model.trim());
        if let Some(alias) = self.aliases.get(&key) {
            return &alias.model;
        }
        if !self.raw_models.contains_key(&key)
            && let Some(model) = self.model_alias_history.get(&key)
        {
            return model;
        }
        requested_model
    }

    pub(crate) fn active_model_alias(&self, requested_model: &str) -> Option<&AliasTarget> {
        // 历史别名只提供模型恢复信息，不能作为本次请求的明确线路选择。
        self.aliases.get(&model_id::key(requested_model.trim()))
    }

    pub(crate) fn resolve_request(&self, request: RouteRequest<'_>) -> Result<RouteSelection> {
        self.resolve_request_observed(request, None)
    }

    pub(crate) fn resolve_request_observed(
        &self,
        request: RouteRequest<'_>,
        mut decision: Option<&mut crate::route_request_log::RouteDecision>,
    ) -> Result<RouteSelection> {
        let result = self.resolve_request_inner(request, decision.as_deref_mut());
        if result.is_err()
            && let Some(decision) = decision
        {
            use crate::route_request_log::RouteDecisionReason as Reason;
            if !matches!(
                decision.reason,
                Reason::MissingModel | Reason::AmbiguousModel | Reason::ModelNotEnabled
            ) {
                decision.reason = Reason::RouteUnavailable;
            }
        }
        result
    }

    fn resolve_request_inner(
        &self,
        request: RouteRequest<'_>,
        mut decision: Option<&mut crate::route_request_log::RouteDecision>,
    ) -> Result<RouteSelection> {
        if let Some(decision) = decision.as_deref_mut() {
            decision.settings_revision = self.settings_revision;
        }
        let requested_model = request.requested_model.trim();
        if requested_model.is_empty() {
            record_decision(
                &mut decision,
                crate::route_request_log::RouteDecisionReason::MissingModel,
                0,
            );
            anyhow::bail!("请求缺少 model 字段");
        }
        if let Some(alias) = self.active_model_alias(requested_model) {
            record_decision(
                &mut decision,
                crate::route_request_log::RouteDecisionReason::ExplicitAlias,
                1,
            );
            // A qualified `provider/model` selector already identifies the
            // route. Codex can replay client metadata from an earlier turn, so
            // an independent route hint must not redirect an explicit alias.
            return self.target_for_route_model(&alias.provider_id, &alias.model, requested_model);
        }
        let requested_key = model_id::key(requested_model);
        if !self.raw_models.contains_key(&requested_key)
            && let Some(source_model) = self
                .model_alias_history
                .get(&requested_key)
                .map(String::as_str)
        {
            if let Some(decision) = decision.as_deref_mut() {
                decision.historical_alias = true;
            }
            // Resolve a recorded upstream id as raw data, never recursively as
            // another selector (an upstream id can itself contain a slash).
            return self
                .resolve_raw_request_observed(
                    &model_id::key(source_model),
                    request.route_hint,
                    request.bound_route,
                    requested_model,
                    decision,
                )
                .with_context(|| {
                    format!(
                        "历史线路已不可用：{requested_model}；请为模型 {source_model} 选择可用线路"
                    )
                });
        }
        self.resolve_raw_request_observed(
            &requested_key,
            request.route_hint,
            request.bound_route,
            requested_model,
            decision,
        )
    }

    /// 当前与历史模型 id 共用同一套优先级；诊断只记录实际执行的分支。
    fn resolve_raw_request_observed(
        &self,
        model_key: &str,
        route_hint: Option<&str>,
        bound_route: Option<&str>,
        requested_model: &str,
        mut decision: Option<&mut crate::route_request_log::RouteDecision>,
    ) -> Result<RouteSelection> {
        use crate::route_request_log::RouteDecisionReason as Reason;
        let candidates = self
            .raw_models
            .get(model_key)
            .map(Vec::as_slice)
            .unwrap_or_default();
        record_decision(&mut decision, Reason::NotResolved, candidates.len());
        let auto_review = model_id::equal(model_key, CODEX_AUTO_REVIEW_MODEL);
        if let Some(route_hint) = route_hint
            && let Some(candidate) = candidates
                .iter()
                .find(|candidate| candidate.provider_id == route_hint)
        {
            record_decision(&mut decision, Reason::RouteHint, candidates.len());
            return self.target_for_route_model(route_hint, &candidate.model, requested_model);
        }
        if let (Some(hint), Some(decision)) = (route_hint, decision.as_deref_mut()) {
            decision.rejected_hint = Some(self.rejection_for_route(hint));
        }
        // Raw ids in the mixed runtime catalog are native OpenAI entries;
        // third-party selections remain route-qualified. An explicit hint
        // above can still select a third-party route with the same model.
        // 同名模型在多条官方线路并存时优先给默认账号，避免随机消耗其它账号额度。
        if !auto_review
            && let Some(official) = self
                .default_official_provider
                .as_deref()
                .and_then(|provider_id| {
                    candidates
                        .iter()
                        .find(|candidate| candidate.provider_id == provider_id)
                })
                .filter(|candidate| {
                    self.routes
                        .get(&candidate.provider_id)
                        .is_some_and(|route| route.official_account)
                })
                .or_else(|| {
                    candidates.iter().find(|candidate| {
                        self.routes
                            .get(&candidate.provider_id)
                            .is_some_and(|route| route.official_account)
                    })
                })
        {
            record_decision(&mut decision, Reason::DefaultOfficial, candidates.len());
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
            record_decision(&mut decision, Reason::SessionBinding, candidates.len());
            return self.target_for_route_model(bound_route, &candidate.model, requested_model);
        }
        if let (Some(bound), Some(decision)) = (bound_route, decision.as_deref_mut()) {
            decision.rejected_binding = Some(self.rejection_for_route(bound));
        }
        // Codex starts automatic approval review as a separate request with a
        // fixed hidden model. Some builds omit turn route metadata on that
        // request, so prefer the official route when no capable hint or thread
        // binding identified a route above. A capable bound third-party route
        // still wins before this fallback.
        // 多账号并存时优先用默认账号，避免复核请求随机消耗其它账号的额度。
        if auto_review
            && let Some(official_route) = self
                .default_official_provider
                .as_deref()
                .and_then(|provider_id| self.routes.get(provider_id))
                .filter(|target| {
                    target.official_account && target.models.contains(CODEX_AUTO_REVIEW_MODEL)
                })
                .or_else(|| {
                    self.routes.values().find(|target| {
                        target.official_account && target.models.contains(CODEX_AUTO_REVIEW_MODEL)
                    })
                })
        {
            record_decision(&mut decision, Reason::AutoReviewOfficial, candidates.len());
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
            record_decision(&mut decision, Reason::UniqueCandidate, 1);
            let candidate = &candidates[0];
            return self.target_for_route_model(
                &candidate.provider_id,
                &candidate.model,
                requested_model,
            );
        }
        if candidates.len() > 1 {
            record_decision(&mut decision, Reason::AmbiguousModel, candidates.len());
            anyhow::bail!("模型 {requested_model} 同时存在于多条线路，缺少明确的 Codey 线路元数据");
        }
        // 没有任何线路支持自动复核时，用杂事模型承接这一请求。这里保留
        // 请求里的原始模型名，让下游的日志与提示头仍能看出这是一次回退。
        if auto_review
            && let Some(misc) = &self.misc_model
            && let Ok(mut selection) =
                self.target_for_route_model(&misc.provider_id, &misc.model, requested_model)
        {
            record_decision(&mut decision, Reason::AutoReviewMiscModel, candidates.len());
            selection.fallback_reason = Some("auto_review_misc_model".to_string());
            return Ok(selection);
        }
        record_decision(&mut decision, Reason::ModelNotEnabled, 0);
        anyhow::bail!("模型未在线路路由表中启用：{requested_model}")
    }

    fn rejection_for_route(
        &self,
        provider_id: &str,
    ) -> crate::route_request_log::RouteCheckRejection {
        if self.routes.contains_key(provider_id) {
            crate::route_request_log::RouteCheckRejection::ModelNotEnabled
        } else {
            crate::route_request_log::RouteCheckRejection::RouteMissing
        }
    }

    #[cfg(test)]
    pub(crate) fn target_for_model(&self, requested_model: &str) -> Result<RouteSelection> {
        self.target_for_request(requested_model, None, None)
    }

    pub(crate) fn target_for_route_model(
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
            fallback_reason: None,
        })
    }

    pub(crate) fn model_ids(&self) -> &[String] {
        &self.model_ids
    }

    #[cfg(test)]
    pub(crate) fn model_aliases(&self) -> Vec<String> {
        let mut aliases = self.aliases.keys().cloned().collect::<Vec<_>>();
        aliases.sort_unstable();
        aliases
    }
}

fn record_decision(
    decision: &mut Option<&mut crate::route_request_log::RouteDecision>,
    reason: crate::route_request_log::RouteDecisionReason,
    count: usize,
) {
    if let Some(decision) = decision.as_deref_mut() {
        decision.reason = reason;
        decision.candidate_count = count as u64;
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RouteTarget {
    pub(crate) provider_id: String,
    pub(crate) route_name: String,
    pub(crate) upstream_url: std::result::Result<String, String>,
    pub(crate) upstream_compact_url: std::result::Result<String, String>,
    pub(crate) upstream_websocket_url: std::result::Result<String, String>,
    pub(crate) upstream_headers: std::result::Result<HeaderMap, String>,
    pub(crate) upstream_authority: String,
    pub(crate) upstream_proxy: Option<String>,
    pub(crate) protocol: UpstreamProtocol,
    pub(crate) official_account: bool,
    pub(crate) concurrency_key: Option<String>,
    pub(crate) max_concurrency: u32,
    pub(crate) official_auth: Option<OfficialRouteAuth>,
    pub(crate) plugin_transport: Option<plugin_transport::Target>,
    pub(crate) supports_websockets: bool,
    pub(crate) supports_remote_compaction: bool,
    pub(crate) remote_compaction_protocol: crate::config::RemoteCompactionProtocol,
    pub(crate) models: HashSet<String>,
    pub(crate) websocket_config: [u8; 32],
    pub(crate) context_config: [u8; 32],
}

/// Where one derived official route reads its ChatGPT credential, and whether
/// that document is the Codex home login Codex refreshes itself.
#[derive(Clone, Debug)]
pub(crate) struct OfficialRouteAuth {
    pub(crate) account_id: String,
    pub(crate) email: Option<String>,
    pub(crate) path: PathBuf,
    /// 受管账号记录独立于路由快照和凭据缓存；删除后立即拒绝新的授权准备。
    pub(crate) record_path: PathBuf,
    /// SIWC 直连凭据的物化文件；有值时该线路的对话流量走官方公开 API，
    /// `path` 里的 Codex 凭据只留给 backend-api 专属能力。
    pub(crate) siwc_path: Option<PathBuf>,
    pub(crate) accepts_incoming_authorization: bool,
}

fn official_route_auth(profile: &crate::config::ProviderProfile) -> Option<OfficialRouteAuth> {
    if !profile.official_account {
        return None;
    }
    let account_id = profile.official_account_id.clone()?;
    let codex_home = crate::codex_config::codex_home();
    let store = crate::official_accounts::OfficialAccountStore::for_config_path(
        &crate::config::default_config_path(),
    );
    let path = store.credential_path(codex_home, &account_id);
    let record = store.get(&account_id).ok().flatten();
    // 账号邮箱随路由快照读取，生命周期不接受客户端提供的邮箱。
    let email = record.as_ref().and_then(|record| record.email.clone());
    // SIWC 槽位有效时路由切到直连平面；失效凭据回落到 Codex 通道，下一次
    // 账号同步会同时刷新 `profile.official_siwc`。
    let siwc_path = record
        .as_ref()
        .is_some_and(|record| record.siwc_active())
        .then(|| store.siwc_path(&account_id));
    // Codex refreshes the default account's copy in place and sends its token
    // with every request; idle accounts keep their own stored document. SIWC
    // 线路不接受下游自带的令牌——那是 Codex 凭据，对公开 API 无效。
    let accepts_incoming_authorization =
        siwc_path.is_none() && path == codex_home.join("auth.json");
    Some(OfficialRouteAuth {
        record_path: store.account_path(&account_id),
        account_id,
        email,
        path,
        siwc_path,
        accepts_incoming_authorization,
    })
}

/// 排序默认账号所在的官方线路。凭据文档就是 Codex 登录本身的那条线路，以及
/// 升级前直接复用 Codex 登录的旧线路，都代表客户端当前的身份；其余存储账号
/// 的线路排在后面，页头额度和审批复核只认第一条。
pub(crate) fn default_route_rank(auth: Option<&OfficialRouteAuth>) -> u8 {
    match auth {
        Some(auth) if auth.accepts_incoming_authorization => 0,
        None => 0,
        Some(_) => 1,
    }
}

impl RouteTarget {
    /// 该线路是否处于 SIWC 直连平面：上游是 OpenAI 公开 API，凭据来自账号的
    /// SIWC 槽位，与 Codex backend-api 线路的请求头、请求体规则都不同。
    pub(crate) fn uses_siwc(&self) -> bool {
        self.official_auth
            .as_ref()
            .is_some_and(|auth| auth.siwc_path.is_some())
    }

    fn websocket_config_fingerprint(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update([u8::from(self.supports_websockets)]);
        digest.update(self.context_config);
        digest.finalize().into()
    }

    fn context_config_fingerprint(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update([u8::from(self.official_account)]);
        if let Some(plugin) = &self.plugin_transport {
            update_length_prefixed_digest(&mut digest, plugin.plugin_id.as_bytes());
            update_length_prefixed_digest(&mut digest, plugin.options.account_email.as_bytes());
        }
        // 每个官方账号使用独立的连接池身份，避免不同账号的登录态互相影响。
        if let Some(auth) = &self.official_auth {
            update_length_prefixed_digest(&mut digest, auth.account_id.as_bytes());
        }
        if let Some(proxy) = &self.upstream_proxy {
            update_length_prefixed_digest(&mut digest, proxy.as_bytes());
        }
        if let Ok(url) = &self.upstream_url {
            update_length_prefixed_digest(&mut digest, url.as_bytes());
        }
        if let Ok(url) = &self.upstream_websocket_url {
            update_length_prefixed_digest(&mut digest, url.as_bytes());
        }
        if let Ok(headers) = &self.upstream_headers {
            let mut headers = headers.iter().collect::<Vec<_>>();
            headers.sort_unstable_by(|(a, av), (b, bv)| {
                a.as_str()
                    .cmp(b.as_str())
                    .then_with(|| av.as_bytes().cmp(bv.as_bytes()))
            });
            for (name, value) in headers {
                update_length_prefixed_digest(&mut digest, name.as_str().as_bytes());
                update_length_prefixed_digest(&mut digest, value.as_bytes());
            }
        }
        digest.finalize().into()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AliasTarget {
    pub(crate) provider_id: String,
    pub(crate) model: String,
}

#[derive(Clone, Debug)]
pub(crate) struct RouteSelection {
    pub(crate) provider_id: String,
    pub(crate) protocol: UpstreamProtocol,
    pub(crate) requested_model: String,
    pub(crate) route: Arc<RouteTarget>,
    pub(crate) upstream_model: String,
    /// 本次选择相对请求模型做过的降级说明，写入请求日志。
    pub(crate) fallback_reason: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RouteRequest<'a> {
    pub(crate) requested_model: &'a str,
    pub(crate) route_hint: Option<&'a str>,
    pub(crate) bound_route: Option<&'a str>,
}

pub(crate) use crate::model_id::model_alias;

#[cfg(test)]
mod decision_tests {
    use super::*;
    use crate::route_request_log::{
        RouteCheckRejection as Rejection, RouteDecision, RouteDecisionReason as Reason,
    };

    #[test]
    fn decision_records_only_the_priority_branches_actually_considered() {
        let (mut config, provider, model) =
            super::super::tests::router_config("https://example.test/v1".into());
        let mut second = config.profiles[0].clone();
        second.id = "route-b".into();
        config.profiles.push(second);
        config
            .selected_models_by_provider
            .insert("route-b".into(), vec![model.clone()]);
        let historical = model_alias("retired", &model);
        config
            .model_alias_history
            .insert(historical.clone(), model.clone());
        config.settings_revision = 17;
        let snapshot = RouterSnapshot::from_config(&config);
        for (requested, hint, bound, reason, historical_alias) in [
            (
                model_alias(&provider, &model),
                Some("missing"),
                Some("missing"),
                Reason::ExplicitAlias,
                false,
            ),
            (
                model.clone(),
                Some("route-b"),
                Some(&*provider),
                Reason::RouteHint,
                false,
            ),
            (
                model.clone(),
                Some("missing"),
                Some(&*provider),
                Reason::SessionBinding,
                false,
            ),
            (
                historical,
                None,
                Some(&*provider),
                Reason::SessionBinding,
                true,
            ),
        ] {
            let mut decision = RouteDecision::default();
            snapshot
                .resolve_request_observed(
                    RouteRequest {
                        requested_model: &requested,
                        route_hint: hint,
                        bound_route: bound,
                    },
                    Some(&mut decision),
                )
                .unwrap();
            assert_eq!(decision.settings_revision, 17);
            assert_eq!(decision.reason, reason);
            assert_eq!(decision.historical_alias, historical_alias);
            assert_eq!(
                decision.candidate_count,
                if reason == Reason::ExplicitAlias {
                    1
                } else {
                    2
                }
            );
            assert_eq!(
                decision.rejected_hint,
                (reason == Reason::SessionBinding && hint.is_some())
                    .then_some(Rejection::RouteMissing)
            );
            assert_eq!(decision.rejected_binding, None);
        }
        let mut decision = RouteDecision::default();
        assert!(
            snapshot
                .resolve_request_observed(
                    RouteRequest {
                        requested_model: &model,
                        route_hint: Some("missing"),
                        bound_route: None
                    },
                    Some(&mut decision)
                )
                .is_err()
        );
        assert_eq!(decision.reason, Reason::AmbiguousModel);
        assert_eq!(decision.candidate_count, 2);
        assert_eq!(decision.rejected_hint, Some(Rejection::RouteMissing));

        let mut official = snapshot.clone();
        Arc::make_mut(official.routes.get_mut(&provider).unwrap()).official_account = true;
        official.default_official_provider = Some(provider.clone());
        let mut decision = RouteDecision::default();
        official
            .resolve_request_observed(
                RouteRequest {
                    requested_model: &model,
                    route_hint: None,
                    bound_route: Some("route-b"),
                },
                Some(&mut decision),
            )
            .unwrap();
        assert_eq!(decision.reason, Reason::DefaultOfficial);
        assert_eq!(decision.rejected_binding, None);
    }

    #[tokio::test]
    async fn hot_reload_decisions_report_the_snapshot_revision_used_by_the_request() {
        let (mut config, _, model) =
            super::super::tests::router_config("http://127.0.0.1:9/v1".into());
        config.settings_revision = 7;
        let router = LocalRouter::start(&config).await.unwrap();
        let original = router.snapshot.read().unwrap().clone();
        config.settings_revision = 8;
        router.update_config(&config);
        for (snapshot, revision) in [(original, 7), (router.snapshot.read().unwrap().clone(), 8)] {
            let mut decision = RouteDecision::default();
            snapshot
                .resolve_request_observed(
                    RouteRequest {
                        requested_model: &model,
                        route_hint: None,
                        bound_route: Some("missing"),
                    },
                    Some(&mut decision),
                )
                .unwrap();
            assert_eq!(decision.settings_revision, revision);
            assert_eq!(decision.reason, Reason::UniqueCandidate);
            assert_eq!(decision.rejected_binding, Some(Rejection::RouteMissing));
        }
    }
}
