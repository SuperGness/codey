use super::*;

impl RouterServer {
    pub(crate) async fn handle_connection(&self, mut stream: TcpStream) -> Result<()> {
        // 协议探测和读取请求头共用这一次超时。慢客户端不能先占满探测窗口，
        // 再单独占满读头窗口。
        let header_started = Instant::now();
        match probe_responses_websocket(&stream).await? {
            ResponsesWebSocketProbe::Upgrade => {
                return self.handle_responses_websocket(stream).await;
            }
            ResponsesWebSocketProbe::Http => {}
            ResponsesWebSocketProbe::Silent => {
                // 空闲或半开连接不会发出请求，和普通 HTTP 路径的读取超时一样
                // 回一个 408 即可；对端可能已经断开，这里只做尽力回复。
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
        if pending.request.method == "GET" && pending.request.path == REQUEST_LOG_ICON_PATH {
            write_static_response(&mut stream, "image/png", REQUEST_LOG_ICON).await?;
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
            ("GET", "/codey/concurrency") => {
                write_json_response(&mut stream, 200, &self.official_concurrency.status()).await?;
            }
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
            // 系统浏览器里的请求日志页通过本地路由获取账号目录，用于账号筛选和
            // 账号名显示。这里只读取账号文档，避免只读页面同步 Codex 登录状态。
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
                let (backend, model_aliases) = {
                    let snapshot = self
                        .snapshot
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let aliases = if statistics {
                        crate::route_request_log::RouteRequestLogModelAliases::new(
                            snapshot
                                .aliases
                                .iter()
                                .map(|(alias, target)| (alias.clone(), target.model.clone())),
                            snapshot.raw_models.keys().cloned(),
                            snapshot.model_alias_history.clone(),
                        )
                    } else {
                        Default::default()
                    };
                    (snapshot.request_log_backend, aliases)
                };
                let root = self.request_log.root().to_path_buf();
                match tokio::task::spawn_blocking(move || {
                    let value = if statistics {
                        serde_json::to_value(
                            crate::route_request_log::query_route_request_log_stats_with_model_aliases(
                                &root, backend, query, model_aliases,
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
            ("POST", "/v1/images/generations")
            | ("POST", "/images/generations")
            | ("POST", "/v1/images/edits")
            | ("POST", "/images/edits") => {
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
        let editing = request.path.ends_with("/images/edits");
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
        let snapshot = Arc::clone(
            &self
                .snapshot
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let image_model = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let Some(probe) = &probe {
            probe.record_request_model(&image_model, snapshot.request_model_name(&image_model));
        }
        let explicit_model_alias = snapshot.active_model_alias(&image_model);
        let (route_hint, mut body_mutated) =
            match take_codey_route_metadata(&mut request, &mut body, explicit_model_alias) {
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
        let mut route_decision = crate::route_request_log::RouteDecision::default();
        let resolved = snapshot.route_for_image_request_observed(
            &image_model,
            route_hint.as_deref(),
            probe
                .as_ref()
                .filter(|probe| probe.records_diagnostics())
                .map(|_| &mut route_decision),
        );
        if let Some(probe) = &probe {
            probe.record_route_decision(route_decision);
        }
        let resolved = match resolved {
            Ok(resolved) => resolved,
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
        let route = resolved.route;
        if image_model != resolved.upstream_model {
            body["model"] = Value::String(resolved.upstream_model.clone());
            body_mutated = true;
        }
        if let Some(probe) = &probe {
            probe.resolve_route(
                &route.provider_id,
                &route.route_name,
                route
                    .official_auth
                    .as_ref()
                    .map(|auth| auth.account_id.as_str()),
                &image_model,
                &resolved.upstream_model,
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
        let mut upstream_url = match image_generation_endpoint(upstream_base_url) {
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
        if editing {
            upstream_url = format!("{}/edits", upstream_url.trim_end_matches("/generations"));
        }
        let mut admission_downstream = HttpResponsesDownstream::new(stream);
        let admission = {
            let mut observed =
                ObservedResponsesDownstream::new(&mut admission_downstream, probe.clone());
            self.admit_official(&route, &mut observed).await
        };
        stream = admission_downstream.stream;
        let _official_permit = match admission {
            Ok(permit) => permit,
            Err(error) if error.is::<AdmissionEnded>() => return Ok(()),
            Err(error) => return Err(error),
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
        let operation = if editing {
            codey_plugin_sdk::transport::Operation::ImageEdit
        } else {
            codey_plugin_sdk::transport::Operation::ImageGeneration
        };
        if let Some(probe) = &probe {
            probe.begin_upstream_attempt(
                if stream_requested {
                    UpstreamTransport::HttpSse
                } else {
                    UpstreamTransport::Http
                },
                crate::route_request_log::AttemptTrigger::Initial,
            );
        }
        let response = match send_route_response_headers(
            route.plugin_transport.as_ref(),
            operation,
            &upstream_client,
            &upstream_url,
            &headers,
            request_body,
            response_header_timeout,
            || {},
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                let timeout = error.is_timeout();
                if let Some(probe) = &probe {
                    use crate::route_request_log::{AttemptOutcome, AttemptPhase};
                    probe.finish_upstream_attempt(
                        if timeout {
                            AttemptOutcome::TimedOut
                        } else {
                            AttemptOutcome::Failed
                        },
                        AttemptPhase::ResponseHeaders,
                    );
                }
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
                if let Some(probe) = &probe {
                    probe.finish_upstream_attempt(
                        crate::route_request_log::AttemptOutcome::TimedOut,
                        crate::route_request_log::AttemptPhase::ResponseHeaders,
                    );
                }
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
            if result.as_ref().err().is_some_and(is_upstream_timeout_error) {
                probe.finish_upstream_attempt(
                    crate::route_request_log::AttemptOutcome::TimedOut,
                    crate::route_request_log::AttemptPhase::ResponseBody,
                );
            }
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
        self.request_log
            .begin(|producer| {
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
            .inspect(|probe| probe.set_subagent(request_is_subagent(request)))
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
                | "/v1/images/edits"
                | "/images/edits"
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
        if let Some(target) = &route.plugin_transport {
            return plugin_transport::account_headers(target)
                .await
                .map_err(|message| (401, "plugin_account_unavailable", message));
        }
        if route.official_account {
            if let Some(auth) = route.official_auth.as_ref() {
                validate_managed_official_account(auth)
                    .await
                    .map_err(|error| (401, "official_account_unavailable", format!("{error:#}")))?;
            }
            if let Some((siwc_path, account_id)) = route.official_auth.as_ref().and_then(|auth| {
                auth.siwc_path
                    .as_deref()
                    .zip(Some(auth.account_id.as_str()))
            }) {
                // SIWC 直连平面：令牌来自账号的 SIWC 槽位，不接受下游自带的
                // Codex 令牌；backend-api 专属头一律剥掉。
                let siwc_auth = resolve_siwc_upstream_auth(
                    siwc_path,
                    account_id,
                    &self.siwc_auth_cache,
                    &self.client,
                    &self.proxied_clients,
                )
                .await
                .map_err(|error| (401, "siwc_auth_unavailable", format!("{error:#}")))?;
                let value = HeaderValue::from_str(&siwc_auth.authorization).map_err(|_| {
                    (
                        401,
                        "siwc_auth_invalid",
                        "官方直连凭据无效，请在账号卡片重新授权".to_string(),
                    )
                })?;
                headers.insert(AUTHORIZATION, value);
                strip_siwc_forbidden_headers(&mut headers);
            } else {
                let official_auth = match &route.official_auth {
                    Some(auth) => {
                        resolve_managed_official_upstream_auth(
                            request,
                            &self.bearer_token,
                            auth,
                            &self.official_auth_cache,
                        )
                        .await
                    }
                    None => {
                        resolve_official_upstream_auth(
                            request,
                            &self.bearer_token,
                            &self.official_auth_path,
                            &self.official_auth_cache,
                            true,
                        )
                        .await
                    }
                };
                let official_auth = official_auth.ok_or_else(|| {
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
        // 连接总预算覆盖握手与空闲阶段，生成执行预算在每轮选路后获取。
        let handshake_context = Arc::new(Mutex::new(None));
        let captured_context = Arc::clone(&handshake_context);
        let token = self.token.clone();
        let bearer_token = self.bearer_token.clone();
        let request_id = current_router_request_id();
        let websocket_config = downstream_websocket_config();
        let socket = tokio::time::timeout(
            REQUEST_READ_TIMEOUT,
            accept_hdr_async_with_config(
                WebSocketIngress::new(stream, Arc::clone(&self.request_body_budget)),
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
            BudgetedWebSocket::new(socket),
            Arc::clone(&self.websocket_backoffs),
            Arc::clone(&self.request_body_budget),
            Arc::clone(&self.idle_downstreams),
        );
        downstream.native_history = NativeResponsesHistory::with_cache(
            Arc::clone(&self.native_history_cache),
            &context.headers,
        );

        loop {
            let Some(message) = downstream.next_message().await? else {
                break;
            };
            if downstream.handle_steering_message(&message).await? {
                downstream.write_steering_notifications().await?;
                continue;
            }
            if downstream.handle_idle_response_interrupt(&message)? {
                continue;
            }
            let BudgetedWebSocketMessage {
                message,
                parsed_body,
                permit: body_budget_permit,
            } = message;
            downstream.clear_stream_id();
            match message {
                WebSocketMessage::Text(text) => {
                    let mut body = match parsed_body.expect("admitted text must have a JSON result")
                    {
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
                    let message_type = body.as_object_mut().and_then(|body| body.remove("type"));
                    if message_type.as_ref().and_then(Value::as_str) != Some("response.create") {
                        // 仅显示有界的协议标识，不把任意字段内容写入错误信息。
                        let received = match message_type.as_ref() {
                            None => "缺少 type 字段".to_string(),
                            Some(Value::String(value))
                                if !value.is_empty()
                                    && value.len() <= 64
                                    && value.bytes().all(|byte| {
                                        byte.is_ascii_alphanumeric()
                                            || matches!(byte, b'.' | b'_' | b'-')
                                    }) =>
                            {
                                format!("收到事件类型 {value}")
                            }
                            Some(Value::String(_)) => "type 不是有效的事件标识".to_string(),
                            Some(_) => "type 字段必须为字符串".to_string(),
                        };
                        downstream
                            .write_error(
                                400,
                                "unsupported_websocket_message",
                                format!(
                                    "Codey Responses WebSocket 支持 response.create、response.steer 和 response.interrupt；{received}"
                                ),
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
                    match downstream
                        .steering
                        .begin_request(&mut body, &self.request_body_budget)
                    {
                        Ok(events) => {
                            for event in events {
                                downstream.write_steering_event(&event).await?;
                            }
                        }
                        Err(error) => {
                            // 这次 create 校验失败，不改变仍在等待工具结果的原响应。
                            let mut event = ResponsesFailure::new(
                                400,
                                "steering_required_input",
                                error.to_string(),
                                None,
                            )
                            .normalized_event();
                            downstream.steering.sequence_response_event(&mut event);
                            downstream
                                .write_text(encode_responses_websocket_event(
                                    &event,
                                    stream_id.as_deref(),
                                )?)
                                .await?;
                            continue;
                        }
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
                            let timeout = is_upstream_timeout_error(&error);
                            downstream
                                .write_error(
                                    if timeout { 504 } else { 502 },
                                    if timeout {
                                        "upstream_timeout"
                                    } else {
                                        "websocket_proxy_failed"
                                    },
                                    format!("Codey 本地路由处理请求失败；请求 ID：{request_id}"),
                                    None,
                                )
                                .await?;
                        }
                    }
                    downstream.write_steering_notifications().await?;
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
        downstream.set_streaming_errors(
            request_kind == ResponsesRequestKind::Create
                && !is_compaction_request(&body, request_kind)
                && body.get("stream").and_then(Value::as_bool).unwrap_or(false),
        );
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
            let requested_model = body
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let snapshot = self
                .snapshot
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            probe.record_request_model(
                requested_model,
                snapshot.request_model_name(requested_model),
            );
            probe.set_subagent(request_is_subagent(&request));
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
        let decision_probe = downstream
            .request_log_probe()
            .filter(|probe| probe.records_diagnostics())
            .cloned();
        let mut route_decision = crate::route_request_log::RouteDecision {
            settings_revision: snapshot.settings_revision,
            ..Default::default()
        };
        let mut portable_attempt =
            match PortableAttempt::acquire(self, &request, compacting, &snapshot) {
                Ok(attempt) => attempt,
                Err(error) => {
                    if let Some(probe) = &decision_probe {
                        route_decision.reason =
                        crate::route_request_log::RouteDecisionReason::PortableHandoffUnavailable;
                        probe.record_route_decision(route_decision);
                    }
                    return downstream
                        .write_error(
                            409,
                            "portable_compaction_unavailable",
                            error.to_string(),
                            None,
                        )
                        .await;
                }
            };
        let model_was_defaulted = requested_model.is_empty();
        route_decision.defaulted_model = model_was_defaulted;
        let model = if model_was_defaulted {
            snapshot.default_model.trim().to_string()
        } else {
            requested_model
        };
        if model.is_empty() {
            if let Some(probe) = &decision_probe {
                route_decision.reason = crate::route_request_log::RouteDecisionReason::MissingModel;
                probe.record_route_decision(route_decision);
            }
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
        let explicit_model_alias = (!model_was_defaulted)
            .then(|| snapshot.active_model_alias(&model))
            .flatten();
        let (route_hint, mut body_mutated) =
            match take_codey_route_metadata(&mut request, &mut body, explicit_model_alias) {
                Ok(extracted) => extracted,
                Err(error) => {
                    if let Some(probe) = &decision_probe {
                        route_decision.reason =
                            crate::route_request_log::RouteDecisionReason::InvalidMetadata;
                        probe.record_route_decision(route_decision);
                    }
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
        let portable_target_metadata = portable_attempt
            .as_ref()
            .and_then(|attempt| attempt.target_metadata(&snapshot));
        if let Some(attempt) = portable_attempt.as_mut() {
            let cache = self
                .portable_history_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .snapshot();
            if attempt.prepare_target(
                &mut body,
                &cache,
                portable_target_metadata.as_ref(),
                &mut request._body_budget_permit,
                &self.request_body_budget,
            ) {
                body_mutated = true;
                encoded_body = None;
            }
        }
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
            let resolved = if let Some(attempt) = &portable_attempt {
                route_decision.reason = if attempt.uses_target {
                    crate::route_request_log::RouteDecisionReason::PortableTargetHandoff
                } else {
                    crate::route_request_log::RouteDecisionReason::PortableSourceHandoff
                };
                route_decision.candidate_count = 1;
                Ok(RouteSelection {
                    provider_id: attempt.route().provider_id.clone(),
                    protocol: attempt.route().protocol,
                    requested_model: model.clone(),
                    route: Arc::clone(attempt.route()),
                    upstream_model: attempt.upstream_model().to_string(),
                    fallback_reason: None,
                })
            } else {
                snapshot.resolve_request_observed(
                    RouteRequest {
                        requested_model: &model,
                        route_hint: route_hint.as_deref(),
                        bound_route: bound_route.as_deref(),
                    },
                    decision_probe.as_ref().map(|_| &mut route_decision),
                )
            };
            (resolved, bound_route)
        };
        if let Some(probe) = decision_probe {
            probe.record_route_decision(route_decision);
        }
        let resolved = match resolved {
            Ok(resolved) => resolved,
            Err(error) => {
                downstream
                    .write_error(404, "model_not_enabled", format!("{error:#}"), None)
                    .await?;
                return Ok(());
            }
        };
        // 只保留亲自观测到的完整输入；压缩响应校验成功后才建立对应关系。
        let portable_checkpoint = if compacting
            && portable_attempt.is_none()
            && resolved.route.supports_remote_compaction
            && resolved.protocol == UpstreamProtocol::OpenAiResponses
        {
            portable_request_key(&request).and_then(|key| {
                let cache = self
                    .portable_history_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .snapshot();
                cache.capture(&key, &resolved.route, &resolved.upstream_model, &body)
            })
        } else {
            None
        };
        let route_changed = previous_route
            .as_deref()
            .is_some_and(|provider_id| provider_id != resolved.provider_id)
            || self
                .bindings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .routes
                .get(binding_keys.first().map(String::as_str).unwrap_or_default())
                .is_some_and(|binding| {
                    !binding.upstream_model.is_empty()
                        && binding.context_config != Some(resolved.route.context_config)
                });
        if model != resolved.upstream_model {
            body.as_object_mut()
                .expect("validated Responses body must remain an object")
                .insert(
                    "model".to_string(),
                    Value::String(resolved.upstream_model.clone()),
                );
            body_mutated = true;
        }
        let bridge = ProtocolBridge::from_upstream_protocol(resolved.protocol);
        // 原生 Responses 上游可能为模型保留 `collaboration` 命名空间并逐字校验 schema，
        // 先改到空闲的普通命名空间，归一化和摘标记才不会再动到保留 schema；
        // 回包由 COLLABORATION_ALIAS 还原。
        let collaboration_alias = (bridge == ProtocolBridge::NativeResponses)
            .then(|| alias_reserved_collaboration_namespace(&mut body))
            .flatten();
        if collaboration_alias.is_some() {
            body_mutated = true;
            encoded_body = None;
        }
        // 没能改名的请求里 `collaboration` 仍是保留名，其内部 schema 必须逐字保留。
        let collaboration_reserved = bridge == ProtocolBridge::NativeResponses
            && body_has_reserved_collaboration_namespace(&body);
        if !resolved.route.official_account && normalize_responses_tool_parameter_roots(&mut body) {
            body_mutated = true;
            encoded_body = None;
        }
        // 官方上游看到协作工具的加密标记就会把任务正文换成只有它能解开的令牌，
        // 第三方子代理线路没有该密钥。摘掉标记让正文以明文返回，跨线路派发才能送达。
        if strip_collaboration_message_encryption(
            &mut body,
            resolved.route.official_account || collaboration_reserved,
        ) {
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
        let native_compaction = compacting
            && portable_attempt.is_none()
            && resolved.route.supports_remote_compaction
            && bridge == ProtocolBridge::NativeResponses;
        let compaction_phase = match portable_attempt.as_ref() {
            Some(attempt) if attempt.uses_target => GatewayCompactionPhase::TargetRouteHandoff,
            Some(_) => GatewayCompactionPhase::SourceRouteHandoff,
            None => GatewayCompactionPhase::GatewaySummary,
        };
        match restore_codey_compaction_summaries(&mut body) {
            Ok(true) => {
                body_mutated = true;
                encoded_body = None;
            }
            Ok(false) => {}
            Err(error) => {
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
        if compacting && !native_compaction {
            let metadata =
                crate::model_catalog::runtime_context_metadata(crate::codex_config::codex_home());
            let model_metadata = portable_target_metadata
                .as_ref()
                .filter(|_| {
                    portable_attempt
                        .as_ref()
                        .is_some_and(|attempt| attempt.uses_target)
                })
                .or_else(|| {
                    gateway_compaction_model_metadata(
                        &metadata,
                        &resolved.provider_id,
                        &resolved.upstream_model,
                    )
                });
            if let Err(error) = gateway_compaction_output_budget(&body, model_metadata)
                .and_then(|budget| prepare_gateway_compaction(&mut body, budget))
            {
                return downstream
                    .write_error(
                        400,
                        "unsupported_responses_payload",
                        error.to_string(),
                        Some(&resolved.route),
                    )
                    .await;
            }
            body_mutated = true;
            encoded_body = None;
        }
        if bridge != ProtocolBridge::NativeResponses
            && let Err(error) = validate_portable_context(&body)
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
            // 展开恢复的历史里仍带着 Codex 侧的 `collaboration` 调用与工具声明，
            // 与请求初次改名后的命名空间保持一致。
            if collaboration_alias
                .as_ref()
                .is_some_and(|alias| alias.alias_history(&mut body))
            {
                encoded_body = None;
            }
        }
        match restore_codey_compaction_summaries(&mut body) {
            Ok(true) => {
                body_mutated = true;
                encoded_body = None;
            }
            Ok(false) => {}
            Err(error) => {
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
        // Only a full, restored request can establish portability. An unresolved
        // previous_response_id carries history that is absent from this input.
        let history_portable = portable_compaction::resolved_history_portability(&body);
        let discard_opaque_reasoning = route_changed || restoring_adapted_history;
        if bridge == ProtocolBridge::NativeResponses {
            if normalize_native_responses_context(&mut body, discard_opaque_reasoning) {
                body_mutated = true;
                encoded_body = None;
            }
            if resolved.route.official_account && sanitize_official_upstream_history(&mut body) {
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
        let compact_endpoint_compat = native_compaction
            && request_kind == ResponsesRequestKind::Create
            && !resolved.route.official_account
            && resolved.route.remote_compaction_protocol
                == crate::config::RemoteCompactionProtocol::CompactEndpoint;
        let upstream_url = match (request_kind, compact_endpoint_compat) {
            (ResponsesRequestKind::Compact, _) if native_compaction => {
                &resolved.route.upstream_compact_url
            }
            (_, true) => &resolved.route.upstream_compact_url,
            _ => &resolved.route.upstream_url,
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
            // 原生透传的顶层改写包含输出上限，原始字节仍可复用。
            body_mutated = true;
        }
        let PreparedProtocolRequest {
            body: mut upstream_body,
            tool_bridge,
            chat_reasoning_summaries,
            anthropic_context_1m,
        } = match PreparedProtocolRequest::prepare(
            body,
            bridge,
            &resolved.route,
            upstream_url,
            &mut encoded_body,
            &mut body_mutated,
            &mut request._body_budget_permit,
        )
        .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                return downstream
                    .write_error(
                        error.status,
                        error.code,
                        error.message,
                        Some(&resolved.route),
                    )
                    .await;
            }
        };
        let _official_permit = match self.admit_official(&resolved.route, downstream).await {
            Ok(permit) => permit,
            Err(error) if error.is::<AdmissionEnded>() => return Ok(()),
            Err(error) => return Err(error),
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
        // Ordinary requests commit after compatibility, conversion and credentials
        // pass local checks. Native compaction waits for validated complete delivery;
        // pending or rejected compaction must keep the original route identity.
        // 自动复核是独立请求，复用主会话标识时也不能更改主会话的线路。
        let auto_review_request =
            model_id::equal(&resolved.upstream_model, CODEX_AUTO_REVIEW_MODEL)
                || resolved.fallback_reason.as_deref() == Some("auto_review_misc_model");
        let mut native_compaction_binding = None;
        if !auto_review_request && portable_attempt.is_none() {
            let refresh_session_binding = !subagent_request
                && incoming_header(&request, "x-codex-parent-thread-id").is_none()
                && (route_hint.is_some() || incoming_header(&request, "thread-id").is_none());
            if native_compaction {
                native_compaction_binding = Some(PendingNativeCompactionBinding::prepare(
                    &self.bindings,
                    &binding_keys,
                    &resolved,
                    refresh_session_binding,
                ));
            } else {
                self.bindings
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remember_resolved(
                        &binding_keys,
                        &resolved,
                        refresh_session_binding,
                        false,
                        history_portable,
                    );
            }
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
        let (result, stream_failure) = COLLABORATION_ALIAS.scope(collaboration_alias.clone(), CURRENT_LIFECYCLE_FAILURE.scope(std::cell::RefCell::new(None), async {
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
                let fallback_reason = match websocket_attempt {
                    UpstreamWebSocketAttempt::Completed => return Ok(()),
                    UpstreamWebSocketAttempt::UseHttp(reason) => reason,
                };
                if had_previous_response && responses_previous_response_id(&upstream_body).is_none()
                {
                    // Reconnection may have expanded history before its handshake failed.
                    body_mutated = true;
                    encoded_body = None;
                }
                if resolved.route.supports_websockets
                    && let Some(probe) = downstream.request_log_probe()
                {
                    probe.mark_fallback(fallback_reason);
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
            // `generate` 是 Responses WebSocket 的传输层字段:false 表示预热
            // 请求,只暂存上下文换取续接用的 response_id,不要求产出内容。
            // HTTP Responses 不认识该参数,剥掉再发又会把预热变成一次真实
            // 生成,所以回退 HTTP 时由本地应答空响应;其他取值只剥除字段。
            // 应答的 response_id 沿用上游形态:已暂存的历史记在这个 id 下,
            // 客户端续接时由原生历史展开;resp_codey_ 前缀会误触发旧线路
            // 历史恢复,不能用。
            match upstream_body
                .as_object_mut()
                .and_then(|object| object.remove("generate"))
            {
                Some(Value::Bool(false)) => {
                    let model = upstream_body
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or(resolved.upstream_model.as_str());
                    let response = json!({
                        "id": format!("resp_{}", Uuid::new_v4().simple()),
                        "object": "response",
                        "status": "completed",
                        "model": model,
                        "output": [],
                        "usage": {"input_tokens":0,"output_tokens":0,"total_tokens":0},
                    });
                    return downstream.write_json(200, &response).await;
                }
                Some(_) => {
                    body_mutated = true;
                    encoded_body = None;
                }
                None => {}
            }
            // A failed/reconnected WebSocket can restore full history after the
            // first normalization pass. Validate and normalize the exact body
            // that will be sent through the HTTP fallback as well.
            match restore_codey_compaction_summaries(&mut upstream_body) {
                Ok(true) => { body_mutated = true; encoded_body = None; }
                Ok(false) => {}
                Err(error) => return downstream.write_error(400, "context_not_portable", error.to_string(), Some(&resolved.route)).await,
            }
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
            if resolved.route.official_account && sanitize_official_upstream_history(&mut upstream_body) {
                body_mutated = true;
                encoded_body = None;
            }
            // 恢复的历史沿用 Codex 侧的 `collaboration` 调用，与改名后的工具保持一致。
            if collaboration_alias
                .as_ref()
                .is_some_and(|alias| alias.alias_history(&mut upstream_body))
            {
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
        if compact_endpoint_compat {
            if let Err(error) = adapt_compaction_trigger_to_compact(&mut upstream_body) {
                return downstream
                    .write_error(
                        400,
                        "unsupported_compaction_payload",
                        error.to_string(),
                        Some(&resolved.route),
                    )
                    .await;
            }
            body_mutated = true;
            encoded_body = None;
            headers.insert(
                reqwest::header::ACCEPT,
                HeaderValue::from_static("application/json"),
            );
            if let Some(probe) = downstream.request_log_probe() {
                probe.mark_fallback("compaction_compact_endpoint");
            }
        }
        let xai_response_fix = if !compacting
            && bridge == ProtocolBridge::NativeResponses
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
        // SIWC 直连只接受公开 Responses 字段：续接状态、采样参数和托管工具
        // 在这里剥掉。净化改写正文就必须整段重编码，透传字节里残留的字段
        // 不能再发出去。
        if resolved.route.uses_siwc()
            && sanitize_siwc_responses_request(&mut upstream_body)
        {
            body_mutated = true;
            encoded_body = None;
        }
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
        let encoded = RequestEncoding {
            bridge,
            requested_model: &model,
            upstream_model: &resolved.upstream_model,
            body_mutated,
        }.encode(&mut request, upstream_body, encoded_body.take(), downstream.request_log_probe()).await?;
        let execution = HttpExecution {
            server: self,
            resolved: &resolved,
            bridge,
            request_kind,
            compacting,
            native_compaction,
            compaction_phase,
            upstream_url,
            upstream_stream_requested,
        };
        let Some(received) = execution.send(
            downstream, &mut lifecycle, &upstream_client, &mut headers, encoded,
            chat_reasoning_summaries.as_deref().unwrap_or(&[]),
        ).await? else {
            return Ok(());
        };
        ResponseDelivery {
            resolved: &resolved,
            bridge,
            request_kind,
            stream_requested,
            compacting,
            native_compaction,
            compact_endpoint_compat,
            compaction_phase,
            tool_bridge: &tool_bridge,
        }.deliver(
            downstream, received, portable_attempt.as_mut(),
            portable_checkpoint.map(|checkpoint| (checkpoint, Arc::clone(&self.portable_history_cache))),
            xai_response_fix,
        ).await
        }.await;
        (result, CURRENT_LIFECYCLE_FAILURE.with(|failure| failure.borrow_mut().take()))
        })).await;
        if let Some((status, code)) = stream_failure {
            observed.status = Some(status);
            observed.error = Some(code);
        }
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
        if result.is_ok()
            && observed.error.is_none()
            && observed
                .status
                .is_some_and(|status| (200..300).contains(&status))
            && let Some(binding) = native_compaction_binding
        {
            binding.complete();
        }
        result
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
