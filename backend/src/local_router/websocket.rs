use super::*;

pub(crate) enum IdleWebSocketEvent {
    Downstream(
        Option<std::result::Result<WebSocketMessage, tokio_tungstenite::tungstenite::Error>>,
    ),
    Upstream(Option<std::result::Result<WebSocketMessage, tokio_tungstenite::tungstenite::Error>>),
    MaintainUpstream,
}

impl WebSocketResponsesDownstream {
    #[cfg(test)]
    pub(crate) fn new(socket: WebSocketStream<TcpStream>) -> Self {
        Self::with_shared_backoffs(
            socket,
            Arc::new(Mutex::new(UpstreamWebSocketBackoffs::default())),
            Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS)),
        )
    }

    pub(crate) fn with_shared_backoffs(
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

    pub(crate) fn set_stream_id(&mut self, stream_id: Option<String>) {
        self.stream_id = stream_id;
    }

    pub(crate) fn clear_stream_id(&mut self) {
        self.stream_id = None;
        self.terminal_started = false;
    }

    pub(crate) async fn next_message(&mut self) -> Result<Option<WebSocketMessage>> {
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
                .unwrap_or_else(std::sync::PoisonError::into_inner)
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
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        serde_json::from_str::<Value>(&data)
            .context("Responses WebSocket 上游 SSE data 不是有效 JSON")?,
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
            status, code, &message, route,
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
        probe: Option<&RouteRequestLogProbe>,
    ) -> Result<UpstreamWebSocketAttempt> {
        self.proxy_upstream_websocket(route, headers, body, probe)
            .await
    }
}

pub(crate) fn websocket_response_failed_event(
    status: u16,
    code: &str,
    message: &str,
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

pub(crate) async fn proxy_native_response_to_websocket(
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
