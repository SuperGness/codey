use super::*;

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
    pub(crate) fn new(socket: WebSocketStream<TcpStream>) -> Self {
        Self::with_shared_backoffs(
            socket,
            Arc::new(Mutex::new(UpstreamWebSocketBackoffs::default())),
            Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS)),
            Arc::new(Mutex::new(IdleDownstreamRegistry::default())),
        )
    }

    pub(crate) fn with_shared_backoffs(
        socket: WebSocketStream<TcpStream>,
        websocket_backoffs: Arc<Mutex<UpstreamWebSocketBackoffs>>,
        request_body_budget: Arc<Semaphore>,
        idle_registry: Arc<Mutex<IdleDownstreamRegistry>>,
    ) -> Self {
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
            pending_budget_blocked: false,
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
        message: &WebSocketMessage,
    ) -> Result<bool> {
        let WebSocketMessage::Text(text) = message else {
            return Ok(false);
        };
        #[derive(serde::Deserialize)]
        struct MessageType {
            #[serde(rename = "type")]
            kind: String,
        }
        if !serde_json::from_str::<MessageType>(text.as_str())
            .is_ok_and(|message| message.kind == "response.steer")
        {
            return Ok(false);
        }
        // 预算不足时沿用普通帧的有界排队，外层取得预算后再处理。
        let Ok(_permit) = acquire_request_body_budget(&self.request_body_budget, text.len()) else {
            return Ok(false);
        };
        let body: Value = serde_json::from_str(text.as_str())?;
        let event = self.steering.accept(&body, &self.request_body_budget);
        self.write_steering_event(&event).await?;
        Ok(true)
    }

    fn response_interrupt_from_message(message: &WebSocketMessage) -> Result<Option<Value>> {
        let WebSocketMessage::Text(text) = message else {
            return Ok(None);
        };
        let Ok(Value::Object(body)) = serde_json::from_str::<Value>(text.as_str()) else {
            return Ok(None);
        };
        if body.get("type").and_then(Value::as_str) != Some("response.interrupt") {
            return Ok(None);
        }
        Ok(Some(Value::Object(body)))
    }

    fn take_response_interrupt(&mut self) -> Option<Value> {
        self.pending_interrupt.take()
    }

    pub(crate) fn handle_idle_response_interrupt(
        &mut self,
        message: &WebSocketMessage,
    ) -> Result<bool> {
        Ok(Self::response_interrupt_from_message(message)?.is_some())
    }

    pub(crate) async fn next_message(&mut self) -> Result<Option<WebSocketMessage>> {
        self.write_steering_notifications().await?;
        if let Some(body) = self.steering.take_continuation() {
            return Ok(Some(WebSocketMessage::Text(
                serde_json::to_string(&body)?.into(),
            )));
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
                let (message, _permit) = self.pending_messages.pop_front().unwrap();
                if self.pending_messages.is_empty() {
                    self.pending_budget_blocked = false;
                }
                return Ok(Some(message));
            }
            if idle_slot.is_none() {
                idle_slot = Some(IdleDownstreamRegistry::register(&self.idle_registry));
            }
            let evict = &mut idle_slot.as_mut().unwrap().1;
            let Some(upstream) = self.upstream.as_ref() else {
                return self.wait_for_idle_downstream(evict, idle_deadline).await;
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

    async fn wait_for_idle_downstream(
        &mut self,
        evict: &mut oneshot::Receiver<()>,
        idle_deadline: tokio::time::Instant,
    ) -> Result<Option<WebSocketMessage>> {
        // 合成 previous_response_id 只存在这条 socket 的历史里，空闲超时不能
        // 关掉它，否则 Codex 复用连接时会丢掉续接。连接数超出上限时另有回收。
        let stateful = self.adapted_history.last.is_some() || self.native_history.has_history();
        let read = self.socket.next();
        tokio::pin!(read);
        let idle = tokio::time::sleep_until(idle_deadline);
        tokio::pin!(idle);
        let outcome = tokio::select! {
            biased;
            _ = &mut *evict => IdleDownstreamWait::Evicted,
            message = &mut read => IdleDownstreamWait::Message(message),
            _ = &mut idle, if !stateful => IdleDownstreamWait::TimedOut,
        };
        match outcome {
            IdleDownstreamWait::Evicted => Ok(None),
            IdleDownstreamWait::Message(message) => message
                .transpose()
                .context("读取 Codey Responses WebSocket 消息失败"),
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
        let official_web_search = route.official_account
            && body
                .get("tools")
                .and_then(Value::as_array)
                .is_some_and(|tools| {
                    tools.iter().any(|tool| {
                        matches!(
                            tool.get("type").and_then(Value::as_str),
                            Some(
                                "web_search"
                                    | "web_search_preview"
                                    | "web_search_2025_08_26"
                                    | "web_search_preview_2025_03_11"
                            )
                        )
                    })
                });
        if !route.supports_websockets || official_web_search {
            self.native_history
                .prepare_for_route(route, auth_identity, body);
            return Ok(UpstreamWebSocketAttempt::UseHttp);
        }
        let upstream_url = route
            .upstream_websocket_url
            .as_ref()
            .map_err(|error| anyhow::anyhow!(error.clone()))?;
        let now = Instant::now();
        let backoff_key =
            UpstreamWebSocketBackoffKey::for_route(route, upstream_url, auth_identity);
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
        if route.official_account && self.native_history.requires_http() {
            return Ok(UpstreamWebSocketAttempt::UseHttp);
        }
        if !cached_matches {
            // A response ID belongs to its original upstream socket. Reconnect
            // with full history only before sending this new request.
            if previous_response_key.is_some()
                && self.native_history.restore(history_key, body).is_err()
            {
                return Ok(UpstreamWebSocketAttempt::UseHttp);
            }
            self.upstream.take();
        }
        normalize_native_responses_context(body, discard_opaque_reasoning);
        if route.official_account {
            sanitize_official_upstream_history(body);
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
                return Ok(UpstreamWebSocketAttempt::UseHttp);
            };
            match self
                .wait_for_upstream(connect_upstream_responses_websocket(
                    upstream_url,
                    headers,
                    probe,
                ))
                .await?
            {
                Ok(socket) => {
                    self.websocket_backoffs
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .record_success(&backoff_key);
                    CachedUpstreamWebSocket {
                        route_id: route.provider_id.clone(),
                        url: upstream_url.clone(),
                        auth_identity,
                        response_ids: VecDeque::new(),
                        config_identity: route.websocket_config,
                        liveness: UpstreamWebSocketLiveness::new(Instant::now()),
                        socket,
                    }
                }
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
                        return Ok(UpstreamWebSocketAttempt::UseHttp);
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
            probe.record_upstream_body(RequestBodySummary::from_responses_body(
                body,
                Some(message.len() as u64),
            ));
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
        let response_deadline = tokio::time::Instant::now() + UPSTREAM_RESPONSE_TIMEOUT;
        let mut response_bytes = 0_usize;
        loop {
            if tokio::time::Instant::now() >= response_deadline {
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
                        record_upstream_websocket_failure(&self.websocket_backoffs, &backoff_key);
                        anyhow::bail!("读取 Responses WebSocket 上游事件超时");
                    }
                    Err(error)
                        if error
                            .downcast_ref::<DownstreamResponseInterrupt>()
                            .is_some() =>
                    {
                        let interrupt = self
                            .take_response_interrupt()
                            .context("Responses WebSocket 缺少待发送的中断事件")?;
                        let message = serde_json::to_string(&interrupt)
                            .context("序列化 Responses WebSocket 中断事件失败")?;
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
                            let successful_backoff_key = UpstreamWebSocketBackoffKey::for_route(
                                route,
                                upstream_url,
                                upstream.auth_identity,
                            );
                            if let Some(response_id) = produced_response_id {
                                if upstream.response_ids.len() >= MAX_CACHED_RESPONSE_IDS {
                                    upstream.response_ids.pop_front();
                                }
                                upstream.response_ids.push_back(response_id);
                            }
                            if responses_websocket_connection_is_reusable(&event) {
                                self.upstream = Some(upstream);
                            }
                            self.websocket_backoffs
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
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
                            if self.handle_steering_message(&message).await? {
                                continue;
                            }
                            if let Some(interrupt) = Self::response_interrupt_from_message(&message)?
                                && self.interrupt_forwarding {
                                self.pending_interrupt = Some(interrupt);
                                return Err(DownstreamResponseInterrupt.into());
                            }
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
    let xai_fix = current_xai_response_fix();
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
                let mut event = serde_json::from_str::<Value>(&data)
                    .context("Responses HTTP 上游 SSE data 不是有效 JSON")?;
                normalize_response_failure(&mut event, retry_advice.as_ref());
                if let Some(fix) = xai_fix.as_ref() {
                    fix.apply(&mut event);
                }
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
            let mut event = serde_json::from_str::<Value>(&data)
                .context("Responses HTTP 上游 SSE 末尾 data 不是有效 JSON")?;
            normalize_response_failure(&mut event, retry_advice.as_ref());
            if let Some(fix) = xai_fix.as_ref() {
                fix.apply(&mut event);
            }
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
        for mut event in events {
            normalize_response_failure(&mut event, retry_advice.as_ref());
            if let Some(fix) = xai_fix.as_ref() {
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
        }
        return downstream.finish_event_stream().await;
    }
    match serde_json::from_slice::<Value>(&body) {
        Ok(mut value) => {
            if let Some(fix) = xai_fix.as_ref() {
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
    async fn search_http_selection_preserves_other_websocket_requests() {
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
        route.supports_websockets = true;
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
            UpstreamWebSocketAttempt::UseHttp
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
