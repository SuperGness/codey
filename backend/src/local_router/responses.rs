use super::*;

impl RouterServer {
    pub(crate) async fn handle_connection(
        &self,
        mut stream: TcpStream,
        connection_permit: OwnedSemaphorePermit,
    ) -> Result<()> {
        // 协议探测和读取请求头共用这一次超时。慢客户端不能先占满探测窗口，
        // 再单独占满读头窗口。
        let header_started = Instant::now();
        match probe_responses_websocket(&stream).await? {
            ResponsesWebSocketProbe::Upgrade => {
                return self
                    .handle_responses_websocket(stream, connection_permit)
                    .await;
            }
            ResponsesWebSocketProbe::Http => {}
            ResponsesWebSocketProbe::Silent => {
                // 空闲或半开连接不会发出请求，和普通 HTTP 路径的读取超时一样
                // 回一个 408 即可；对端可能已经断开，这里只做尽力回复。
                let _connection_permit = connection_permit;
                let _ = write_error_response(
                    &mut stream,
                    408,
                    "request_timeout",
                    "读取本地路由请求超时",
                    None,
                )
                .await;
                return Ok(());
            }
        }
        let _connection_permit = connection_permit;
        let header_budget = REQUEST_READ_TIMEOUT.saturating_sub(header_started.elapsed());
        let pending =
            match tokio::time::timeout(header_budget, read_http_request_head(&mut stream)).await {
                Ok(Ok(request)) => request,
                Ok(Err(error)) if error.is::<RequestBodyTooLarge>() => {
                    write_error_response(
                        &mut stream,
                        413,
                        "request_too_large",
                        error.to_string(),
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
        if pending.request.path == "/healthz" {
            write_json_response(&mut stream, 200, &json!({"status":"ok"})).await?;
            return Ok(());
        }
        // 与本地路由使用同一地址和令牌。没有令牌不会统计，也不会启动共享 app-server。
        if pending.request.method == "POST"
            && pending.request.path == crate::appserver_call::HTTP_PATH
        {
            if !self.authorized(&pending.request) {
                self.record_rejected_request(
                    &pending.request,
                    "http_rejected",
                    401,
                    "invalid_router_token",
                );
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
            if crate::appserver_call::call_is_too_large(pending.content_length) {
                write_error_response(
                    &mut stream,
                    413,
                    "request_too_large",
                    "app-server 请求过大",
                    None,
                )
                .await?;
                return Ok(());
            }
            let Some(request) = self
                .read_admitted_request_body(&mut stream, pending)
                .await?
            else {
                return Ok(());
            };
            let route = crate::appserver_call::Route {
                base_url: self.endpoint.base_url.clone(),
                token: self.endpoint.token.clone(),
                requires_openai_auth: self.endpoint.requires_openai_auth,
                supports_websockets: self.endpoint.supports_websockets,
                supports_remote_compaction: self.endpoint.supports_remote_compaction,
            };
            let (status, body) = crate::appserver_call::handle(&route, &request.body).await;
            write_json_response(&mut stream, status, &body).await?;
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
            self.record_rejected_request(
                &pending.request,
                "http_rejected",
                401,
                "invalid_router_token",
            );
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
        let Some(request) = self
            .read_admitted_request_body(&mut stream, pending)
            .await?
        else {
            return Ok(());
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
            ("POST", "/codey/api/query_official_account_usage") => {
                #[derive(serde::Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct UsageQuery {
                    force_refresh: Option<bool>,
                    account_id: Option<String>,
                }
                let args = match serde_json::from_slice::<UsageQuery>(&request.body) {
                    Ok(args) => args,
                    Err(error) => {
                        write_error_response(
                            &mut stream,
                            400,
                            "invalid_usage_query",
                            format!("额度查询参数无效：{error}"),
                            None,
                        )
                        .await?;
                        return Ok(());
                    }
                };
                let requested_account = args
                    .account_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|account_id| !account_id.is_empty());
                let route = {
                    let snapshot = self
                        .snapshot
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    match requested_account {
                        Some(account_id) => snapshot
                            .routes
                            .values()
                            .find(|route| {
                                route.official_account
                                    && route
                                        .official_auth
                                        .as_ref()
                                        .is_some_and(|auth| auth.account_id == account_id)
                            })
                            .cloned(),
                        // 没有账号参数时读取默认账号，页头额度要跟客户端登录身份一致。
                        None => snapshot
                            .default_official_provider
                            .as_deref()
                            .and_then(|provider_id| snapshot.routes.get(provider_id))
                            .filter(|route| route.official_account)
                            .cloned(),
                    }
                };
                let value = match route {
                    None => {
                        json!({"status": "unavailable", "reason": "official_account_missing", "message": "当前线路列表中没有可用的官方账号线路"})
                    }
                    Some(route) => {
                        // 每个账号读取自己的凭据文档，额度查询也走该线路的出口代理。
                        let auth_path = route
                            .official_auth
                            .as_ref()
                            .map(|auth| auth.path.clone())
                            .unwrap_or_else(|| self.official_auth_path.clone());
                        let mut cache = self.account_usage_cache.lock().await;
                        crate::account_usage::query_snapshot_at(
                            cache.for_auth_path(&auth_path),
                            &auth_path,
                            args.force_refresh.unwrap_or(false),
                            route.upstream_proxy.as_deref(),
                        )
                        .await
                    }
                };
                write_json_response(&mut stream, 200, &value).await?;
            }
            // 系统浏览器里的请求日志页只能经由本地路由访问后端；账号筛选、账号名
            // 显示和按账号推算额度都依赖同一份账号目录。这里只读取账号文档，不再
            // 同步 Codex 登录，避免浏览器的只读页面改动登录状态。
            ("POST", "/codey/api/list_official_accounts") => {
                let store = crate::official_accounts::OfficialAccountStore::for_config_path(
                    &crate::config::default_config_path(),
                );
                let message = match tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
                    let default_account_id = store.default_account_id()?;
                    Ok(json!({
                        "status": "ok",
                        "accounts": store.summaries()?,
                        "defaultAccountId": default_account_id,
                    }))
                })
                .await
                {
                    Ok(Ok(value)) => {
                        write_json_response(&mut stream, 200, &value).await?;
                        return Ok(());
                    }
                    Ok(Err(error)) => format!("读取官方账号列表失败：{error:#}"),
                    Err(error) => format!("读取官方账号列表任务异常退出：{error}"),
                };
                write_error_response(
                    &mut stream,
                    500,
                    "official_accounts_unavailable",
                    message,
                    None,
                )
                .await?;
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
            ("POST", "/codey/api/query_route_request_log_models") => {
                let query = match serde_json::from_slice::<
                    crate::route_request_log::RouteRequestLogModelQuery,
                >(&request.body)
                {
                    Ok(query) => query,
                    Err(error) => {
                        write_error_response(
                            &mut stream,
                            400,
                            "invalid_request_log_query",
                            format!("模型候选查询参数无效：{error}"),
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
                let result = tokio::task::spawn_blocking(move || {
                    crate::route_request_log::query_route_request_log_models(&root, backend, query)
                })
                .await;
                let message = match result {
                    Ok(Ok(page)) => {
                        write_json_response(&mut stream, 200, &serde_json::to_value(page)?).await?;
                        return Ok(());
                    }
                    Ok(Err(error)) => format!("查询模型候选失败：{error:#}"),
                    Err(error) => format!("模型候选查询任务异常退出：{error}"),
                };
                write_error_response(&mut stream, 500, "request_log_query_failed", message, None)
                    .await?;
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
                self.record_rejected_request(&request, "http_rejected", 404, "not_found");
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

    /// Waits briefly for request-buffer budget, then reads the body under the
    /// router read timeout. Returns `None` after writing the error response.
    async fn read_admitted_request_body(
        &self,
        stream: &mut TcpStream,
        pending: PendingHttpRequest,
    ) -> Result<Option<HttpRequest>> {
        let admission = match acquire_request_body_budget_within(
            &self.request_body_budget,
            pending.content_length,
            REQUEST_BODY_BUDGET_WAIT,
        )
        .await
        {
            Ok(permit) => permit,
            Err(error)
                if error
                    .downcast_ref::<RequestBodyBudgetUnavailable>()
                    .is_some() =>
            {
                write_error_response(
                    stream,
                    503,
                    "router_memory_busy",
                    "Codey 本地路由请求缓冲区已满，请稍后重试",
                    None,
                )
                .await?;
                return Ok(None);
            }
            Err(error) if error.is::<RequestBodyTooLarge>() => {
                write_error_response(stream, 413, "request_too_large", error.to_string(), None)
                    .await?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        match tokio::time::timeout(
            REQUEST_READ_TIMEOUT,
            read_http_request_body_with_budget(stream, pending, None),
        )
        .await
        {
            Ok(Ok(mut request)) => {
                request._body_budget_permit = admission;
                Ok(Some(request))
            }
            Ok(Err(error)) => {
                write_error_response(
                    stream,
                    400,
                    "invalid_http_request",
                    format!("本地路由请求无效：{error:#}"),
                    None,
                )
                .await?;
                Ok(None)
            }
            Err(_) => {
                write_error_response(stream, 408, "request_timeout", "读取本地路由请求超时", None)
                    .await?;
                Ok(None)
            }
        }
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
        let image_model = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let route = match snapshot.route_for_image_request(image_model, route_hint.as_deref()) {
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
                route
                    .official_auth
                    .as_ref()
                    .map(|auth| auth.account_id.as_str()),
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
        let mut headers = headers;
        // insert 保证只有一个 content-type；在 .headers() 之后用 .header() 会追加重复值。
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(probe) = &probe {
            probe.set_upstream_request_headers(&format_upstream_headers(&headers));
        }
        let upstream_client = match self.upstream_client(&route) {
            Ok(client) => client,
            Err(message) => {
                mark_error(502, "route_configuration_error");
                write_error_response(
                    &mut stream,
                    502,
                    "route_configuration_error",
                    message,
                    Some(&route),
                )
                .await?;
                return Ok(());
            }
        };
        let request_body = if body_mutated {
            Bytes::from(serde_json::to_vec(&body).context("序列化 Images 上游请求失败")?)
        } else {
            Bytes::from(request.body)
        };
        let response_header_timeout = if stream_requested {
            UPSTREAM_RESPONSE_HEADER_TIMEOUT
        } else {
            UPSTREAM_NON_STREAM_RESPONSE_HEADER_TIMEOUT
        };
        let response = match send_for_response_headers(
            upstream_client.post(&upstream_url).headers(headers),
            request_body,
            response_header_timeout,
            || {},
        )
        .await
        {
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
        if !matches!(
            request.path.as_str(),
            "/v1/models"
                | "/models"
                | "/v1/responses"
                | "/responses"
                | "/v1/images/generations"
                | "/images/generations"
                | "/v1/responses/compact"
                | "/responses/compact"
                | "/v1/v1/responses/compact"
                | "/codex/v1/responses/compact"
        ) {
            return;
        }
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
        let connection_scoped = connection_scoped_header_names(
            request
                .headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        );
        for (name, value) in &request.headers {
            if !connection_scoped.contains(&name.to_ascii_lowercase())
                && should_forward_incoming_header(name, route.official_account)
                && let (Ok(name), Ok(value)) = (
                    HeaderName::from_bytes(name.as_bytes()),
                    HeaderValue::from_str(value),
                )
            {
                headers.insert(name, value);
            }
        }
        // 线路覆盖中的空值是删除标记：合并时移除对应请求头，而不是把空值头
        // 原样发到上游。Codey 内部请求 ID 只写入下游响应和本地日志，不随上游
        // 请求外发，避免向上游暴露代理痕迹。
        apply_upstream_headers(&mut headers, prepared_headers);
        if route.official_account {
            let (auth_path, accepts_incoming_authorization) = match &route.official_auth {
                Some(auth) => (auth.path.as_path(), auth.accepts_incoming_authorization),
                None => (self.official_auth_path.as_path(), true),
            };
            let official_auth = resolve_official_upstream_auth(
                request,
                &self.bearer_token,
                auth_path,
                &self.official_auth_cache,
                accepts_incoming_authorization,
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
    pub(crate) async fn handle_responses_websocket(
        &self,
        stream: TcpStream,
        connection_permit: OwnedSemaphorePermit,
    ) -> Result<()> {
        // 握手完成前一直占着并发名额。空闲等待在循环里释放，避免预连接和
        // 已结束的回合把名额占满；下一条需要转发的消息会重新获取。
        let mut connection_permit = Some(connection_permit);
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
            Arc::clone(&self.idle_downstreams),
        );
        downstream.native_history = NativeResponsesHistory::with_cache(
            Arc::clone(&self.native_history_cache),
            &context.headers,
        );

        loop {
            drop(connection_permit.take());
            let Some(message) = downstream.next_message().await? else {
                break;
            };
            if matches!(message, WebSocketMessage::Text(_)) {
                match acquire_connection_permit_within(
                    &self.connection_limit,
                    REQUEST_BODY_BUDGET_WAIT,
                )
                .await
                {
                    Ok(permit) => connection_permit = Some(permit),
                    Err(ConnectionPermitError::Busy) => {
                        downstream
                            .write_error(
                                503,
                                "router_busy",
                                "Codey 本地路由当前请求过多，请稍后重试".to_string(),
                                None,
                            )
                            .await?;
                        continue;
                    }
                    Err(ConnectionPermitError::Closed) => {
                        anyhow::bail!("等待本地路由连接名额失败");
                    }
                }
            }
            downstream.clear_stream_id();
            match message {
                WebSocketMessage::Text(text) => {
                    let body_budget_permit = match acquire_request_body_budget_within(
                        &self.request_body_budget,
                        text.len(),
                        REQUEST_BODY_BUDGET_WAIT,
                    )
                    .await
                    {
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
                    let request_body_bytes = Some(text.len() as u64);
                    drop(text);
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
                                    request_body_bytes,
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
        let wire_bytes = request.body.len();
        let encoded_body = match decode_responses_request_body(
            &mut request,
            &self.request_body_budget,
        )
        .await
        {
            Ok(body) => body,
            Err(error) if error.is::<RequestBodyTooLarge>() => {
                let too_large = error.downcast_ref::<RequestBodyTooLarge>().unwrap();
                record_router_failure_nonblocking(
                    "local_router_request_too_large",
                    "decode_responses_request_body",
                    error.to_string(),
                    json!({"requestId": current_router_request_id(), "wireBytes": wire_bytes,
                            "decodedBytesAtLeast": too_large.bytes, "limitBytes": MAX_REQUEST_BYTES}),
                );
                self.record_rejected_request(
                    &request,
                    request_kind.label(),
                    413,
                    "request_too_large",
                );
                HttpResponsesDownstream::new(stream)
                    .write_error(413, "request_too_large", error.to_string(), None)
                    .await?;
                return Ok(());
            }
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
        let (encoded_body, parsed_body, body_budget_permit) =
            match parse_responses_request_body(encoded_body, request._body_budget_permit.take())
                .await
            {
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
        request._body_budget_permit = body_budget_permit;
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
            None,
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
        request_body_bytes: Option<u64>,
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
        if let Some(probe) = &probe {
            probe.set_requested_service_tier(body.get("service_tier").and_then(Value::as_str));
            probe.record_request_body(RequestBodySummary::from_responses_body(
                &body,
                request_body_bytes.or_else(|| encoded_body.as_ref().map(|body| body.len() as u64)),
            ));
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
        // Resolve against the current binding, but do not replace it until the
        // request passes local cross-route history validation.
        let (resolved, previous_route) = {
            let bindings = self
                .bindings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let bound_route = bindings.route_for_keys(&binding_keys);
            let resolved =
                snapshot.target_for_request(&model, route_hint.as_deref(), bound_route.as_deref());
            (resolved, bound_route)
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
        let route_changed = previous_route
            .as_deref()
            .is_some_and(|provider_id| provider_id != resolved.provider_id);
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
            if let Some(reason) = resolved.fallback_reason.as_deref() {
                probe.mark_fallback(reason);
            }
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
                resolved
                    .route
                    .official_auth
                    .as_ref()
                    .map(|auth| auth.account_id.as_str()),
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
        let restoring_adapted_history = bridge == ProtocolBridge::NativeResponses
            && has_codey_synthetic_previous_response_id(&body);
        if restoring_adapted_history {
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
        if bridge == ProtocolBridge::NativeResponses
            && route_changed
            && let Err(error) = validate_cross_route_context(&body)
        {
            return downstream
                .write_error(
                    400,
                    "context_not_portable",
                    error.to_string(),
                    Some(&resolved.route),
                )
                .await;
        }
        let discard_opaque_reasoning = route_changed || restoring_adapted_history;
        if bridge == ProtocolBridge::NativeResponses {
            if normalize_native_responses_context(&mut body, discard_opaque_reasoning) {
                body_mutated = true;
                encoded_body = None;
            }
        } else {
            // 历史恢复完成后检查密文任务，避免转换时静默丢失正文。
            if let Err(error) = validate_adapted_agent_payloads(&body) {
                return downstream
                    .write_error(
                        400,
                        "context_not_portable",
                        error.to_string(),
                        Some(&resolved.route),
                    )
                    .await;
            }
            // 第三方线路写入该字段的明文任务仍恢复为可见文本。
            // encoded_body 在非原生线路上只作为大请求异步转换的体积标记，正文本身
            // 以转换结果为准，保留它可以让超大请求继续走异步转换。
            if normalize_encrypted_agent_payloads(&mut body) {
                body_mutated = true;
            }
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
        let replay_reasoning_summary = should_replay_reasoning_text(
            resolved.route.official_account,
            bridge,
            request_kind,
            compacting,
            upstream_url,
        );
        // Chat 转换只保留 reasoning_text，summary 会在转换时消失。DeepSeek
        // 要在首次发送时带上摘要，所以先写回明文。这里不丢弃 encoded_body：
        // 非原生线路只拿它判断是否把转换放到阻塞线程，正文以转换结果为准。
        if bridge == ProtocolBridge::ResponsesToChatCompletions
            && replay_reasoning_summary
            && restore_reasoning_text_from_summary(&mut body)
        {
            body_mutated = true;
        }
        // Codex 不发送输出上限。高推理强度在 Claude 网关上会变成超过 8192 的思考预算，
        // 三种协议都会因此被拒。只在客户端省略该字段时按模型补上；输出预留仍只缩减上下文。
        if !compacting
            && !resolved.route.official_account
            && ensure_omitted_reasoning_output_limit(&mut body, bridge)
        {
            body_mutated = true;
            if bridge == ProtocolBridge::NativeResponses {
                // 原生透传只改写少数顶层字段，新增的输出上限必须整段重编码才会发出。
                encoded_body = None;
            }
        }
        let mut tool_bridge = ResponsesToolBridge::default();
        let offload_conversion = bridge != ProtocolBridge::NativeResponses
            && encoded_body
                .as_ref()
                .is_some_and(|body| body.len() >= REQUEST_JSON_OFFLOAD_BYTES);
        let (body, converted) = if offload_conversion {
            let permit = request._body_budget_permit.take();
            let encoded = encoded_body.take();
            match tokio::task::spawn_blocking(move || {
                let converted = bridge.convert_responses_body(&body);
                (body, converted, encoded, permit)
            })
            .await
            {
                Ok((body, converted, encoded, permit)) => {
                    encoded_body = encoded;
                    request._body_budget_permit = permit;
                    (body, converted)
                }
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
        let mut chat_reasoning_summaries = Vec::new();
        let mut upstream_body = match converted {
            Ok(converted) => {
                if let Some(converted) = converted {
                    drop(body);
                    tool_bridge = converted.tool_bridge;
                    chat_reasoning_summaries = converted.chat_reasoning_summaries;
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
        if !resolved.route.official_account
            && normalize_responses_tool_parameter_roots(&mut upstream_body)
        {
            body_mutated = true;
            encoded_body = None;
        }
        // Codex Desktop 的工具 schema 会把 `$ref` 和 description/type 写在同一层。
        // Moonshot、Kimi 的 Chat Completions 拒绝这种写法，收进 allOf 后语义不变。
        if bridge == ProtocolBridge::ResponsesToChatCompletions
            && upstream_rejects_ref_sibling_keywords(upstream_url)
            && wrap_chat_tool_ref_siblings(&mut upstream_body)
        {
            body_mutated = true;
            encoded_body = None;
        }
        let anthropic_context_1m = bridge == ProtocolBridge::ResponsesToAnthropicMessages
            && strip_anthropic_context_1m_model_field(&mut upstream_body);
        if anthropic_context_1m {
            body_mutated = true;
            encoded_body = None;
        }
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
        // 请求体的模型名已还原为上游模型名，路由提示头里的模型名必须保持一致；
        // HTTP、WebSocket 握手和压缩请求共用这份头。
        align_routing_hint_model(&mut headers, &resolved.upstream_model);
        if anthropic_context_1m {
            if let Some(model) = upstream_body.get("model").and_then(Value::as_str) {
                let model = model.to_string();
                align_routing_hint_model(&mut headers, &model);
            }
            ensure_anthropic_context_1m_beta(&mut headers);
        }
        // Commit the new binding only after the request's route compatibility,
        // payload conversion, and credentials have passed local checks. A
        // rejected switch must leave the prior route available for a retry.
        // 自动复核是独立请求，复用主会话标识时也不能更改主会话的线路。
        let auto_review_request =
            model_id::equal(&resolved.upstream_model, CODEX_AUTO_REVIEW_MODEL)
                || resolved.fallback_reason.as_deref() == Some("auto_review_misc_model");
        if !auto_review_request {
            let refresh_session_binding = route_hint.is_some() && !subagent_request;
            self.bindings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remember(
                    &binding_keys,
                    &resolved.provider_id,
                    refresh_session_binding,
                );
        }
        let cache_key_deleted = resolved
            .route
            .upstream_headers
            .as_ref()
            .is_ok_and(|overrides| {
                [PROMPT_CACHE_KEY_HEADER, PROMPT_CACHE_KEY_COMPAT_HEADER]
                    .iter()
                    .any(|name| overrides.get(*name).is_some_and(HeaderValue::is_empty))
            });
        if bridge == ProtocolBridge::NativeResponses && !cache_key_deleted {
            ensure_native_prompt_cache_key(
                &mut headers,
                &upstream_body,
                &resolved.provider_id,
                upstream_url,
                &resolved.upstream_model,
            );
        }
        let mut lifecycle = request_lifecycle(
            &headers,
            &resolved,
            bridge,
            request_kind,
            stream_requested,
            subagent_request,
        );
        let mut observed = LifecycleDownstream {
            inner: downstream,
            status: None,
            error: None,
        };
        // 重试只留下还没写进首次请求的摘要。没有摘要时不保留这份列表。
        let chat_reasoning_summaries = (matches!(
            bridge,
            ProtocolBridge::NativeResponses | ProtocolBridge::ResponsesToChatCompletions
        ) && request_kind == ResponsesRequestKind::Create
            && !compacting
            && !resolved.route.official_account
            && chat_reasoning_summaries.iter().any(Option::is_some))
        .then_some(chat_reasoning_summaries);
        let result: Result<()> = async {
        let downstream = &mut observed;
        // Every downstream socket owns its upstream WebSocket cache. Subagents
        // therefore keep incremental `previous_response_id` state on their own
        // upstream connection without sharing the main agent's connection.
        if downstream_websocket
            && request_kind == ResponsesRequestKind::Create
            && stream_requested
            && bridge == ProtocolBridge::NativeResponses
        {
            // Lifecycle plugins need response headers, so they skip this
            // attempt. Continuation history is staged on the HTTP fallback.
            // Grok 回包要改工具名和整数参数，不能走原样透传的上游 WebSocket。
            // 历史仍由下面的 HTTP 回退展开。
            if !compacting
                && !lifecycle.is_active()
                && !native_upstream_needs_xai_compat(upstream_url, &resolved.upstream_model)
            {
                let had_previous_response =
                    responses_previous_response_id(&upstream_body).is_some();
                let websocket_attempt = downstream
                    .try_proxy_upstream_websocket(
                        &resolved.route,
                        &headers,
                        &mut upstream_body,
                        discard_opaque_reasoning,
                    )
                    .await?;
                if websocket_attempt == UpstreamWebSocketAttempt::Completed {
                    return Ok(());
                }
                if had_previous_response && responses_previous_response_id(&upstream_body).is_none()
                {
                    // Reconnection may have expanded history before its handshake failed.
                    body_mutated = true;
                    encoded_body = None;
                }
                if resolved.route.supports_websockets
                    && let Some(probe) = downstream.request_log_probe()
                {
                    probe.mark_fallback("websocket_to_http_sse");
                }
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
            // A failed/reconnected WebSocket can restore full history after the
            // first normalization pass. Validate and normalize the exact body
            // that will be sent through the HTTP fallback as well.
            if route_changed && let Err(error) = validate_cross_route_context(&upstream_body) {
                return downstream
                    .write_error(
                        400,
                        "context_not_portable",
                        error.to_string(),
                        Some(&resolved.route),
                    )
                    .await;
            }
            if normalize_native_responses_context(&mut upstream_body, discard_opaque_reasoning) {
                body_mutated = true;
                encoded_body = None;
            }
        }
        // 原生请求可能刚被 WebSocket 回退展开，摘要要在这份最终正文上还原。
        // Chat 已在转换前还原；转换后的 messages 里没有 summary，再扫一次没有结果。
        if bridge == ProtocolBridge::NativeResponses
            && replay_reasoning_summary
            && restore_reasoning_text_from_summary(&mut upstream_body)
        {
            body_mutated = true;
            encoded_body = None;
        }
        let xai_response_fix = if bridge == ProtocolBridge::NativeResponses
            && !resolved.route.official_account
            && native_upstream_needs_xai_compat(upstream_url, &resolved.upstream_model)
        {
            match prepare_xai_native_request(&mut upstream_body) {
                Ok(prepared) => {
                    if prepared.request_changed {
                        body_mutated = true;
                        encoded_body = None;
                    }
                    Some(prepared.response)
                }
                Err(error) => {
                    return downstream
                        .write_error(
                            400,
                            "unsupported_responses_payload",
                            format!(
                                "线路「{}」发往 Grok 前无法整理请求：{error:#}",
                                route_display_name(&resolved.route)
                            ),
                            Some(&resolved.route),
                        )
                        .await;
                }
            }
        } else {
            None
        };
        let upstream_stream_requested = upstream_body
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // insert 保证只有一个 content-type（线路覆盖可能已写入）；在 .headers() 之后
        // 用 .header() 会追加重复值。只在 HTTP 请求上设置，上游 WebSocket 握手已在
        // 此前发起，不携带 content-type。
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let upstream_client = match self.upstream_client(&resolved.route) {
            Ok(client) => client,
            Err(message) => {
                return downstream
                    .write_error(
                        502,
                        "route_configuration_error",
                        message,
                        Some(&resolved.route),
                    )
                    .await;
            }
        };
        // 部分第三方 thinking 线路要求把上一轮的 reasoning 明文原样回传，而 Codex
        // 回放历史时只保留加密字段。DeepSeek 官方地址在首次发送前还原已有摘要。
        // 其它线路仍先按原样发送，只有上游明确报出明文缺失时才回填：有摘要用摘要，
        // 没有摘要才补占位。Chat 重试使用转换时留下的摘要文本，不保留第二份正文。
        // 官方线路沿用加密推理语义，不参与该回退。
        let reasoning_text_retry_allowed = matches!(
            bridge,
            ProtocolBridge::NativeResponses | ProtocolBridge::ResponsesToChatCompletions
        ) && request_kind == ResponsesRequestKind::Create
            && !compacting
            && !resolved.route.official_account;
        // 重发需要同一份请求头和完整请求体，只有可能重发时才保留。
        // 压缩请求不设置 reqwest 总期限:该期限从建连算到响应体读完,会把耗时较长的
        // 压缩中途截断。等待响应头由 response_header_timeout 约束,响应体读取由
        // PreparedUpstreamResponse 的总期限与空闲期限约束。
        let encoded: Bytes = if bridge == ProtocolBridge::NativeResponses {
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
                    let (body, permit) = rewrite_native_responses_encoded_body_offloaded(
                        body,
                        &upstream_body,
                        request._body_budget_permit.take(),
                    )
                    .await?;
                    request._body_budget_permit = permit;
                    body
                }
                None => serde_json::to_vec(&upstream_body)
                    .context("序列化 Responses WebSocket 上游请求失败")?,
            };
            if let Some(probe) = downstream.request_log_probe() {
                probe.record_upstream_body(RequestBodySummary::from_responses_body(
                    &upstream_body,
                    Some(passthrough_body.len() as u64),
                ));
            }
            passthrough_body.into()
        } else {
            drop(encoded_body.take());
            serde_json::to_vec(&upstream_body).context("序列化转换后的上游请求失败")?.into()
        };
        // 重发只保留紧凑的编码副本。解析出的 JSON 树在发出前丢掉，
        // 预算从 4 倍工作集收回到这一份正文。
        let retryable_encoded = reasoning_text_retry_allowed.then(|| encoded.clone());
        drop(upstream_body);
        drop(std::mem::take(&mut request.body));
        let mut admission = request._body_budget_permit.take();
        retain_compact_request_budget(&mut admission, encoded.len());
        let retained_after_upload = if retryable_encoded.is_some() || lifecycle.is_active() {
            encoded.len()
        } else {
            0
        };
        let response_header_timeout = if compacting {
            // 流式压缩在生成期间持续返回事件，非流式压缩要到生成结束后才返回
            // 响应头，两者的等待期限不同，都只约束响应头。
            if upstream_stream_requested {
                COMPACTION_RESPONSE_HEADER_TIMEOUT
            } else {
                UPSTREAM_NON_STREAM_RESPONSE_HEADER_TIMEOUT
            }
        } else if upstream_stream_requested {
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
        let mut attempt = 0;
        let response_result = send_lifecycle_http(
            downstream, &mut lifecycle, &upstream_client, upstream_url,
            &mut headers, encoded, &mut || {
                retain_compact_request_budget(&mut admission, retained_after_upload);
            }, &mut attempt, response_header_timeout,
        ).await?;
        let Some(response) = Self::finish_upstream_http_send(
            downstream,
            response_result,
            response_header_timeout,
            &resolved,
            bridge,
            request_kind,
            upstream_stream_requested,
            compacting,
        )
        .await?
        else {
            return Ok(());
        };
        let mut upstream_status = response.status().as_u16();
        let mut upstream_request_id = upstream_request_id_from_headers(response.headers());
        if let Some(probe) = downstream.request_log_probe() {
            probe.set_upstream_response_headers(&format_upstream_response_headers(
                response.headers(),
            ));
        }
        let mut upstream_response = Some(response);
        let mut preloaded_error_body = None;
        if upstream_status == 400
            && reasoning_text_retry_allowed && attempt == 0
        {
            let response = upstream_response
                .take()
                .expect("first upstream response is still held here");
            let probe = downstream.request_log_probe().cloned();
            // 错误正文与成功正文共用同一个总期限：只有每次读取的空闲期限时，
            // 上游每隔不到期限发送少量数据就能永久占用请求与压缩会话锁。
            let deadline = upstream_response_body_deadline();
            let body = match await_upstream(
                downstream,
                read_bounded_upstream_error_body(response, probe.as_ref(), deadline),
            )
            .await
            {
                Ok(Ok(body)) => body,
                Ok(Err(error)) | Err(error) if is_upstream_timeout_error(&error) => {
                    write_upstream_error_body_timeout(downstream, &resolved, compacting, &error)
                        .await?;
                    return Ok(());
                }
                Ok(Err(error)) | Err(error) => return Err(error),
            };
            let retryable_body = match retryable_encoded.as_deref() {
                Some(encoded)
                    if requires_reasoning_text_fallback(&body)
                        && grow_request_body_budget(
                            &mut admission,
                            &self.request_body_budget,
                            encoded.len(),
                        )
                        .is_ok() =>
                {
                    let mut retryable_body = serde_json::from_slice::<Value>(encoded)
                        .context("解析 reasoning 回退请求失败")?;
                    fill_missing_reasoning_text_with_chat_summaries(
                        &mut retryable_body,
                        chat_reasoning_summaries.as_deref().unwrap_or(&[]),
                    )
                    .then_some(retryable_body)
                }
                _ => None,
            };
            if let Some(retryable_body) = retryable_body {
                let encoded = serde_json::to_vec(&retryable_body)
                    .context("序列化补齐 reasoning 明文的 Responses 请求失败")?;
                if let Some(probe) = downstream.request_log_probe() {
                    probe.mark_fallback("reasoning_text_placeholder_retry");
                    probe.mark_upstream_send(if upstream_stream_requested {
                        UpstreamTransport::HttpSse
                    } else {
                        UpstreamTransport::Http
                    });
                    probe.record_upstream_body(RequestBodySummary::from_responses_body(
                        &retryable_body,
                        Some(encoded.len() as u64),
                    ));
                }
                let retry_retained = if lifecycle.is_active() {
                    encoded.len()
                } else {
                    0
                };
                attempt += 1;
                let retry_result = send_lifecycle_http(
                    downstream, &mut lifecycle, &upstream_client, upstream_url,
                    &mut headers, encoded.into(), &mut || {
                        retain_compact_request_budget(&mut admission, retry_retained);
                    }, &mut attempt, response_header_timeout,
                ).await?;
                let Some(retried) = Self::finish_upstream_http_send(
                    downstream,
                    retry_result,
                    response_header_timeout,
                    &resolved,
                    bridge,
                    request_kind,
                    upstream_stream_requested,
                    compacting,
                )
                .await?
                else {
                    return Ok(());
                };
                upstream_status = retried.status().as_u16();
                upstream_request_id = upstream_request_id_from_headers(retried.headers());
                if let Some(probe) = downstream.request_log_probe() {
                    probe.set_upstream_response_headers(&format_upstream_response_headers(
                        retried.headers(),
                    ));
                }
                upstream_response = Some(retried);
            } else {
                preloaded_error_body = Some(body);
            }
        }
        // 重发已经结束。工具名映射不保留请求正文，响应内存在另一份预算里。
        drop(retryable_encoded);
        drop(admission);
        drop(std::mem::take(&mut request.body));
        if let Some(probe) = downstream.request_log_probe() {
            probe.mark_upstream_headers(upstream_status, upstream_request_id.as_deref());
        }
        // 首次错误正文已经读完且没有重发时，直接把它写回下游。
        let Some(response) = upstream_response else {
            return write_upstream_http_error(
                downstream,
                upstream_status,
                upstream_request_id.as_deref(),
                preloaded_error_body.as_deref().unwrap_or_default(),
                &resolved,
                bridge,
                request_kind,
            )
            .await;
        };
        let result = match bridge {
            // Every upstream protocol surfaces its real HTTP status. Mapping
            // Anthropic 4xx to 502 made Codex retry non-retryable failures.
            _ if !response.status().is_success() => {
                let probe = downstream.request_log_probe().cloned();
                let deadline = upstream_response_body_deadline();
                // 错误正文和成功正文共用同一个总期限；读取超时时返回结构化
                // 504，语义与压缩路径一致，下游不会只看到连接断开。
                match await_upstream(
                    downstream,
                    read_bounded_upstream_error_body(response, probe.as_ref(), deadline),
                )
                .await
                {
                    Ok(Ok(body)) => {
                        write_upstream_http_error(
                            downstream,
                            upstream_status,
                            upstream_request_id.as_deref(),
                            &body,
                            &resolved,
                            bridge,
                            request_kind,
                        )
                        .await
                    }
                    Ok(Err(error)) | Err(error) if is_upstream_timeout_error(&error) => {
                        write_upstream_error_body_timeout(downstream, &resolved, compacting, &error)
                            .await
                    }
                    Ok(Err(error)) | Err(error) => Err(error),
                }
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
            _ => {
                if let Some(fix) = xai_response_fix {
                    XAI_RESPONSE_FIX
                        .scope(fix, downstream.proxy_response(response))
                        .await
                } else {
                    downstream.proxy_response(response).await
                }
            }
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
            // 取完整错误链，最外层信息不足以定位传输层原因。
            let detail = sanitize_upstream_error_text(&format!("{error:#}"), &resolved.route, 512)
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
        }.await;
        let result = match result {
            Err(error) if error.is::<crate::codey_plugins::lifecycle::LifecycleError>() => {
                let error = error
                    .downcast::<crate::codey_plugins::lifecycle::LifecycleError>()
                    .expect("checked lifecycle error type");
                observed
                    .write_error(
                        error.status,
                        &error.code,
                        error.message,
                        Some(&resolved.route),
                    )
                    .await
            }
            result => result,
        };
        observed.finish(&mut lifecycle, &result);
        result
    }

    /// 把上游 HTTP 发送结果转换成可用的响应；传输层失败会记录并写回下游，
    /// 返回 None 表示调用方直接结束请求。
    #[allow(clippy::too_many_arguments)]
    async fn finish_upstream_http_send<D>(
        downstream: &mut D,
        response_result: std::result::Result<
            std::result::Result<reqwest::Response, reqwest::Error>,
            tokio::time::error::Elapsed,
        >,
        response_header_timeout: Duration,
        resolved: &RouteSelection,
        bridge: ProtocolBridge,
        request_kind: ResponsesRequestKind,
        upstream_stream_requested: bool,
        compacting: bool,
    ) -> Result<Option<reqwest::Response>>
    where
        D: ResponsesDownstream + ?Sized,
    {
        let response = match response_result {
            Ok(Ok(response)) => response,
            Ok(Err(error)) if compacting && error.is_timeout() => {
                let detail = sanitize_upstream_error_text(
                    &error.without_url().to_string(),
                    &resolved.route,
                    256,
                )
                .unwrap_or_else(|| "上游未在期限内返回响应头".to_string());
                downstream
                    .write_error(
                        504,
                        "compaction_timeout",
                        format!(
                            "远程压缩等待上游响应超时（{detail}），原始会话历史未被 Codey 修改，请稍后重试"
                        ),
                        Some(&resolved.route),
                    )
                    .await?;
                return Ok(None);
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
                return Ok(None);
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
                if compacting {
                    // 压缩的等待期限与普通请求不同，失败保持压缩专用的结构化
                    // 错误码，客户端据此显示可重试的压缩超时提示。
                    downstream
                        .write_error(
                            504,
                            "compaction_timeout",
                            format!(
                                "远程压缩等待上游 {} 返回响应头超时，原始会话历史未被 Codey 修改，请稍后重试",
                                resolved.route.upstream_authority
                            ),
                            Some(&resolved.route),
                        )
                        .await?;
                } else {
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
                }
                return Ok(None);
            }
        };
        Ok(Some(response))
    }
}

pub(crate) fn format_upstream_headers(headers: &reqwest::header::HeaderMap) -> String {
    headers
        .iter()
        .map(|(name, value)| {
            let sensitive = value.is_sensitive() || is_sensitive_upstream_header(name.as_str());
            format!(
                "{name}: {}",
                if sensitive {
                    "[REDACTED]"
                } else {
                    value.to_str().unwrap_or("<binary>")
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 上游响应头的日志文本。沿用请求头的脱敏规则，并额外隐藏响应侧的
/// 会话 Cookie。
pub(crate) fn format_upstream_response_headers(headers: &reqwest::header::HeaderMap) -> String {
    headers
        .iter()
        .map(|(name, value)| {
            let sensitive = value.is_sensitive()
                || is_sensitive_upstream_header(name.as_str())
                || is_sensitive_upstream_response_header(name.as_str());
            format!(
                "{name}: {}",
                if sensitive {
                    "[REDACTED]"
                } else {
                    value.to_str().unwrap_or("<binary>")
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_sensitive_upstream_response_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("set-cookie") || name.eq_ignore_ascii_case("set-cookie2")
}
