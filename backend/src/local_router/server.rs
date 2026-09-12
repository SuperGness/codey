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
    pub(crate) endpoint: RuntimeRouterEndpoint,
    pub(crate) snapshot: Arc<RwLock<Arc<RouterSnapshot>>>,
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
        account_usage_cache: Arc<tokio::sync::Mutex<crate::account_usage::AccountUsageCache>>,
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
        account_usage_cache: Arc<tokio::sync::Mutex<crate::account_usage::AccountUsageCache>>,
    ) -> Result<Self> {
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
        let server = RouterServer {
            token: endpoint.token.clone(),
            bearer_token: format!("Bearer {}", endpoint.token),
            snapshot: Arc::clone(&snapshot),
            connection_limit: Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS)),
            rejection_limit: Arc::new(Semaphore::new(MAX_CONCURRENT_REJECTIONS)),
            request_body_budget: Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS)),
            bindings: Arc::new(Mutex::new(RouteBindings::default())),
            websocket_backoffs: Arc::clone(&websocket_backoffs),
            native_history_cache: Arc::new(Mutex::new(NativeHistoryCache::default())),
            client: reqwest::Client::builder()
                .connect_timeout(UPSTREAM_CONNECT_TIMEOUT)
                // Reuse a warm TLS connection across normal tool turns while
                // TCP probes evict half-open sockets before the next request.
                .pool_idle_timeout(Some(UPSTREAM_HTTP_POOL_IDLE_TIMEOUT))
                .http2_adaptive_window(true)
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
                .build()
                .context("创建 Codey 本地路由 HTTP 客户端失败")?,
            official_auth_path,
            account_usage_cache,
            official_auth_cache: Arc::new(Mutex::new(
                crate::account_usage::OfficialAuthCache::default(),
            )),
            request_log: Arc::clone(&request_log),
        };
        let server = Arc::new(server);
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
                                let server = Arc::clone(&server);
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
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::clone(&next);
        self.websocket_backoffs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .update_routes(&next);
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

pub(crate) struct RouterServer {
    pub(crate) token: String,
    pub(crate) bearer_token: String,
    pub(crate) snapshot: Arc<RwLock<Arc<RouterSnapshot>>>,
    pub(crate) connection_limit: Arc<Semaphore>,
    pub(crate) rejection_limit: Arc<Semaphore>,
    pub(crate) request_body_budget: Arc<Semaphore>,
    pub(crate) bindings: Arc<Mutex<RouteBindings>>,
    pub(crate) websocket_backoffs: Arc<Mutex<UpstreamWebSocketBackoffs>>,
    pub(crate) native_history_cache: Arc<Mutex<NativeHistoryCache>>,
    pub(crate) client: reqwest::Client,
    pub(crate) official_auth_path: PathBuf,
    pub(crate) account_usage_cache:
        Arc<tokio::sync::Mutex<crate::account_usage::AccountUsageCache>>,
    pub(crate) official_auth_cache: Arc<Mutex<crate::account_usage::OfficialAuthCache>>,
    pub(crate) request_log: Arc<RouteRequestLogController>,
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
    pub(crate) compacting: HashSet<String>,
    pub(crate) routes: HashMap<String, RouteBinding>,
    pub(crate) order: VecDeque<(String, u64)>,
    pub(crate) next_generation: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct RouteBinding {
    pub(crate) provider_id: String,
    pub(crate) generation: u64,
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
    pub(crate) routes: HashMap<String, Arc<RouteTarget>>,
    pub(crate) aliases: HashMap<String, AliasTarget>,
    pub(crate) raw_models: HashMap<String, Vec<AliasTarget>>,
    pub(crate) model_alias_history: BTreeMap<String, String>,
    pub(crate) model_ids: Vec<String>,
    pub(crate) default_model: String,
    pub(crate) request_log_backend: RouteRequestLogBackend,
    pub(crate) request_log_catalog: RequestLogCatalog,
}

impl RouterSnapshot {
    pub(crate) fn from_config(config: &CodeyConfig) -> Self {
        let mut routes = HashMap::new();
        let mut aliases = HashMap::new();
        let mut raw_models = HashMap::<String, Vec<AliasTarget>>::new();
        for profile in &config.profiles {
            if !profile.enabled {
                continue;
            }
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
                supports_remote_compaction: config
                    .route_supports_remote_compaction_this_launch(profile),
                models: HashSet::new(),
                websocket_config: [0; 32],
                context_config: [0; 32],
            };
            target.context_config = target.context_config_fingerprint();
            target.websocket_config = target.websocket_config_fingerprint();
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

    pub(crate) fn target_for_auxiliary_request(
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

    pub(crate) fn resolve_request(&self, request: RouteRequest<'_>) -> Result<RouteSelection> {
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

    pub(crate) fn resolve_raw_request(
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

#[derive(Clone, Debug)]
pub(crate) struct RouteTarget {
    pub(crate) provider_id: String,
    pub(crate) route_name: String,
    pub(crate) upstream_url: std::result::Result<String, String>,
    pub(crate) upstream_compact_url: std::result::Result<String, String>,
    pub(crate) upstream_websocket_url: std::result::Result<String, String>,
    pub(crate) upstream_headers: std::result::Result<HeaderMap, String>,
    pub(crate) upstream_authority: String,
    pub(crate) protocol: UpstreamProtocol,
    pub(crate) official_account: bool,
    pub(crate) supports_websockets: bool,
    pub(crate) supports_remote_compaction: bool,
    pub(crate) models: HashSet<String>,
    pub(crate) websocket_config: [u8; 32],
    pub(crate) context_config: [u8; 32],
}

impl RouteTarget {
    fn websocket_config_fingerprint(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update([u8::from(self.supports_websockets)]);
        digest.update(self.context_config);
        digest.finalize().into()
    }

    fn context_config_fingerprint(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update([u8::from(self.official_account)]);
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
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RouteRequest<'a> {
    pub(crate) requested_model: &'a str,
    pub(crate) route_hint: Option<&'a str>,
    pub(crate) bound_route: Option<&'a str>,
}

pub(crate) fn route_models(
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

pub(crate) use crate::model_id::model_alias;
