use super::*;

// 预算耗尽时仍允许小型中断事件；每条连接最多保留一条，并立即转发或丢弃。
// 该固定上限也限制了紧急路径上的 JSON 解析和分配。
const MAX_EMERGENCY_INTERRUPT_BYTES: usize = 4 * 1024;
const MAX_PENDING_WEBSOCKET_MESSAGES: usize = 8;

#[derive(Debug)]
pub(crate) struct BudgetedWebSocketMessage {
    pub(crate) message: WebSocketMessage,
    pub(crate) parsed_body: Option<serde_json::Result<Value>>,
    pub(crate) permit: Option<OwnedSemaphorePermit>,
}

impl BudgetedWebSocketMessage {
    fn body(&self) -> Option<&Value> {
        self.parsed_body.as_ref()?.as_ref().ok()
    }

    fn is_interrupt(&self) -> bool {
        self.body()
            .is_some_and(|body| body["type"] == "response.interrupt")
    }
}

async fn parse_websocket_message(
    message: WebSocketMessage,
    permit: Option<OwnedSemaphorePermit>,
) -> Result<BudgetedWebSocketMessage> {
    let offload = matches!(&message, WebSocketMessage::Text(text) if text.len() >= REQUEST_JSON_OFFLOAD_BYTES);
    let parse = move || {
        let parsed_body = match &message {
            WebSocketMessage::Text(text) => Some(serde_json::from_str(text.as_str())),
            _ => None,
        };
        BudgetedWebSocketMessage {
            message,
            parsed_body,
            permit,
        }
    };
    if offload {
        // 任务取消后，正文和许可仍由 blocking 任务持有，直到解析结果被释放。
        tokio::task::spawn_blocking(parse)
            .await
            .context("等待大型 Responses WebSocket JSON 解析任务失败")
    } else {
        Ok(parse())
    }
}

pub(crate) enum IdleDownstreamWait {
    Evicted,
    Message(Option<std::result::Result<WebSocketMessage, tokio_tungstenite::tungstenite::Error>>),
    TimedOut,
}

pub(crate) enum IdleWebSocketEvent {
    Downstream(
        Option<std::result::Result<WebSocketMessage, tokio_tungstenite::tungstenite::Error>>,
    ),
    Upstream(Option<std::result::Result<WebSocketMessage, tokio_tungstenite::tungstenite::Error>>),
    MaintainUpstream,
    ConfigurationChanged,
    Evicted,
}

/// 空闲下游 WebSocket 的登记表。名额按注册顺序保留，超出上限时关闭最久的一条。
/// 空闲等待下一条请求时，对端不发 Close 帧直接断开 TCP 是连接的正常结束，
/// 不能当作路由失败记录。
fn idle_downstream_message(
    message: Option<std::result::Result<WebSocketMessage, tokio_tungstenite::tungstenite::Error>>,
) -> Result<Option<WebSocketMessage>> {
    use tokio_tungstenite::tungstenite::{Error, error::ProtocolError};
    match message {
        Some(Err(
            Error::ConnectionClosed
            | Error::AlreadyClosed
            | Error::Protocol(ProtocolError::ResetWithoutClosingHandshake),
        )) => Ok(None),
        Some(Err(Error::Io(error)))
            if matches!(
                error.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
            ) =>
        {
            Ok(None)
        }
        message => message
            .transpose()
            .context("读取 Codey Responses WebSocket 消息失败"),
    }
}

#[derive(Default)]
pub(crate) struct IdleDownstreamRegistry {
    next_id: u64,
    waiters: HashMap<u64, oneshot::Sender<()>>,
}

pub(crate) struct IdleGuard {
    id: u64,
    registry: Arc<Mutex<IdleDownstreamRegistry>>,
}

impl Drop for IdleGuard {
    fn drop(&mut self) {
        self.registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .waiters
            .remove(&self.id);
    }
}

impl IdleDownstreamRegistry {
    pub(crate) fn register(registry: &Arc<Mutex<Self>>) -> (IdleGuard, oneshot::Receiver<()>) {
        let (evict_tx, evict_rx) = oneshot::channel();
        let mut slots = registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = slots.next_id;
        slots.next_id = slots.next_id.wrapping_add(1);
        slots.waiters.insert(id, evict_tx);
        while slots.waiters.len() > MAX_CONCURRENT_CONNECTIONS {
            let Some(oldest) = slots
                .waiters
                .keys()
                .copied()
                .filter(|waiter| *waiter != id)
                .min()
            else {
                break;
            };
            if let Some(evict) = slots.waiters.remove(&oldest) {
                let _ = evict.send(());
            }
        }
        drop(slots);
        (
            IdleGuard {
                id,
                registry: Arc::clone(registry),
            },
            evict_rx,
        )
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.waiters.len()
    }
}

impl WebSocketResponsesDownstream {
    #[cfg(test)]
    pub(crate) fn new(socket: BudgetedWebSocket) -> Self {
        Self::with_shared_backoffs(
            socket,
            Arc::new(Mutex::new(UpstreamWebSocketBackoffs::default())),
            Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS)),
            Arc::new(Mutex::new(IdleDownstreamRegistry::default())),
        )
    }

    pub(crate) fn with_shared_backoffs(
        mut socket: BudgetedWebSocket,
        websocket_backoffs: Arc<Mutex<UpstreamWebSocketBackoffs>>,
        request_body_budget: Arc<Semaphore>,
        idle_registry: Arc<Mutex<IdleDownstreamRegistry>>,
    ) -> Self {
        socket.set_budget(Arc::clone(&request_body_budget));
        let config_changes = websocket_backoffs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .changes
            .subscribe();
        Self {
            socket,
            upstream: None,
            websocket_backoffs,
            stream_id: None,
            adapted_history: AdaptedResponsesHistory::default(),
            native_history: NativeResponsesHistory::default(),
            steering: SteeringState::default(),
            pending_interrupt: None,
            interrupt_forwarding: false,
            terminal_started: false,
            pending_messages: VecDeque::new(),
            request_body_budget,
            config_changes,
            idle_registry,
        }
    }

    pub(crate) fn set_stream_id(&mut self, stream_id: Option<String>) {
        self.stream_id = stream_id;
    }

    pub(crate) fn clear_stream_id(&mut self) {
        self.stream_id = None;
        self.terminal_started = false;
    }

    pub(crate) async fn write_steering_event(&mut self, event: &Value) -> Result<()> {
        let encoded = encode_responses_websocket_event(event, None)?;
        // 控制事件不经过响应失败归一化，不结束正在生成的响应。
        self.write_text(encoded).await
    }

    pub(crate) async fn write_steering_notifications(&mut self) -> Result<()> {
        for event in self.steering.notifications() {
            self.write_steering_event(&event).await?;
        }
        Ok(())
    }

    pub(crate) async fn handle_steering_message(
        &mut self,
        message: &BudgetedWebSocketMessage,
    ) -> Result<bool> {
        let Some(body) = message.body() else {
            return Ok(false);
        };
        if body["type"] != "response.steer" {
            return Ok(false);
        }
        let event = self.steering.accept(body, &self.request_body_budget);
        self.write_steering_event(&event).await?;
        Ok(true)
    }

    fn take_response_interrupt(&mut self) -> Option<BudgetedWebSocketMessage> {
        self.pending_interrupt.take()
    }

    pub(crate) fn handle_idle_response_interrupt(
        &mut self,
        message: &BudgetedWebSocketMessage,
    ) -> Result<bool> {
        Ok(message.is_interrupt())
    }

    async fn reject_message(&mut self, code: &str, message: &str, waiting: bool) -> Result<()> {
        let failure = ResponsesFailure::new(503, code, message.into(), None);
        if waiting {
            // 拒绝的是后续请求，不得结束当前响应或清除其续接状态。
            self.write_steering_event(&failure.normalized_event()).await
        } else {
            self.clear_stream_id();
            self.write_response_failure(&failure).await
        }
    }

    async fn admit_message(
        &mut self,
        message: WebSocketMessage,
        waiting: bool,
    ) -> Result<Option<BudgetedWebSocketMessage>> {
        if !matches!(
            message,
            WebSocketMessage::Text(_) | WebSocketMessage::Binary(_)
        ) {
            return parse_websocket_message(message, None).await.map(Some);
        }
        let reservation = match self.socket.take_message_permit() {
            Some(permit) => Ok(Some(permit)),
            None => acquire_request_body_budget(&self.request_body_budget, message.len()),
        };
        let permit = match reservation {
            Ok(permit) => permit,
            Err(error)
                if error
                    .downcast_ref::<RequestBodyBudgetUnavailable>()
                    .is_some() =>
            {
                if let WebSocketMessage::Text(text) = &message
                    && text.len() <= MAX_EMERGENCY_INTERRUPT_BYTES
                    && let Ok(body) = serde_json::from_str::<Value>(text.as_str())
                    && matches!(
                        body["type"].as_str(),
                        Some("response.interrupt" | "response.cancel")
                    )
                {
                    return Ok(Some(BudgetedWebSocketMessage {
                        message,
                        parsed_body: Some(Ok(body)),
                        permit: None,
                    }));
                }
                drop(message);
                self.reject_message(
                    "router_memory_busy",
                    "Codey 本地路由请求缓冲区已满，请稍后重试",
                    waiting,
                )
                .await?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        parse_websocket_message(message, permit).await.map(Some)
    }

    pub(crate) async fn next_message(&mut self) -> Result<Option<BudgetedWebSocketMessage>> {
        self.write_steering_notifications().await?;
        if let Some(body) = self.steering.take_continuation() {
            let message = WebSocketMessage::Text(serde_json::to_string(&body)?.into());
            if let Some(message) = self.admit_message(message, false).await? {
                return Ok(Some(message));
            }
            self.write_steering_notifications().await?;
        }
        let idle_deadline = tokio::time::Instant::now() + DOWNSTREAM_WEBSOCKET_IDLE_TIMEOUT;
        let mut idle_slot = None;
        loop {
            if self.upstream.as_ref().is_some_and(|cached| {
                !self
                    .websocket_backoffs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .route_is_current(&cached.route_id, &cached.config_identity)
            }) {
                self.upstream.take();
            }
            if !self.pending_messages.is_empty()
                && self.upstream.as_ref().is_none_or(|upstream| {
                    upstream.liveness.heartbeat_sent_at.is_none()
                        && upstream.liveness.maintenance_deadline() > Instant::now()
                })
            {
                return Ok(self.pending_messages.pop_front());
            }
            if idle_slot.is_none() {
                idle_slot = Some(IdleDownstreamRegistry::register(&self.idle_registry));
            }
            let evict = &mut idle_slot.as_mut().unwrap().1;
            let Some(upstream) = self.upstream.as_ref() else {
                let Some(message) = self.wait_for_idle_downstream(evict, idle_deadline).await?
                else {
                    return Ok(None);
                };
                if let Some(message) = self.admit_message(message, false).await? {
                    return Ok(Some(message));
                }
                continue;
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
                    _ = &mut *evict => IdleWebSocketEvent::Evicted,
                    _ = self.config_changes.changed() => IdleWebSocketEvent::ConfigurationChanged,
                    message = upstream_socket.next() => IdleWebSocketEvent::Upstream(message),
                    _ = &mut maintenance => IdleWebSocketEvent::MaintainUpstream,
                    message = downstream_socket.next(), if self.pending_messages.is_empty() => IdleWebSocketEvent::Downstream(message),
                }
            };

            match event {
                IdleWebSocketEvent::Evicted => {
                    self.upstream.take();
                    return Ok(None);
                }
                IdleWebSocketEvent::ConfigurationChanged => continue,
                IdleWebSocketEvent::Downstream(message) => {
                    let message = idle_downstream_message(message)?;
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
                    let Some(message) = message else {
                        return Ok(None);
                    };
                    if let Some(message) = self.admit_message(message, false).await? {
                        return Ok(Some(message));
                    }
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

    async fn wait_for_idle_downstream(
        &mut self,
        evict: &mut oneshot::Receiver<()>,
        idle_deadline: tokio::time::Instant,
    ) -> Result<Option<WebSocketMessage>> {
        // 合成 previous_response_id 只存在这条 socket 的历史里，空闲超时不能
        // 关掉它，否则 Codex 复用连接时会丢掉续接。连接数超出上限时另有回收。
        let stateful = self.adapted_history.last.is_some() || self.native_history.has_history();
        let outcome = {
            let read = self.socket.next();
            tokio::pin!(read);
            let idle = tokio::time::sleep_until(idle_deadline);
            tokio::pin!(idle);
            tokio::select! {
                biased;
                _ = &mut *evict => IdleDownstreamWait::Evicted,
                message = &mut read => IdleDownstreamWait::Message(message),
                _ = &mut idle, if !stateful => IdleDownstreamWait::TimedOut,
            }
        };
        match outcome {
            IdleDownstreamWait::Evicted => Ok(None),
            IdleDownstreamWait::Message(message) => idle_downstream_message(message),
            IdleDownstreamWait::TimedOut => {
                let _ = self.close(None).await;
                Ok(None)
            }
        }
    }

    pub(crate) async fn write_text(&mut self, text: impl Into<WebSocketText>) -> Result<()> {
        tokio::time::timeout(
            DOWNSTREAM_WRITE_TIMEOUT,
            self.socket.send(WebSocketMessage::Text(text.into())),
        )
        .await
        .context("写入 Codey Responses WebSocket 消息超时")
        .and_then(|result| result.context("写入 Codey Responses WebSocket 消息失败"))
        .context(DownstreamClosed)
    }

    pub(crate) async fn write_pong(
        &mut self,
        payload: tokio_tungstenite::tungstenite::Bytes,
    ) -> Result<()> {
        tokio::time::timeout(
            DOWNSTREAM_WRITE_TIMEOUT,
            self.socket.send(WebSocketMessage::Pong(payload)),
        )
        .await
        .context("写入 Codey Responses WebSocket Pong 超时")?
        .context("写入 Codey Responses WebSocket Pong 失败")
    }

    pub(crate) async fn proxy_upstream_websocket(
        &mut self,
        route: &RouteTarget,
        headers: &HeaderMap,
        body: &mut Value,
        discard_opaque_reasoning: bool,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<UpstreamWebSocketAttempt> {
        self.interrupt_forwarding = true;
        let result = self
            .proxy_upstream_websocket_impl(route, headers, body, discard_opaque_reasoning, probe)
            .await;
        self.interrupt_forwarding = false;
        result
    }

    async fn proxy_upstream_websocket_impl(
        &mut self,
        route: &RouteTarget,
        headers: &HeaderMap,
        body: &mut Value,
        discard_opaque_reasoning: bool,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<UpstreamWebSocketAttempt> {
        let auth_identity = UpstreamWebSocketAuthIdentity::from_headers(headers);
        if !route.supports_websockets {
            self.native_history
                .prepare_for_route(route, auth_identity, body);
            return Ok(UpstreamWebSocketAttempt::UseHttp("websocket_not_supported"));
        }
        let upstream_url = route
            .upstream_websocket_url
            .as_ref()
            .map_err(|error| anyhow::anyhow!(error.clone()))?;
        let now = Instant::now();
        let previous_response_id = responses_previous_response_id(body);
        let previous_response_key: Option<[u8; 32]> =
            previous_response_id.map(|id| Sha256::digest(id.as_bytes()).into());
        let cached_available = self.upstream.as_ref().is_some_and(|cached| {
            cached.route_id == route.provider_id
                && cached.url == *upstream_url
                && cached.config_identity == route.websocket_config
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
                .is_some_and(|cached| match previous_response_key {
                    Some(response_id) => cached.response_ids.contains(&response_id),
                    None => cached.auth_identity == auth_identity,
                });
        let effective_auth = self
            .upstream
            .as_ref()
            .filter(|_| cached_matches)
            .map_or(auth_identity, |cached| cached.auth_identity);
        // !cached_matches 时 effective_auth 就是请求头身份，prepare 与 restore 共用同一把钥匙。
        let history_key = self
            .native_history
            .prepare_for_route(route, effective_auth, body);
        let backoff_key =
            UpstreamWebSocketBackoffKey::for_route(route, upstream_url, effective_auth);
        // 同线路的其他会话可能已经报告故障；缓存连接也必须遵守共享退避。
        if self
            .websocket_backoffs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_backing_off(&backoff_key, Instant::now())
        {
            self.upstream.take();
            return Ok(UpstreamWebSocketAttempt::UseHttp("websocket_backoff"));
        }
        if route.official_account
            && self.native_history.requires_http_for_websocket(
                body,
                cached_matches && previous_response_key.is_some(),
            )
        {
            return Ok(UpstreamWebSocketAttempt::UseHttp(
                "websocket_history_requires_http",
            ));
        }
        if !cached_matches {
            // A response ID belongs to its original upstream socket. Reconnect
            // with full history only before sending this new request.
            if previous_response_key.is_some()
                && self.native_history.restore(history_key, body).is_err()
            {
                return Ok(UpstreamWebSocketAttempt::UseHttp(
                    "websocket_history_unavailable",
                ));
            }
            self.upstream.take();
        }
        normalize_native_responses_context(body, discard_opaque_reasoning);
        if route.official_account {
            sanitize_official_upstream_history(body);
        }
        let collaboration_alias = current_collaboration_alias();
        if let Some(alias) = &collaboration_alias {
            alias.alias_history(body);
        }
        if route.upstream_url.as_ref().is_ok_and(|url| {
            should_replay_reasoning_text(
                route.official_account,
                ProtocolBridge::NativeResponses,
                ResponsesRequestKind::Create,
                is_compaction_request(body, ResponsesRequestKind::Create),
                url,
            )
        }) {
            restore_reasoning_text_from_summary(body);
        }
        let mut upstream = if let Some(cached) = self.upstream.take() {
            cached
        } else {
            let Some(_probe_guard) =
                UpstreamWebSocketProbe::acquire(&self.websocket_backoffs, &backoff_key)
            else {
                return Ok(UpstreamWebSocketAttempt::UseHttp("websocket_backoff"));
            };
            match self
                .wait_for_upstream(connect_upstream_responses_websocket(
                    upstream_url,
                    headers,
                    probe,
                ))
                .await?
            {
                // 握手成功不代表请求已恢复；保留故障次数直到收到成功终态。
                Ok(socket) => CachedUpstreamWebSocket {
                    route_id: route.provider_id.clone(),
                    url: upstream_url.clone(),
                    auth_identity,
                    response_ids: VecDeque::new(),
                    config_identity: route.websocket_config,
                    liveness: UpstreamWebSocketLiveness::new(Instant::now()),
                    socket,
                },
                Err(error) => {
                    if upstream_websocket_endpoint_is_unsupported(&error) {
                        self.websocket_backoffs
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
                                "backoffSeconds": UPSTREAM_WEBSOCKET_UNSUPPORTED_TTL.as_secs(),
                                "requestId": current_router_request_id(),
                            }),
                        );
                        return Ok(UpstreamWebSocketAttempt::UseHttp(
                            "websocket_endpoint_unsupported",
                        ));
                    }
                    let (failure_count, backoff_duration) = self
                        .websocket_backoffs
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
                    return Ok(UpstreamWebSocketAttempt::UseHttp(
                        "websocket_connect_failed",
                    ));
                }
            }
        };

        let upstream_model = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        // 必须按实际发送的正文判断；重连时可能已经移除了 previous_response_id。
        let mut can_recover_persisted_context = responses_previous_response_id(body).is_some();
        let message_body = body
            .as_object_mut()
            .context("Responses WebSocket 上游请求必须是 JSON 对象")?;
        let http_stream = message_body.remove("stream");
        let http_background = message_body.remove("background");
        let http_type = message_body.insert(
            "type".to_string(),
            Value::String("response.create".to_string()),
        );
        let message =
            serde_json::to_string(body).context("序列化 Responses WebSocket 上游请求失败")?;

        // 发送后通常不能重放。唯一例外是首事件明确拒绝持久化续接且没有执行迹象，
        // 此时仍须验证完整历史，才能通过 HTTP 继续当前请求。
        if let Some(probe) = probe {
            probe.record_upstream_body(RequestBodySummary::from_responses_body(
                body,
                Some(message.len() as u64),
            ));
        }
        match self
            .wait_for_upstream(tokio::time::timeout(DOWNSTREAM_WRITE_TIMEOUT, async {
                if let Some(probe) = probe {
                    probe.begin_upstream_attempt(
                        UpstreamTransport::WebSocket,
                        crate::route_request_log::AttemptTrigger::Initial,
                    );
                }
                upstream
                    .socket
                    .send(WebSocketMessage::Text(message.into()))
                    .await
            }))
            .await?
        {
            Ok(Ok(())) => {
                if let Some(probe) = probe {
                    probe.mark_attempt_phase(crate::route_request_log::AttemptPhase::ResponseBody);
                }
            }
            Ok(Err(error)) => {
                if let Some(probe) = probe {
                    probe.finish_upstream_attempt(
                        crate::route_request_log::AttemptOutcome::Failed,
                        crate::route_request_log::AttemptPhase::Send,
                    );
                }
                record_upstream_websocket_failure(&self.websocket_backoffs, &backoff_key);
                return Err(error).context("发送 Responses WebSocket 上游请求失败");
            }
            Err(_) => {
                if let Some(probe) = probe {
                    probe.finish_upstream_attempt(
                        crate::route_request_log::AttemptOutcome::TimedOut,
                        crate::route_request_log::AttemptPhase::Send,
                    );
                }
                record_upstream_websocket_failure(&self.websocket_backoffs, &backoff_key);
                anyhow::bail!("发送 Responses WebSocket 上游请求超时");
            }
        }

        let mut produced_response_id = None;
        let response_deadline = tokio::time::Instant::now() + UPSTREAM_RESPONSE_TIMEOUT;
        let mut response_bytes = 0_usize;
        loop {
            if tokio::time::Instant::now() >= response_deadline {
                if let Some(probe) = probe {
                    probe.finish_upstream_attempt(
                        crate::route_request_log::AttemptOutcome::TimedOut,
                        crate::route_request_log::AttemptPhase::ResponseBody,
                    );
                }
                return Err(anyhow::Error::new(UpstreamResponseDeadline));
            }
            let next = loop {
                let waited = self
                    .wait_for_upstream(tokio::time::timeout_at(
                        std::cmp::min(
                            response_deadline,
                            tokio::time::Instant::now() + UPSTREAM_READ_IDLE_TIMEOUT,
                        ),
                        upstream.socket.next(),
                    ))
                    .await;
                let next = match waited {
                    Ok(Ok(next)) => next,
                    Ok(Err(_)) => {
                        if let Some(probe) = probe {
                            probe.finish_upstream_attempt(
                                crate::route_request_log::AttemptOutcome::TimedOut,
                                crate::route_request_log::AttemptPhase::ResponseBody,
                            );
                        }
                        // 等待上限取总期限与空闲期限的较小值；总期限耗尽不代表线路
                        // 不支持 WebSocket，和帧间检查一样不记退避。
                        if tokio::time::Instant::now() >= response_deadline {
                            return Err(anyhow::Error::new(UpstreamResponseDeadline));
                        }
                        record_upstream_websocket_failure(&self.websocket_backoffs, &backoff_key);
                        return Err(anyhow::Error::new(UpstreamReadIdleTimeout {
                            operation: "读取 Responses WebSocket 上游事件",
                        }));
                    }
                    Err(error)
                        if error
                            .downcast_ref::<DownstreamResponseInterrupt>()
                            .is_some() =>
                    {
                        let interrupt = self
                            .take_response_interrupt()
                            .context("Responses WebSocket 缺少待发送的中断事件")?;
                        let message = serde_json::to_string(
                            interrupt
                                .body()
                                .expect("admitted interrupt must contain JSON"),
                        )
                        .context("序列化 Responses WebSocket 中断事件失败")?;
                        can_recover_persisted_context = false;
                        match tokio::time::timeout(
                            DOWNSTREAM_WRITE_TIMEOUT,
                            upstream.socket.send(WebSocketMessage::Text(message.into())),
                        )
                        .await
                        {
                            Ok(Ok(())) => continue,
                            Ok(Err(error)) => {
                                record_upstream_websocket_failure(
                                    &self.websocket_backoffs,
                                    &backoff_key,
                                );
                                return Err(error).context("发送 Responses WebSocket 中断事件失败");
                            }
                            Err(_) => {
                                record_upstream_websocket_failure(
                                    &self.websocket_backoffs,
                                    &backoff_key,
                                );
                                anyhow::bail!("发送 Responses WebSocket 中断事件超时");
                            }
                        }
                    }
                    Err(error) => return Err(error),
                };
                break next;
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
                    response_bytes = response_bytes.saturating_add(text.len());
                    if response_bytes > MAX_UPSTREAM_RESPONSE_BYTES {
                        anyhow::bail!("上游响应累计大小超过 Codey 安全上限");
                    }
                    if !text.is_empty()
                        && let Some(probe) = probe
                    {
                        probe.mark_first_upstream_data(FirstByteSource::UpstreamWebSocketEvent);
                    }
                    upstream.liveness.record_activity(Instant::now());
                    let (single_event, events, raw_json_text) =
                        match serde_json::from_str::<Value>(text.as_str()) {
                            Ok(event) => (Some(event), Vec::new(), Some(text)),
                            Err(json_error) => (
                                None,
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
                    // SSE 包装可能在同一帧里携带多个事件，不能忽略后续执行迹象。
                    let only_event_in_frame = single_event.is_some() || events.len() == 1;
                    let mut raw_json_text = raw_json_text;
                    for mut event in single_event.into_iter().chain(events) {
                        let persisted_context_rejected =
                            responses_event_rejects_persisted_context(&event);
                        if can_recover_persisted_context
                            && only_event_in_frame
                            && persisted_context_rejected
                            && persisted_rejection_has_no_execution(&event)
                        {
                            // HTTP 使用当前请求头身份；刷新令牌及工具历史校验沿用既有规则。
                            // 只在明确拒绝后重新暂存，健康 WS 不增加历史复制。
                            let http_owner =
                                self.native_history
                                    .prepare_for_route(route, auth_identity, body);
                            if self.native_history.can_recover_rejected_continuation(
                                http_owner,
                                route.official_account || route.supports_remote_compaction,
                            ) {
                                self.native_history.mark_continuation_rejected();
                                let object =
                                    body.as_object_mut().expect("validated request object");
                                for (name, value) in [
                                    ("stream", http_stream),
                                    ("background", http_background),
                                    ("type", http_type),
                                ] {
                                    object.remove(name);
                                    if let Some(value) = value {
                                        object.insert(name.into(), value);
                                    }
                                }
                                if let Some(probe) = probe {
                                    probe.mark_upstream_error_summary(&redact_upstream_error_text(
                                        &event.to_string(),
                                        route,
                                    ));
                                    // 不 observe 拒绝终态，避免污染随后 HTTP 成功的最终状态。
                                    probe.finish_upstream_attempt(
                                        crate::route_request_log::AttemptOutcome::Failed,
                                        crate::route_request_log::AttemptPhase::ResponseBody,
                                    );
                                }
                                // upstream 随返回丢弃；上下文拒绝不影响其他会话的线路退避。
                                return Ok(UpstreamWebSocketAttempt::UseHttp(
                                    "websocket_persisted_context_rejected",
                                ));
                            }
                        }
                        can_recover_persisted_context = false;
                        if collaboration_alias
                            .as_ref()
                            .is_some_and(|alias| alias.apply(&mut event))
                        {
                            raw_json_text = None;
                        }
                        let infrastructure_failure =
                            responses_event_is_websocket_infrastructure_failure(&event);
                        if infrastructure_failure {
                            record_upstream_websocket_failure(
                                &self.websocket_backoffs,
                                &backoff_key,
                            );
                        }
                        if responses_event_is_failure(&event) {
                            if let Some(probe) = probe {
                                let original = raw_json_text
                                    .as_deref()
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| event.to_string());
                                probe.mark_upstream_error_summary(&redact_upstream_error_text(
                                    &original, route,
                                ));
                            }
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
                            normalize_response_failure(&mut event, None);
                        }
                        if let Some(probe) = probe {
                            probe.observe_event(&event);
                        }
                        let terminal = responses_event_is_terminal(&event);
                        if let Some(response_id) = responses_event_response_id(&event) {
                            produced_response_id =
                                Some(Sha256::digest(response_id.as_bytes()).into());
                        }
                        if self.event_needs_stream_id(&event) {
                            self.write_event(&event).await?;
                        } else if let Some(text) = raw_json_text.take() {
                            // Preserve an already-validated bare JSON frame on
                            // the latency-sensitive first-event path. SSE-wrapped
                            // WebSocket frames must be normalized to bare JSON
                            // before they are sent to Codex.
                            self.terminal_started |= terminal;
                            self.native_history.observe(&event);
                            let renumbered = self.steering.sequence_response_event(&mut event);
                            self.steering.observe(&event);
                            if renumbered {
                                self.write_text(serde_json::to_string(&event)?).await?;
                            } else {
                                self.write_text(text).await?;
                            }
                        } else {
                            self.write_event(&event).await?;
                        }
                        if responses_event_has_user_content(&event)
                            && let Some(probe) = probe
                        {
                            probe.mark_first_downstream_content();
                        }
                        if terminal {
                            if let Some(response_id) = produced_response_id {
                                if upstream.response_ids.len() >= MAX_CACHED_RESPONSE_IDS {
                                    upstream.response_ids.pop_front();
                                }
                                upstream.response_ids.push_back(response_id);
                            }
                            if !infrastructure_failure
                                && !persisted_context_rejected
                                && responses_websocket_connection_is_reusable(&event)
                            {
                                self.upstream = Some(upstream);
                            }
                            if !responses_event_is_failure(&event) {
                                self.websocket_backoffs
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .record_success(&backoff_key);
                            }
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

    pub(crate) fn event_needs_stream_id(&self, event: &Value) -> bool {
        self.stream_id.is_some()
            && event.get("stream_id").is_none()
            && responses_websocket_connection_is_reusable(event)
    }

    pub(crate) async fn close(
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

/// 复用连接补 `stream_id` 时 flatten 原对象再追加字段，避免为 delta 事件整树 clone。
/// 新键写在末尾，与 `Map::insert` 后再 `to_string` 的键序一致。
pub(crate) fn encode_responses_websocket_event(
    event: &Value,
    stream_id: Option<&str>,
) -> Result<String> {
    match stream_id {
        Some(stream_id) => {
            let object = event
                .as_object()
                .context("Responses WebSocket 事件必须是 JSON 对象")?;
            serde_json::to_string(&EventWithInsertedStreamId {
                event: object,
                stream_id,
            })
            .context("序列化 Responses WebSocket 事件失败")
        }
        None => serde_json::to_string(event).context("序列化 Responses WebSocket 事件失败"),
    }
}

#[derive(serde::Serialize)]
struct EventWithInsertedStreamId<'a> {
    #[serde(flatten)]
    event: &'a serde_json::Map<String, Value>,
    stream_id: &'a str,
}

pub(crate) fn responses_event_is_terminal(event: &Value) -> bool {
    matches!(
        event.get("type").and_then(Value::as_str),
        Some("response.completed" | "response.failed" | "response.incomplete" | "error")
    )
}

pub(crate) fn responses_event_type_has_user_content(event_type: &str) -> bool {
    matches!(
        event_type,
        "response.output_text.delta"
            | "response.refusal.delta"
            | "response.function_call_arguments.delta"
            | "response.custom_tool_call_input.delta"
            | "response.reasoning_summary_text.delta"
    )
}

pub(crate) fn responses_event_has_user_content(event: &Value) -> bool {
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

pub(crate) fn responses_event_is_failure(event: &Value) -> bool {
    matches!(
        event.get("type").and_then(Value::as_str),
        Some("response.failed" | "error")
    )
}

pub(crate) fn responses_event_is_websocket_infrastructure_failure(event: &Value) -> bool {
    responses_event_is_failure(event)
        && ["/error/code", "/response/error/code"]
            .into_iter()
            .any(|path| {
                event.pointer(path).and_then(Value::as_str) == Some("rebuild_target_not_ready")
            })
}

fn responses_event_rejects_persisted_context(event: &Value) -> bool {
    let error = match event["type"].as_str() {
        Some("error") => event.get("error"),
        Some("response.failed") => event.pointer("/response/error"),
        _ => None,
    };
    error.is_some_and(|error| {
        error["type"] == "invalid_request_error"
            && error["code"] == "unsupported_persisted_item_context"
    })
}

fn persisted_rejection_has_no_execution(event: &Value) -> bool {
    // id、输出、用量或工具字段都可能表明响应已开始，即使拒绝是首个收到的事件。
    [
        "/response_id",
        "/response/id",
        "/item",
        "/delta",
        "/usage",
        "/response/usage",
    ]
    .into_iter()
    .all(|path| event.pointer(path).is_none_or(Value::is_null))
        && ["/output", "/response/output"].into_iter().all(|path| {
            event.pointer(path).is_none_or(|output| {
                output.is_null() || output.as_array().is_some_and(Vec::is_empty)
            })
        })
}

pub(crate) fn parse_responses_websocket_sse_events(text: &str) -> Result<Vec<Value>> {
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

pub(crate) fn append_responses_websocket_sse_event(
    events: &mut Vec<Value>,
    frame: &[u8],
) -> Result<()> {
    let Some(data) = sse_frame_data(frame)? else {
        return Ok(());
    };
    if data.trim() == "[DONE]" {
        return Ok(());
    }
    events.push(
        serde_json::from_str::<Value>(&data).context("Responses 上游 SSE data 不是有效 JSON")?,
    );
    Ok(())
}

pub(crate) fn responses_previous_response_id(body: &Value) -> Option<&str> {
    body.get("previous_response_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|response_id| !response_id.is_empty())
}

pub(crate) fn responses_event_response_id(event: &Value) -> Option<&str> {
    event
        .get("response")
        .and_then(|response| response.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|response_id| !response_id.is_empty())
}

pub(crate) fn responses_websocket_connection_is_reusable(event: &Value) -> bool {
    event
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        != Some("websocket_connection_limit_reached")
}

pub(crate) fn upstream_websocket_request(
    url: &str,
    headers: &HeaderMap,
) -> Result<WebSocketRequest> {
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

pub(crate) async fn connect_upstream_responses_websocket(
    url: &str,
    headers: &HeaderMap,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
    let request = upstream_websocket_request(url, headers)?;
    if let Some(probe) = probe {
        probe.set_upstream_request_headers(&super::responses::format_upstream_headers(
            request.headers(),
        ));
    }
    let config = WebSocketConfig::default()
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_REQUEST_BYTES)
        .max_message_size(Some(MAX_UPSTREAM_RESPONSE_BYTES))
        .max_frame_size(Some(MAX_UPSTREAM_RESPONSE_BYTES));
    let (socket, response) = tokio::time::timeout(
        UPSTREAM_WEBSOCKET_CONNECT_TIMEOUT,
        // Responses events are small and latency-sensitive. Disable Nagle on
        // the underlying upstream TCP socket before the TLS/WS handshake.
        tokio_tungstenite::connect_async_tls_with_config(
            request,
            Some(config),
            true,
            Some(super::websocket_tls::connector()),
        ),
    )
    .await
    .context("连接 Responses WebSocket 上游超时")?
    .context("连接 Responses WebSocket 上游失败")?;
    if let Some(probe) = probe {
        probe.set_upstream_response_headers(&super::responses::format_upstream_response_headers(
            response.headers(),
        ));
    }
    Ok(socket)
}

pub(crate) fn upstream_websocket_endpoint_is_unsupported(error: &anyhow::Error) -> bool {
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

impl WebSocketResponsesDownstream {
    async fn wait_response_future<T, F>(&mut self, future: F, admission: bool) -> Result<T>
    where
        T: Send,
        F: std::future::Future<Output = T> + Send,
    {
        tokio::pin!(future);
        let mut control_messages = 0_u8;
        loop {
            tokio::select! {
                biased;
                // Keep the same pinned deadline when Ping or queued requests arrive.
                message = self.socket.next(), if control_messages < 16 => {
                    control_messages += 1;
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
                            let Some(message) = self.admit_message(message, true).await? else {
                                continue;
                            };
                            if self.handle_steering_message(&message).await? {
                                continue;
                            }
                            if message.is_interrupt() || (admission && message.body().is_some_and(|body| body["type"] == "response.cancel")) {
                                if admission {
                                    if message.body().and_then(|body| body.get("stream_id")).and_then(Value::as_str)
                                        .is_some_and(|id| Some(id) != self.stream_id.as_deref()) {
                                        continue;
                                    }
                                    return Err(DownstreamResponseInterrupt.into());
                                }
                                if self.interrupt_forwarding {
                                    self.pending_interrupt = Some(message);
                                    return Err(DownstreamResponseInterrupt.into());
                                }
                                continue;
                            }
                            if self.pending_messages.len() >= MAX_PENDING_WEBSOCKET_MESSAGES {
                                drop(message);
                                self.reject_message(
                                    "router_busy",
                                    "Codey 本地路由当前请求过多，请稍后重试",
                                    true,
                                ).await?;
                                continue;
                            }
                            self.pending_messages.push_back(message);
                        }
                    }
                }
                result = &mut future => return Ok(result),
                _ = tokio::task::yield_now(), if control_messages >= 16 => {
                    control_messages = 0;
                }
            }
        }
    }
}

#[async_trait]
impl ResponsesDownstream for WebSocketResponsesDownstream {
    async fn wait_for_admission<T, F>(&mut self, future: F) -> Result<T>
    where
        T: Send,
        F: std::future::Future<Output = T> + Send,
    {
        self.wait_response_future(future, true).await
    }
    async fn wait_for_upstream<T, F>(&mut self, future: F) -> Result<T>
    where
        T: Send,
        F: std::future::Future<Output = T> + Send,
    {
        self.wait_response_future(future, false).await
    }
    fn is_websocket(&self) -> bool {
        true
    }

    fn select_route(&mut self, route: &RouteTarget) {
        if self.upstream.as_ref().is_some_and(|cached| {
            cached.route_id != route.provider_id
                || cached.config_identity != route.websocket_config
                || !route.supports_websockets
        }) {
            self.upstream.take();
        }
    }

    fn prepare_adapted_response_context(&mut self, body: &mut Value) -> Result<bool> {
        self.adapted_history.prepare(body)
    }

    fn prepare_native_http_fallback(
        &mut self,
        route: &RouteTarget,
        headers: &HeaderMap,
        body: &mut Value,
    ) -> Result<bool> {
        let key = self.native_history.prepare_for_route(
            route,
            UpstreamWebSocketAuthIdentity::from_headers(headers),
            body,
        );
        // The upstream WebSocket attempt normally stages continuation history.
        // Compaction and an active lifecycle plugin both skip that attempt, so
        // stage before every HTTP restore. A second call after a failed
        // handshake only restages the same turn.
        let restored = self.native_history.restore_for_http(
            key,
            body,
            route.official_account || route.supports_remote_compaction,
        );
        if restored.is_ok() {
            self.upstream.take();
        }
        restored
    }

    fn remember_adapted_response(&mut self, response_id: &str, output: &[Value]) -> Result<()> {
        self.adapted_history.remember(response_id, output)
    }

    async fn write_error(
        &mut self,
        status: u16,
        code: &str,
        message: String,
        route: Option<&RouteTarget>,
    ) -> Result<()> {
        self.write_response_failure(&ResponsesFailure::new(status, code, message, route))
            .await
    }

    async fn write_response_failure(&mut self, failure: &ResponsesFailure) -> Result<()> {
        self.write_event(&failure.normalized_event()).await
    }

    async fn write_json(&mut self, status: u16, value: &Value) -> Result<()> {
        if !(200..300).contains(&status) {
            return self
                .write_response_failure(&ResponsesFailure::from_json(status, value))
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
        let mut event = normalized_response_event(event);
        if self.steering.controls_sent {
            self.steering.sequence_response_event(event.to_mut());
        }
        if responses_event_is_terminal(&event) {
            if self.terminal_started {
                return Ok(());
            }
            self.terminal_started = true;
        }
        self.native_history.observe(&event);
        self.steering.observe(&event);
        let encoded = encode_responses_websocket_event(
            &event,
            self.event_needs_stream_id(&event).then(|| {
                self.stream_id
                    .as_deref()
                    .expect("stream id must exist when insertion is required")
            }),
        )?;
        self.write_text(encoded).await
    }

    async fn finish_event_stream(&mut self) -> Result<()> {
        Ok(())
    }

    async fn proxy_response_with_probe(
        &mut self,
        response: reqwest::Response,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<()> {
        proxy_native_response_to_websocket(self, response, probe).await
    }

    async fn try_proxy_upstream_websocket_with_probe(
        &mut self,
        route: &RouteTarget,
        headers: &HeaderMap,
        body: &mut Value,
        discard_opaque_reasoning: bool,
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<UpstreamWebSocketAttempt> {
        self.proxy_upstream_websocket(route, headers, body, discard_opaque_reasoning, probe)
            .await
    }
}

/// HTTP 回退的每个事件都经过同一套归一化、修正、观测和写回；返回是否为终态事件。
async fn forward_fallback_event(
    downstream: &mut WebSocketResponsesDownstream,
    mut event: Value,
    retry_advice: Option<&ResponseRetryAdvice>,
    response_fix: Option<&NativeResponseFix>,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<bool> {
    normalize_response_failure(&mut event, retry_advice);
    if let Some(fix) = response_fix {
        fix.apply(&mut event);
    }
    if let Some(probe) = probe {
        probe.observe_event(&event);
    }
    downstream.write_event(&event).await?;
    if responses_event_has_user_content(&event)
        && let Some(probe) = probe
    {
        probe.mark_first_downstream_content();
    }
    Ok(responses_event_is_terminal(&event))
}

pub(crate) async fn proxy_native_response_to_websocket(
    downstream: &mut WebSocketResponsesDownstream,
    response: reqwest::Response,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<()> {
    let status = response.status().as_u16();
    let retry_advice = ResponseRetryAdvice::from_headers(response.headers());
    let upstream_request_id = upstream_request_id_from_headers(response.headers());
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.chars().take(128).collect::<String>());
    let response_fix = current_native_response_fix();
    let mut prepared = await_upstream(
        downstream,
        prepare_upstream_response(response, "读取 Responses HTTP 上游响应失败", probe),
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
                "读取 Responses HTTP 上游 SSE 流失败",
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
                    .context("Responses HTTP 上游 SSE data 不是有效 JSON")?;
                terminal |= forward_fallback_event(
                    downstream,
                    event,
                    retry_advice.as_ref(),
                    response_fix.as_ref(),
                    probe,
                )
                .await?;
                if terminal {
                    break;
                }
            }
            if done || terminal {
                break;
            }
        }
        if !done
            && !terminal
            && !buffer[cursor.consumed..]
                .iter()
                .all(u8::is_ascii_whitespace)
            && let Some(data) = sse_frame_data(&buffer[cursor.consumed..])?
            && data.trim() != "[DONE]"
        {
            let event = serde_json::from_str::<Value>(&data)
                .context("Responses HTTP 上游 SSE 末尾 data 不是有效 JSON")?;
            terminal |= forward_fallback_event(
                downstream,
                event,
                retry_advice.as_ref(),
                response_fix.as_ref(),
                probe,
            )
            .await?;
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
            &mut prepared,
            limit,
            "读取 Responses HTTP 上游响应失败",
            probe,
        ),
    )
    .await??;
    if (200..300).contains(&status)
        && let Ok(text) = std::str::from_utf8(&body)
        && responses_body_looks_like_sse(text)
    {
        let events = parse_responses_websocket_sse_events(text)
            .context("解析 Responses HTTP 上游未标记的 SSE 响应失败")?;
        if !events.iter().any(responses_event_is_terminal) {
            anyhow::bail!("Responses HTTP/SSE 降级响应缺少终态事件");
        }
        downstream.start_event_stream().await?;
        for event in events {
            forward_fallback_event(
                downstream,
                event,
                retry_advice.as_ref(),
                response_fix.as_ref(),
                probe,
            )
            .await?;
        }
        return downstream.finish_event_stream().await;
    }
    match serde_json::from_slice::<Value>(&body) {
        Ok(mut value) => {
            if let Some(fix) = response_fix.as_ref() {
                fix.apply(&mut value);
            }
            if let Some(probe) = probe {
                probe.observe_response(status, &value);
            }
            if !(200..300).contains(&status) {
                let mut failure = ResponsesFailure::from_json(status, &value);
                failure.retry_advice = retry_advice;
                if let Some(request_id) = upstream_request_id {
                    failure.event["response"]["error"]["codey"]["upstreamRequestId"] =
                        request_id.into();
                }
                return write_probed_websocket_failure(downstream, &failure, probe).await;
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
            let message = format!(
                "Responses HTTP 上游返回无法解析的响应（HTTP {status}，Content-Type: {}），需要 JSON 或 SSE；请检查线路地址和上游协议。解析错误：{error}",
                content_type.as_deref().unwrap_or("未提供")
            );
            let failure = ResponsesFailure::new(
                if (200..300).contains(&status) {
                    502
                } else {
                    status
                },
                "upstream_protocol_error",
                message,
                None,
            );
            write_probed_websocket_failure(downstream, &failure, probe).await
        }
    }
}

async fn write_probed_websocket_failure(
    downstream: &mut WebSocketResponsesDownstream,
    failure: &ResponsesFailure,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<()> {
    // 内部下游直接写出错误时，外层观察器看不到 write_error 调用。
    if let Some(probe) = probe {
        probe.mark_error(failure.status, &failure.code);
        probe.observe_event(&failure.normalized_event());
    }
    observe_lifecycle_response_failure(&failure.normalized_event());
    downstream.write_response_failure(failure).await
}

pub(crate) fn responses_body_looks_like_sse(text: &str) -> bool {
    text.lines()
        .any(|line| line.trim_end_matches('\r').starts_with("data:"))
}

pub(crate) async fn write_error_response(
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

pub(crate) async fn write_text_error_response<W>(
    stream: &mut W,
    status: u16,
    code: &str,
    message: impl AsRef<str>,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    write_text_error_response_with_retry_after(stream, status, code, message, None).await
}

pub(crate) async fn write_text_error_response_with_retry_after<W>(
    stream: &mut W,
    status: u16,
    code: &str,
    message: impl AsRef<str>,
    retry_after: Option<&str>,
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
    let retry_header = retry_after
        .filter(|value| HeaderValue::from_str(value).is_ok())
        .map(|value| format!("retry-after: {value}\r\n"))
        .unwrap_or_default();
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\n{request_id_header}{retry_header}connection: close\r\n\r\n",
        body.len()
    );
    write_all_with_timeout(stream, header.as_bytes(), "写入本地路由错误响应头失败").await?;
    write_all_with_timeout(stream, body.as_bytes(), "写入本地路由错误响应失败").await?;
    Ok(())
}

pub(crate) async fn write_json_response(
    stream: &mut TcpStream,
    status: u16,
    value: &Value,
) -> Result<()> {
    write_json_response_with_retry_after(stream, status, value, None).await
}

pub(crate) async fn write_json_response_with_retry_after(
    stream: &mut TcpStream,
    status: u16,
    value: &Value,
    retry_after: Option<&str>,
) -> Result<()> {
    let mut body = serde_json::to_vec(value).context("序列化 Codey 本地路由响应失败")?;
    body.push(b'\n');
    let reason = reason_phrase(status);
    let request_id_header = router_request_id_header();
    let retry_header = retry_after
        .filter(|value| HeaderValue::from_str(value).is_ok())
        .map(|value| format!("retry-after: {value}\r\n"))
        .unwrap_or_default();
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n{request_id_header}{retry_header}connection: close\r\n\r\n",
        body.len()
    );
    write_all_with_timeout(stream, header.as_bytes(), "写入本地路由 JSON 响应头失败").await?;
    write_all_with_timeout(stream, &body, "写入本地路由 JSON 响应失败").await?;
    Ok(())
}

pub(crate) async fn write_static_response(
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

#[cfg(test)]
mod http_fallback_history_tests {
    use super::super::tests::{local_websocket_pair, router_config};
    use super::*;

    #[tokio::test]
    async fn websocket_diagnostics_count_response_create_but_not_the_handshake() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let WebSocketMessage::Text(request) = socket.next().await.unwrap().unwrap() else {
                panic!("response.create expected");
            };
            let request: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["type"], "response.create");
            socket.send(WebSocketMessage::Text(json!({"type":"response.completed","response":{"id":"response-test","status":"completed","output":[]}}).to_string().into())).await.unwrap();
        });
        let (mut config, provider, model) = router_config(format!("http://{address}/v1"));
        config.profiles[0].supports_websockets = true;
        let snapshot = RouterSnapshot::from_config(&config);
        let (socket, _peer) = local_websocket_pair().await;
        let mut downstream = WebSocketResponsesDownstream::new(socket);
        let probe = RouteRequestLogProbe::detached_test_probe();
        assert_eq!(
            downstream
                .proxy_upstream_websocket(
                    &snapshot.routes[&provider],
                    &HeaderMap::new(),
                    &mut json!({"model":model,"input":"test"}),
                    false,
                    Some(&probe)
                )
                .await
                .unwrap(),
            UpstreamWebSocketAttempt::Completed
        );
        upstream.await.unwrap();
        let attempts = probe.attempts_for_test();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].transport, UpstreamTransport::WebSocket);
        assert_eq!(
            attempts[0].status_code, None,
            "101 握手状态不能作为模型响应状态"
        );
        assert_eq!(
            attempts[0].outcome,
            crate::route_request_log::AttemptOutcome::Succeeded
        );
    }

    #[tokio::test]
    async fn persisted_context_rejection_after_forwarded_interrupt_is_not_replayed() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (second_sent, second_received) = oneshot::channel();
        let upstream = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket.next().await.unwrap().unwrap();
            socket
                .send(WebSocketMessage::Text(
                    json!({"type":"response.completed",
                        "response":{"id":"resp-original","status":"completed","output":[]}})
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            socket.next().await.unwrap().unwrap();
            second_sent.send(()).unwrap();
            let WebSocketMessage::Text(interrupt) = socket.next().await.unwrap().unwrap() else {
                panic!("expected forwarded interrupt");
            };
            assert_eq!(
                serde_json::from_str::<Value>(&interrupt).unwrap()["type"],
                "response.interrupt"
            );
            socket
                .send(WebSocketMessage::Text(
                    json!({"type":"error","error":{"type":"invalid_request_error",
                        "code":"unsupported_persisted_item_context","message":"Cannot replay"}})
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        });
        let (mut config, provider, model) = router_config(format!("http://{address}/v1"));
        config.profiles[0].supports_websockets = true;
        let snapshot = RouterSnapshot::from_config(&config);
        let route = &snapshot.routes[&provider];
        let (socket, mut peer) = local_websocket_pair().await;
        let mut downstream = WebSocketResponsesDownstream::new(socket);
        downstream
            .proxy_upstream_websocket(
                route,
                &HeaderMap::new(),
                &mut json!({"model":model,"input":"task"}),
                false,
                None,
            )
            .await
            .unwrap();
        peer.next().await.unwrap().unwrap();
        downstream.clear_stream_id();
        let client = tokio::spawn(async move {
            second_received.await.unwrap();
            peer.send(WebSocketMessage::Text(
                json!({"type":"response.interrupt","response_id":"resp-original"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let WebSocketMessage::Text(event) = peer.next().await.unwrap().unwrap() else {
                panic!("expected rejection");
            };
            let event: Value = serde_json::from_str(&event).unwrap();
            assert_eq!(
                event["response"]["error"]["codey"]["originalCode"],
                "unsupported_persisted_item_context"
            );
        });
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            downstream.proxy_upstream_websocket(
                route,
                &HeaderMap::new(),
                &mut json!({"model":model,"previous_response_id":"resp-original","input":"next"}),
                false,
                None,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result, UpstreamWebSocketAttempt::Completed);
        assert!(downstream.upstream.is_none());
        upstream.await.unwrap();
        client.await.unwrap();
    }

    #[test]
    fn persisted_context_recovery_rejects_execution_in_the_error_event() {
        for evidence in [
            json!({"response_id":"resp-new"}),
            json!({"response":{"id":"resp-new"}}),
            json!({"response":{"output":[{"type":"function_call"}]}}),
            json!({"usage":{"output_tokens":1}}),
            json!({"item":{"type":"function_call"}}),
            json!({"delta":"started"}),
        ] {
            assert!(!persisted_rejection_has_no_execution(&evidence));
        }
        assert!(persisted_rejection_has_no_execution(
            &json!({"response":{"output":[]}})
        ));
    }

    #[tokio::test]
    async fn declared_search_tools_preserve_websocket_selection() {
        let (config, provider_id, model) = router_config("http://127.0.0.1:9/v1".into());
        let snapshot = RouterSnapshot::from_config(&config);
        let mut route = snapshot.routes[&provider_id].as_ref().clone();
        route.supports_websockets = true;
        route.upstream_websocket_url = Err("websocket selected".into());
        let (socket, _peer) = local_websocket_pair().await;
        let mut downstream = WebSocketResponsesDownstream::new(socket);
        for (official_account, tools) in [
            (true, json!([])),
            (true, json!([{"type":"function","name":"web_search"}])),
            (true, json!([{"type":"tool_search"}])),
            (true, json!([{"type":"web_search"}])),
            (true, json!([{"type":"web_search_preview"}])),
            (true, json!([{"type":"web_search_2025_08_26"}])),
            (true, json!([{"type":"web_search_preview_2025_03_11"}])),
            (false, json!([{"type":"web_search"}])),
            (false, json!([{"type":"web_search_preview"}])),
        ] {
            route.official_account = official_account;
            let mut body = json!({"model":model,"input":"hello","tools":tools});
            let error = downstream
                .proxy_upstream_websocket(&route, &HeaderMap::new(), &mut body, false, None)
                .await
                .unwrap_err();
            assert_eq!(error.to_string(), "websocket selected");
        }
    }

    #[tokio::test]
    async fn http_fallback_stages_history_when_the_websocket_attempt_is_skipped() {
        let (config, provider_id, model) = router_config("http://127.0.0.1:9/v1".into());
        let snapshot = RouterSnapshot::from_config(&config);
        let mut route = snapshot.routes[&provider_id].as_ref().clone();
        route.official_account = true;
        route.supports_websockets = false;
        let route = &route;
        let (socket, mut peer) = local_websocket_pair().await;
        let mut downstream = WebSocketResponsesDownstream::new(socket);
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("session-id"),
            HeaderValue::from_static("lifecycle-session"),
        );
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer old-token"));
        headers.insert(
            HeaderName::from_static(CHATGPT_ACCOUNT_ID_HEADER),
            HeaderValue::from_static("same-account"),
        );
        downstream.native_history = NativeResponsesHistory::with_cache(
            Arc::new(Mutex::new(NativeHistoryCache::default())),
            &[("session-id".into(), "lifecycle-session".into())],
        );

        let mut first = json!({"model": model, "stream": true, "input": "original task"});
        assert!(
            !downstream
                .prepare_native_http_fallback(route, &headers, &mut first)
                .unwrap()
        );
        downstream
            .write_event(&json!({
                "type": "response.completed",
                "response": {
                    "id": "resp-first",
                    "status": "completed",
                    "output": []
                }
            }))
            .await
            .unwrap();
        let WebSocketMessage::Text(_) = peer.next().await.unwrap().unwrap() else {
            panic!("expected the completed event");
        };

        downstream.clear_stream_id();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer refreshed-token"),
        );
        let mut second = json!({
            "model": model,
            "stream": true,
            "previous_response_id": "resp-first",
            "input": "follow up",
            "tools": [{"type":"web_search"}]
        });
        assert_eq!(
            downstream
                .proxy_upstream_websocket(route, &headers, &mut second, false, None)
                .await
                .unwrap(),
            UpstreamWebSocketAttempt::UseHttp("websocket_not_supported")
        );
        assert!(
            downstream
                .prepare_native_http_fallback(route, &headers, &mut second)
                .unwrap()
        );
        assert!(second.get("previous_response_id").is_none());
        assert_eq!(second["tools"], json!([{"type":"web_search"}]));
        assert_eq!(
            second["input"],
            json!([
                {"role":"user","content":"original task"},
                {"role":"user","content":"follow up"}
            ])
        );

        let mut unknown = json!({
            "model": model,
            "previous_response_id": "resp-missing",
            "input": "another"
        });
        let error = downstream
            .prepare_native_http_fallback(route, &headers, &mut unknown)
            .unwrap_err();
        assert!(error.to_string().contains("会话历史已失效"), "{error}");
        assert_eq!(unknown["previous_response_id"], "resp-missing");
    }
}

#[cfg(test)]
mod encode_stream_id_tests {
    use super::*;

    #[test]
    fn inserted_stream_id_matches_map_insert_key_order() {
        let event = json!({
            "delta": "x",
            "type": "response.output_text.delta",
        });
        let encoded = encode_responses_websocket_event(&event, Some("main")).unwrap();
        let mut cloned = event.clone();
        cloned
            .as_object_mut()
            .unwrap()
            .insert("stream_id".into(), Value::String("main".into()));
        assert_eq!(encoded, serde_json::to_string(&cloned).unwrap());
        assert!(encoded.contains("\"stream_id\":\"main\""));
    }

    #[test]
    fn missing_stream_id_serializes_the_original_event() {
        let event = json!({"type": "response.created"});
        assert_eq!(
            encode_responses_websocket_event(&event, None).unwrap(),
            serde_json::to_string(&event).unwrap()
        );
    }
}

#[cfg(test)]
mod admission_tests {
    use super::super::tests::local_websocket_pair;
    use super::*;

    #[tokio::test]
    async fn exhausted_websocket_budget_rejects_small_messages_without_ending_response() {
        let (socket, mut peer) = local_websocket_pair().await;
        let mut downstream = WebSocketResponsesDownstream::new(socket);
        downstream.request_body_budget = Arc::new(Semaphore::new(0));
        downstream
            .socket
            .set_budget(Arc::clone(&downstream.request_body_budget));
        let (release, released) = oneshot::channel::<()>();
        let waiting = tokio::spawn(async move {
            downstream
                .wait_for_upstream(released)
                .await
                .unwrap()
                .unwrap();
            downstream
        });
        peer.send(WebSocketMessage::Text(
            json!({"type":"response.create","input":"x".repeat(1024)})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(2), peer.next())
            .await
            .expect("an exhausted budget must reject the frame promptly")
            .unwrap()
            .unwrap();
        let WebSocketMessage::Text(event) = event else {
            panic!("expected failure event")
        };
        let event: Value = serde_json::from_str(&event).unwrap();
        assert_eq!(event["type"], "response.failed");
        assert_eq!(event["response"]["error"]["code"], "router_memory_busy");
        release.send(()).unwrap();
        let downstream = waiting.await.unwrap();
        assert!(downstream.pending_messages.is_empty());
        assert!(!downstream.terminal_started);
    }

    async fn receive_failure(peer: &mut WebSocketStream<MaybeTlsStream<TcpStream>>) -> Value {
        let message = tokio::time::timeout(Duration::from_secs(2), peer.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let WebSocketMessage::Text(text) = message else {
            panic!("expected failure event")
        };
        serde_json::from_str(&text).unwrap()
    }

    #[tokio::test]
    async fn queued_websocket_budget_is_shared_and_survives_message_handoff() {
        let budget = Arc::new(Semaphore::new(1));
        let mut downstreams = Vec::new();
        for connection in 0..3 {
            let (socket, mut peer) = local_websocket_pair().await;
            let mut downstream = WebSocketResponsesDownstream::new(socket);
            downstream.request_body_budget = Arc::clone(&budget);
            downstream.socket.set_budget(Arc::clone(&budget));
            let (release, released) = oneshot::channel::<()>();
            let waiting = tokio::spawn(async move {
                downstream
                    .wait_for_upstream(released)
                    .await
                    .unwrap()
                    .unwrap();
                downstream
            });
            peer.feed(WebSocketMessage::Text(
                json!({"type":"response.create","input":"queued"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            peer.feed(WebSocketMessage::Ping(Default::default()))
                .await
                .unwrap();
            peer.flush().await.unwrap();
            if connection > 0 {
                assert_eq!(
                    receive_failure(&mut peer).await["response"]["error"]["code"],
                    "router_memory_busy"
                );
            }
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(2), peer.next())
                    .await
                    .unwrap(),
                Some(Ok(WebSocketMessage::Pong(_)))
            ));
            release.send(()).unwrap();
            let downstream = waiting.await.unwrap();
            assert_eq!(
                downstream.pending_messages.len(),
                usize::from(connection == 0)
            );
            assert_eq!(budget.available_permits(), 0);
            downstreams.push((downstream, peer));
        }
        let message = downstreams[0].0.next_message().await.unwrap().unwrap();
        assert_eq!(message.body().unwrap()["input"], "queued");
        assert!(downstreams[0].0.pending_messages.is_empty());
        assert_eq!(budget.available_permits(), 0);
        drop(message);
        assert_eq!(budget.available_permits(), 1);
    }

    #[tokio::test]
    async fn full_websocket_queue_and_exhausted_budget_still_accept_interrupts() {
        for permits in [
            MAX_PENDING_WEBSOCKET_MESSAGES,
            MAX_PENDING_WEBSOCKET_MESSAGES * 2,
        ] {
            let (socket, mut peer) = local_websocket_pair().await;
            let budget = Arc::new(Semaphore::new(permits));
            let mut downstream = WebSocketResponsesDownstream::new(socket);
            downstream.request_body_budget = Arc::clone(&budget);
            downstream.socket.set_budget(Arc::clone(&budget));
            downstream.interrupt_forwarding = true;
            for _ in 0..=MAX_PENDING_WEBSOCKET_MESSAGES {
                peer.feed(WebSocketMessage::Text(
                    json!({"type":"response.create","input":"queued"})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            }
            peer.feed(WebSocketMessage::Text(
                json!({"type":"response.interrupt","response_id":"active","mode":"discard_partial_items"})
                    .to_string().into(),
            )).await.unwrap();
            peer.flush().await.unwrap();
            let error = tokio::time::timeout(
                Duration::from_secs(2),
                downstream.wait_for_upstream(std::future::pending::<()>()),
            )
            .await
            .expect("interrupt must bypass a full queue")
            .unwrap_err();
            assert!(
                error
                    .downcast_ref::<DownstreamResponseInterrupt>()
                    .is_some()
            );
            assert_eq!(
                downstream.pending_messages.len(),
                MAX_PENDING_WEBSOCKET_MESSAGES
            );
            assert_eq!(
                receive_failure(&mut peer).await["response"]["error"]["code"],
                if permits == MAX_PENDING_WEBSOCKET_MESSAGES {
                    "router_memory_busy"
                } else {
                    "router_busy"
                }
            );
            let interrupt = downstream.take_response_interrupt().unwrap();
            assert_eq!(interrupt.body().unwrap()["response_id"], "active");
            assert_eq!(
                interrupt.permit.is_none(),
                permits == MAX_PENDING_WEBSOCKET_MESSAGES
            );
            assert!(!downstream.terminal_started);
            drop((interrupt, downstream));
            assert_eq!(budget.available_permits(), permits);
        }
    }

    #[tokio::test]
    async fn oversized_emergency_interrupt_stops_at_header_when_budget_is_exhausted() {
        let (socket, mut peer) = local_websocket_pair().await;
        let mut downstream = WebSocketResponsesDownstream::new(socket);
        downstream.request_body_budget = Arc::new(Semaphore::new(0));
        downstream
            .socket
            .set_budget(Arc::clone(&downstream.request_body_budget));
        downstream.interrupt_forwarding = true;
        peer.send(WebSocketMessage::Text(
            json!({"type":"response.interrupt","padding":"x".repeat(MAX_EMERGENCY_INTERRUPT_BYTES)})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            downstream.wait_for_upstream(std::future::pending::<()>()),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.downcast_ref::<DownstreamClosed>().is_some());
        assert!(error.chain().any(|cause| matches!(
            cause.downcast_ref::<WebSocketError>(),
            Some(WebSocketError::Io(error)) if error.kind() == std::io::ErrorKind::OutOfMemory
        )));
        assert!(downstream.take_response_interrupt().is_none());
        assert!(downstream.pending_messages.is_empty());
    }

    #[test]
    fn cancelled_large_websocket_parse_holds_budget_until_blocking_work_finishes() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let (started, started_rx) = oneshot::channel();
            let (release, release_rx) = oneshot::channel();
            let occupied = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                release_rx.blocking_recv().unwrap();
            });
            started_rx.await.unwrap();
            let budget = Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS));
            let message = WebSocketMessage::Text(
                json!({"type":"response.create",
                "input":"x".repeat(REQUEST_JSON_OFFLOAD_BYTES)})
                .to_string()
                .into(),
            );
            let permit = acquire_request_body_budget(&budget, message.len()).unwrap();
            let held = permit.as_ref().unwrap().num_permits();
            let parsing = tokio::spawn(parse_websocket_message(message, permit));
            tokio::task::yield_now().await;
            let was_pending = !parsing.is_finished();
            parsing.abort();
            let result = parsing.await;
            let was_cancelled = result.as_ref().is_err_and(|error| error.is_cancelled());
            drop(result);
            let available_after_cancellation = budget.available_permits();
            release.send(()).unwrap();
            occupied.await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while budget.available_permits() != REQUEST_BODY_BUDGET_PERMITS {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(was_pending, "large JSON must use the blocking pool");
            assert!(was_cancelled);
            assert_eq!(
                available_after_cancellation,
                REQUEST_BODY_BUDGET_PERMITS - held
            );
        });
    }
}

#[cfg(test)]
mod idle_disconnect_tests {
    use super::super::tests::local_websocket_pair;
    use super::*;
    use tokio_tungstenite::tungstenite::{Error, error::ProtocolError};

    #[test]
    fn peer_disconnects_end_the_idle_wait_without_an_error() {
        for error in [
            Error::ConnectionClosed,
            Error::AlreadyClosed,
            Error::Protocol(ProtocolError::ResetWithoutClosingHandshake),
            Error::Io(std::io::ErrorKind::UnexpectedEof.into()),
            Error::Io(std::io::ErrorKind::ConnectionReset.into()),
        ] {
            assert!(idle_downstream_message(Some(Err(error))).unwrap().is_none());
        }
        assert!(
            idle_downstream_message(Some(Err(Error::Io(
                std::io::ErrorKind::PermissionDenied.into()
            ))))
            .is_err()
        );
    }

    #[tokio::test]
    async fn dropped_client_socket_ends_the_connection_cleanly() {
        let (socket, peer) = local_websocket_pair().await;
        let mut downstream = WebSocketResponsesDownstream::new(socket);
        drop(peer);
        assert!(downstream.next_message().await.unwrap().is_none());
    }
}
