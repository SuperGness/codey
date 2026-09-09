use super::*;

impl RouterServer {
    pub(crate) async fn handle_connection(&self, mut stream: TcpStream) -> Result<()> {
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
            if !pending.request.path.starts_with("/codey/") {
                self.record_rejected_request(
                    &pending.request,
                    "http_rejected",
                    401,
                    "invalid_router_token",
                );
            }
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
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                );
                let data = snapshot
                    .model_ids()
                    .iter()
                    .map(|id| json!({"id":id,"object":"model","owned_by":"codey"}))
                    .collect::<Vec<_>>();
                write_json_response(&mut stream, 200, &json!({"object":"list","data":data}))
                    .await?;
                if let Some(probe) = self.begin_basic_request_log(&request, "models") {
                    probe.mark_response_started(200);
                    probe.finish_success();
                }
            }
            ("POST", "/codey/api/load_codey_config") => {
                let catalog = self
                    .snapshot
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .request_log_catalog
                    .clone();
                write_json_response(&mut stream, 200, &json!({"config": catalog})).await?;
            }
            (
                "POST",
                "/codey/api/query_route_request_logs" | "/codey/api/query_route_request_log_stats",
            ) => {
                let statistics = route_path.ends_with("query_route_request_log_stats");
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
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .request_log_backend;
                let root = self.request_log.root().to_path_buf();
                match tokio::task::spawn_blocking(move || {
                    let value = if statistics {
                        serde_json::to_value(
                            crate::route_request_log::query_route_request_log_stats(
                                &root, backend, query,
                            )?,
                        )
                    } else {
                        serde_json::to_value(crate::route_request_log::query_route_request_logs(
                            &root, backend, query,
                        )?)
                    };
                    value.map_err(anyhow::Error::from)
                })
                .await
                {
                    Ok(Ok(mut page)) => {
                        if statistics {
                            page["recordingHealth"] =
                                serde_json::to_value(self.request_log.health().await)?;
                        }
                        write_json_response(&mut stream, 200, &page).await?;
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
                if !request.path.starts_with("/codey/") {
                    self.record_rejected_request(&request, "http_rejected", 404, "not_found");
                }
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

    pub(crate) async fn proxy_image_generation(
        &self,
        mut request: HttpRequest,
        mut stream: TcpStream,
    ) -> Result<()> {
        let probe = self.begin_basic_request_log(&request, "images_generations");
        let _log_guard = RouteRequestLogGuard::new(probe.clone());
        let mark_error = |status, code: &str| {
            if let Some(probe) = &probe {
                probe.mark_error(status, code);
            }
        };
        let mut body = match serde_json::from_slice::<Value>(&request.body) {
            Ok(body) if body.is_object() => body,
            Ok(_) => {
                mark_error(400, "invalid_request_body");
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
                mark_error(400, "invalid_request_body");
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
                mark_error(400, "route_metadata_invalid");
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
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let binding_keys = request_binding_keys(&request);
        let bound_route = self
            .bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .route_for_keys(&binding_keys);
        let route = match snapshot
            .target_for_auxiliary_request(route_hint.as_deref(), bound_route.as_deref())
        {
            Ok(route) => route,
            Err(error) => {
                mark_error(404, "route_not_enabled");
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
        if let Some(probe) = &probe {
            let model = body
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default();
            probe.resolve_route(
                &route.provider_id,
                &route.route_name,
                model,
                model,
                &route.upstream_authority,
                route.protocol.label(),
                "images",
                false,
            );
        }
        if route.protocol == UpstreamProtocol::AnthropicMessages {
            mark_error(400, "image_generation_not_supported");
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
                mark_error(502, "route_configuration_error");
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
                mark_error(502, "route_configuration_error");
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
                mark_error(status, code);
                write_error_response(&mut stream, status, code, message, Some(&route)).await?;
                return Ok(());
            }
        };
        let stream_requested = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
        if let Some(probe) = &probe {
            probe.set_request_protocol(if stream_requested {
                RequestProtocol::Sse
            } else {
                RequestProtocol::Http
            });
            probe.mark_upstream_send(if stream_requested {
                UpstreamTransport::HttpSse
            } else {
                UpstreamTransport::Http
            });
        }
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
                    mark_error(status, code);
                    write_text_error_response(&mut stream, status, code, message).await?;
                    return Ok(());
                }
                Err(_) => {
                    mark_error(504, "upstream_header_timeout");
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
        if let Some(probe) = &probe {
            probe.mark_upstream_headers(
                response.status().as_u16(),
                response
                    .headers()
                    .get("x-request-id")
                    .and_then(|value| value.to_str().ok()),
            );
        }
        let result = write_proxy_response(&mut stream, response, probe.as_ref(), false).await;
        if let Some(probe) = &probe {
            if result.is_ok() {
                probe.finish_success();
            } else if result
                .as_ref()
                .err()
                .is_some_and(|error| error.is::<DownstreamClosed>())
            {
                probe.mark_cancelled("downstream_image_response_closed");
                probe.finish_cancelled();
            } else {
                probe.mark_error(502, "upstream_image_response_failed");
                probe.finish_failed();
            }
        }
        result
    }

    fn begin_basic_request_log(
        &self,
        request: &HttpRequest,
        kind: &str,
    ) -> Option<RouteRequestLogProbe> {
        self.request_log.begin(|producer| {
            let request_id = current_router_request_id().unwrap_or_default();
            let (session, parent) = request_log_codex_session(request);
            producer.begin(RouteRequestLogStart {
                request_id: &request_id,
                started_at: current_router_request_started_at().unwrap_or_else(Instant::now),
                request_protocol: RequestProtocol::Http,
                request_kind: kind,
                requested_model: "",
                reasoning_effort: None,
                thinking_budget_tokens: None,
                codex_session_id: session,
                codex_session_is_parent: parent,
            })
        })
    }

    fn record_rejected_request(&self, request: &HttpRequest, kind: &str, status: u16, code: &str) {
        if let Some(probe) = self.begin_basic_request_log(request, kind) {
            probe.mark_error(status, code);
            probe.finish_failed();
        }
    }

    pub(crate) async fn prepare_upstream_request_headers(
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

    pub(crate) fn authorized(&self, request: &HttpRequest) -> bool {
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
    pub(crate) async fn handle_responses_websocket(&self, stream: TcpStream) -> Result<()> {
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
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .context("Codey Responses WebSocket 缺少握手上下文")?;
        let mut downstream = WebSocketResponsesDownstream::with_shared_backoffs(
            socket,
            Arc::clone(&self.websocket_backoffs),
            Arc::clone(&self.request_body_budget),
        );
        downstream.native_history = NativeResponsesHistory::with_cache(
            Arc::clone(&self.native_history_cache),
            &context.headers,
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
                    downstream.adapted_history.clear_pending();
                    downstream.native_history.clear_pending();
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
                                    format!("Codey 本地路由处理请求失败；请求 ID：{request_id}"),
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

    pub(crate) async fn proxy_responses(
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
                    self.record_rejected_request(
                        &request,
                        request_kind.label(),
                        503,
                        "router_memory_busy",
                    );
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
                    self.record_rejected_request(
                        &request,
                        request_kind.label(),
                        415,
                        "unsupported_content_encoding",
                    );
                    downstream
                        .write_error(415, "unsupported_content_encoding", error.to_string(), None)
                        .await?;
                    return Ok(());
                }
                Err(error) => {
                    let mut downstream = HttpResponsesDownstream::new(stream);
                    self.record_rejected_request(
                        &request,
                        request_kind.label(),
                        400,
                        "invalid_request_body",
                    );
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
                self.record_rejected_request(
                    &request,
                    request_kind.label(),
                    500,
                    "request_parse_failed",
                );
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
                self.record_rejected_request(
                    &request,
                    request_kind.label(),
                    400,
                    "invalid_request_body",
                );
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
                self.record_rejected_request(
                    &request,
                    request_kind.label(),
                    400,
                    "invalid_request_body",
                );
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

    pub(crate) async fn proxy_parsed_responses<D>(
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
                request_kind: if request_kind == ResponsesRequestKind::Create
                    && is_compaction_request(&body, request_kind)
                {
                    "responses_compact_v2"
                } else {
                    request_kind.label()
                },
                requested_model,
                reasoning_effort,
                thinking_budget_tokens,
                codex_session_id,
                codex_session_is_parent,
            })
        });
        if probe.is_none() {
            return self
                .proxy_with_compaction_budget(request, body, encoded_body, request_kind, downstream)
                .await;
        }
        let _request_log_guard = RouteRequestLogGuard::new(probe.clone());
        let mut observed = ObservedResponsesDownstream::new(downstream, probe);
        self.proxy_with_compaction_budget(request, body, encoded_body, request_kind, &mut observed)
            .await
    }

    pub(crate) async fn proxy_parsed_responses_inner<D>(
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
        let compacting = is_compaction_request(&body, request_kind);
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
                .unwrap_or_else(std::sync::PoisonError::into_inner),
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
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        if compacting && !resolved.route.supports_remote_compaction {
            return downstream
                .write_error(
                    400,
                    "compaction_unsupported",
                    "当前线路未启用原生远程压缩，请使用 Codex 本地摘要".into(),
                    Some(&resolved.route),
                )
                .await;
        }
        if bridge != ProtocolBridge::NativeResponses {
            if compacting {
                return downstream
                    .write_error(
                        400,
                        "compaction_unsupported",
                        "当前线路不支持原生远程压缩，请使用 Codex 本地摘要".into(),
                        Some(&resolved.route),
                    )
                    .await;
            }
            if let Err(error) = validate_portable_context(&body) {
                return downstream
                    .write_error(
                        400,
                        "context_not_portable",
                        error.to_string(),
                        Some(&resolved.route),
                    )
                    .await;
            }
        }
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
        downstream.select_route(&resolved.route);
        if bridge != ProtocolBridge::NativeResponses
            && let Err(error) = downstream.prepare_adapted_response_context(&mut body)
        {
            downstream
                .write_error(
                    413,
                    "context_budget_exceeded",
                    error.to_string(),
                    Some(&resolved.route),
                )
                .await?;
            return Ok(());
        }
        if bridge == ProtocolBridge::NativeResponses
            && has_codey_synthetic_previous_response_id(&body)
        {
            match downstream.prepare_adapted_response_context(&mut body) {
                Ok(true) => {}
                _ => {
                    return downstream
                        .write_error(
                            400,
                            "context_not_portable",
                            "无法恢复旧线路的会话历史，请重新发送完整上下文；未删除历史引用".into(),
                            Some(&resolved.route),
                        )
                        .await;
                }
            }
            body_mutated = true;
            // Expansion changed the input too; do not reuse its original raw
            // JSON slice, which would contain only the latest delta.
            encoded_body = None;
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
            && !compacting
            && request_kind == ResponsesRequestKind::Create
            && stream_requested
            && bridge == ProtocolBridge::NativeResponses
        {
            let had_previous_response = responses_previous_response_id(&upstream_body).is_some();
            let websocket_attempt = downstream
                .try_proxy_upstream_websocket(&resolved.route, &headers, &mut upstream_body)
                .await?;
            if websocket_attempt == UpstreamWebSocketAttempt::Completed {
                return Ok(());
            }
            if had_previous_response && responses_previous_response_id(&upstream_body).is_none() {
                // Reconnection may have expanded history before its handshake failed.
                body_mutated = true;
                encoded_body = None;
            }
            if resolved.route.supports_websockets
                && let Some(probe) = downstream.request_log_probe()
            {
                probe.mark_fallback("websocket_to_http_sse");
            }
            match downstream.prepare_native_http_fallback(
                &resolved.route,
                &headers,
                &mut upstream_body,
            ) {
                Ok(true) => {
                    body_mutated = true;
                    encoded_body = None;
                }
                Ok(false) => {}
                Err(error) => {
                    record_router_failure_nonblocking(
                        "local_router_context_not_recoverable",
                        "restore_native_response_history",
                        error.to_string(),
                        json!({"requestId": current_router_request_id(), "routeId": resolved.route.provider_id}),
                    );
                    return downstream
                        .write_error(
                            400,
                            "context_not_recoverable",
                            error.to_string(),
                            Some(&resolved.route),
                        )
                        .await;
                }
            }
        }
        let upstream_stream_requested = upstream_body
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut request_builder = self.client.post(upstream_url).headers(headers);
        if compacting {
            request_builder = request_builder.timeout(COMPACTION_TIMEOUT);
        }
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
        let response_result = await_upstream(
            downstream,
            tokio::time::timeout(response_header_timeout, request_builder.send()),
        )
        .await?;
        let response = match response_result {
            Ok(Ok(response)) => response,
            Ok(Err(error)) if compacting && error.is_timeout() => {
                return downstream
                    .write_error(
                        504,
                        "compaction_timeout",
                        "远程压缩超过总时限，原始会话历史未被 Codey 修改，请稍后重试".into(),
                        Some(&resolved.route),
                    )
                    .await;
            }
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
                        "upstreamStream": upstream_stream_requested,
                        "responseHeaderTimeoutSeconds": response_header_timeout.as_secs(),
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
                        "upstreamStream": upstream_stream_requested,
                        "responseHeaderTimeoutSeconds": response_header_timeout.as_secs(),
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
        let result = match bridge {
            // Every upstream protocol surfaces its real HTTP status. Mapping
            // Anthropic 4xx to 502 made Codex retry non-retryable failures.
            _ if !response.status().is_success() => {
                write_upstream_http_error(downstream, response, &resolved, bridge, request_kind)
                    .await
            }
            _ if compacting => {
                write_validated_compaction(
                    downstream,
                    response,
                    request_kind == ResponsesRequestKind::Create,
                    stream_requested,
                    &resolved.route,
                )
                .await
            }
            ProtocolBridge::ResponsesToAnthropicMessages
            | ProtocolBridge::ResponsesToChatCompletions => {
                write_adapted_upstream_as_responses(
                    downstream,
                    response,
                    bridge,
                    &resolved.upstream_model,
                    stream_requested,
                    &resolved.route,
                    &tool_bridge,
                )
                .await
            }
            _ => downstream.proxy_response(response).await,
        };
        if let Err(error) = &result
            && downstream_websocket
            && !error.is::<DownstreamClosed>()
        {
            // A local WebSocket can relay an HTTP-only route. Report the
            // upstream response failure before the socket-level fallback.
            record_router_failure_nonblocking(
                "local_router_upstream_response_failed",
                "proxy_local_router_request",
                format!("{error:#}"),
                json!({ "routeId": resolved.provider_id, "requestId": current_router_request_id() }),
            );
            let detail = sanitize_upstream_error_text(&error.to_string(), &resolved.route, 512)
                .unwrap_or_else(|| "上游响应未能完成".to_string());
            return downstream
                .write_error(
                    502,
                    "upstream_response_failed",
                    format!(
                        "Codey 线路「{}」处理上游 HTTP 响应失败：{detail}",
                        route_display_name(&resolved.route)
                    ),
                    Some(&resolved.route),
                )
                .await;
        }
        result
    }
}
