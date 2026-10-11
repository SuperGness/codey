use super::*;
use crate::codey_plugins::lifecycle::{
    LifecycleRequest, LifecycleStage, TestPlugin, has_plugins, with_test_plugins,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
struct CapturedDownstream {
    status: Option<u16>,
    body: Vec<u8>,
    delivered: Arc<AtomicBool>,
    websocket: bool,
    websocket_attempts: usize,
    fallback_attempts: usize,
    cancel_at: Option<tokio::time::Instant>,
}

#[async_trait]
impl ResponsesDownstream for CapturedDownstream {
    fn is_websocket(&self) -> bool {
        self.websocket
    }
    async fn wait_for_upstream<T, F>(&mut self, future: F) -> Result<T>
    where
        T: Send,
        F: std::future::Future<Output = T> + Send,
    {
        if let Some(deadline) = self.cancel_at {
            tokio::select! { value = future => Ok(value), _ = tokio::time::sleep_until(deadline) => Err(DownstreamClosed.into()) }
        } else {
            Ok(future.await)
        }
    }
    fn prepare_native_http_fallback(
        &mut self,
        _: &RouteTarget,
        _: &HeaderMap,
        _: &mut Value,
    ) -> Result<bool> {
        self.fallback_attempts += 1;
        Ok(false)
    }
    async fn write_error(
        &mut self,
        status: u16,
        code: &str,
        _: String,
        _: Option<&RouteTarget>,
    ) -> Result<()> {
        self.status = Some(status);
        self.body = code.as_bytes().to_vec();
        Ok(())
    }
    async fn write_json(&mut self, status: u16, value: &Value) -> Result<()> {
        self.status = Some(status);
        self.body = serde_json::to_vec(value)?;
        Ok(())
    }
    async fn start_event_stream(&mut self) -> Result<()> {
        Ok(())
    }
    async fn write_event(&mut self, _: &Value) -> Result<()> {
        Ok(())
    }
    async fn finish_event_stream(&mut self) -> Result<()> {
        Ok(())
    }
    async fn proxy_response_with_probe(
        &mut self,
        response: reqwest::Response,
        _: Option<&RouteRequestLogProbe>,
    ) -> Result<()> {
        self.delivered.store(true, Ordering::SeqCst);
        self.status = Some(response.status().as_u16());
        self.body = response.bytes().await?.to_vec();
        Ok(())
    }
    async fn try_proxy_upstream_websocket_with_probe(
        &mut self,
        _: &RouteTarget,
        _: &HeaderMap,
        _: &mut Value,
        _: bool,
        _: Option<&RouteRequestLogProbe>,
    ) -> Result<UpstreamWebSocketAttempt> {
        self.websocket_attempts += 1;
        Ok(UpstreamWebSocketAttempt::Completed)
    }
}

fn test_server(config: &CodeyConfig) -> RouterServer {
    RouterServer {
        endpoint: RuntimeRouterEndpoint {
            base_url: "http://127.0.0.1:9/v1".into(),
            token: "test-router".into(),
            supports_websockets: false,
            supports_remote_compaction: false,
            requires_openai_auth: false,
        },
        token: "test-router".into(),
        bearer_token: "Bearer test-router".into(),
        snapshot: Arc::new(RwLock::new(Arc::new(RouterSnapshot::from_config(config)))),
        connection_limit: Arc::new(Semaphore::new(4)),
        rejection_limit: Arc::new(Semaphore::new(4)),
        request_body_budget: Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS)),
        bindings: Arc::default(),
        websocket_backoffs: Arc::default(),
        native_history_cache: Arc::default(),
        idle_downstreams: Arc::default(),
        client: super::server::upstream_http_client_builder()
            .no_proxy()
            .build()
            .unwrap(),
        proxied_clients: Mutex::default(),
        official_auth_path: PathBuf::from("/nonexistent/codey-lifecycle-test-auth"),
        account_usage_cache: Arc::default(),
        official_auth_cache: Arc::default(),
        request_log: Arc::new(RouteRequestLogController::new()),
    }
}

fn official_history_server(url: String) -> (RouterServer, String) {
    let mut profile = crate::config::ProviderProfile::new("Official test");
    profile.id = crate::config::DERIVED_OFFICIAL_PROFILE_ID.into();
    profile.source_provider_id = Some("openai".into());
    profile.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.into();
    profile.base_url = url;
    profile.supports_remote_compaction = true;
    profile.normalize();
    let mut config = CodeyConfig {
        profiles: vec![profile],
        official_account_available_this_launch: true,
        ..CodeyConfig::default()
    };
    config
        .selected_models_by_provider
        .insert("openai".into(), vec!["test-model".into()]);
    (test_server(&config), model_alias("openai", "test-model"))
}

fn foreign_history_items() -> Value {
    json!([
        {"type":"reasoning","id":"item_summary","summary":[{"type":"summary_text","text":"preserved reasoning"}],"encrypted_content":"foreign-state"},
        {"type":"message","id":"item_message","role":"assistant","content":[{"type":"output_text","text":"visible answer"}],"status":"completed"},
        {"type":"function_call","id":"item_tool","call_id":"call_original","name":"lookup","arguments":"{}"},
        {"type":"function_call_output","call_id":"call_original","output":"tool result"}
    ])
}

fn assert_portable_official_history(items: &[Value]) {
    assert_eq!(
        items[0],
        json!({"type":"reasoning","summary":[{"type":"summary_text","text":"preserved reasoning"}]})
    );
    assert_eq!(
        items[1],
        json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"visible answer"}],"status":"completed"})
    );
    assert_eq!(
        items[2],
        json!({"type":"function_call","call_id":"call_original","name":"lookup","arguments":"{}"})
    );
    assert_eq!(items[3], foreign_history_items()[3]);
}

fn official_history_request() -> HttpRequest {
    HttpRequest {
        method: "POST".into(),
        path: "/v1/responses".into(),
        headers: vec![
            ("authorization".into(), "Bearer test-oauth".into()),
            ("thread-id".into(), "foreign-history".into()),
        ],
        body: Vec::new(),
        _body_budget_permit: None,
    }
}

#[tokio::test]
async fn official_history_http_normalizes_cold_start_and_route_switch() {
    for kind in [ResponsesRequestKind::Create, ResponsesRequestKind::Compact] {
        for route_changed in [false, true] {
            let (url, upstream) = fake_upstream(vec![200]).await;
            let (server, model) = official_history_server(url);
            let request = official_history_request();
            if route_changed {
                server.bindings.lock().unwrap().remember(
                    &request_binding_keys(&request),
                    "previous-provider",
                    false,
                );
            }
            let body = json!({"model":model,"input":foreign_history_items()});
            let encoded = serde_json::to_vec_pretty(&body).unwrap();
            let mut downstream = CapturedDownstream::default();
            tokio::time::timeout(
                Duration::from_secs(5),
                server.proxy_parsed_responses_inner(
                    request,
                    body,
                    Some(encoded),
                    kind,
                    &mut downstream,
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                downstream.status,
                Some(200),
                "{kind:?}, route_changed={route_changed}: {}",
                String::from_utf8_lossy(&downstream.body)
            );
            let requests = tokio::time::timeout(Duration::from_secs(5), upstream)
                .await
                .unwrap()
                .unwrap();
            let sent: Value = serde_json::from_slice(&requests[0].body).unwrap();
            assert_portable_official_history(sent["input"].as_array().unwrap());
        }
    }
}

#[tokio::test]
async fn official_history_sanitization_does_not_change_third_party_requests() {
    let (url, upstream) = fake_upstream(vec![200]).await;
    let (config, provider, model) = super::tests::router_config(url);
    let server = test_server(&config);
    let body = json!({"model":model_alias(&provider, &model),"input":foreign_history_items()});
    let encoded = serde_json::to_vec_pretty(&body).unwrap();
    let mut downstream = CapturedDownstream::default();
    server
        .proxy_parsed_responses_inner(
            official_history_request(),
            body,
            Some(encoded),
            ResponsesRequestKind::Create,
            &mut downstream,
        )
        .await
        .unwrap();
    assert_eq!(downstream.status, Some(200));
    let requests = upstream.await.unwrap();
    let sent: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(sent["input"], foreign_history_items());
}

#[tokio::test]
async fn official_history_is_sanitized_for_websocket_and_opaque_http_continuations() {
    for websocket in [true, false] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let (server, model) =
            official_history_server(format!("http://{}/v1", listener.local_addr().unwrap()));
        let upstream = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let response =
                json!({"id":"resp-second","object":"response","status":"completed","output":[]});
            if websocket {
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let WebSocketMessage::Text(text) = socket.next().await.unwrap().unwrap() else {
                    panic!("expected response.create")
                };
                let request: Value = serde_json::from_str(&text).unwrap();
                socket
                    .send(WebSocketMessage::Text(
                        json!({"type":"response.completed","response":response})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                request
            } else {
                let request = read_http_request(&mut stream).await.unwrap();
                assert_eq!(request.method, "POST");
                write_json_response(&mut stream, 200, &response)
                    .await
                    .unwrap();
                serde_json::from_slice::<Value>(&request.body).unwrap()
            }
        });
        let resolved = server
            .snapshot
            .read()
            .unwrap()
            .target_for_model(&model)
            .unwrap();
        let request = official_history_request();
        let headers = server
            .prepare_upstream_request_headers(&request, &resolved.route)
            .await
            .unwrap();
        let (socket, mut peer) = super::tests::local_websocket_pair().await;
        let mut downstream = WebSocketResponsesDownstream::new(socket);
        let mut original =
            json!({"model":resolved.upstream_model,"stream":true,"input":"original task"});
        downstream
            .prepare_native_http_fallback(&resolved.route, &headers, &mut original)
            .unwrap();
        let mut history = foreign_history_items();
        if websocket {
            history[0]
                .as_object_mut()
                .unwrap()
                .remove("encrypted_content");
        }
        downstream.write_event(&json!({"type":"response.completed","response":{"id":"resp-first","status":"completed","output":history}})).await.unwrap();
        peer.next().await.unwrap().unwrap();
        downstream.clear_stream_id();
        let body = json!({"model":model,"stream":true,"previous_response_id":"resp-first","input":"follow up"});
        let encoded = serde_json::to_vec(&body).unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            server.proxy_parsed_responses_inner(
                request,
                body,
                Some(encoded),
                ResponsesRequestKind::Create,
                &mut downstream,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let sent = tokio::time::timeout(Duration::from_secs(5), upstream)
            .await
            .unwrap()
            .unwrap();
        assert!(sent.get("previous_response_id").is_none());
        let items = sent["input"].as_array().unwrap();
        assert_eq!(items.len(), 6);
        assert_eq!(items[0], json!({"role":"user","content":"original task"}));
        assert_portable_official_history(&items[1..5]);
        assert_eq!(items[5], json!({"role":"user","content":"follow up"}));
    }
}

async fn invoke<D: ResponsesDownstream + ?Sized>(
    server: &RouterServer,
    model: &str,
    stream: bool,
    downstream: &mut D,
) -> Result<()> {
    let body = json!({"model":model,"stream":stream,"input":[{"type":"reasoning","encrypted_content":"test","summary":[]},{"role":"user","content":"test"}]});
    let encoded = serde_json::to_vec_pretty(&body).unwrap();
    let request = HttpRequest {
        method: "POST".into(),
        path: "/v1/responses".into(),
        headers: vec![],
        body: Vec::new(),
        _body_budget_permit: None,
    };
    server
        .proxy_parsed_responses_inner(
            request,
            body,
            Some(encoded),
            ResponsesRequestKind::Create,
            downstream,
        )
        .await
}

async fn fake_upstream(statuses: Vec<u16>) -> (String, tokio::task::JoinHandle<Vec<HttpRequest>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for status in statuses {
            let (mut stream, _) = listener.accept().await.unwrap();
            requests.push(read_http_request(&mut stream).await.unwrap());
            let body = if status == 400 {
                json!({"error":{"message":"The `reasoning_text` in the thinking mode must be passed back to the API.","code":"invalid_request_error"}})
            } else if requests
                .last()
                .unwrap()
                .path
                .ends_with("/responses/compact")
            {
                json!({"id":"test-compaction","object":"response.compaction","output":[{"type":"compaction","id":"cmp_test","encrypted_content":"test-state"}]})
            } else {
                json!({"id":"test-response","object":"response","status":"completed","output":[]})
            };
            write_json_response(&mut stream, status, &body)
                .await
                .unwrap();
        }
        requests
    });
    (url, task)
}

fn recording_plugin(
    callback: impl Fn(&str, &Value) -> Value + Send + Sync + 'static,
) -> (TestPlugin, mpsc::UnboundedReceiver<(String, Value)>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
        TestPlugin::new("router-test", move |method, params| {
            let answer = callback(method, &params);
            tx.send((method.to_owned(), params)).unwrap();
            Ok(answer)
        }),
        rx,
    )
}

async fn terminal(rx: &mut mpsc::UnboundedReceiver<(String, Value)>) -> (String, Value) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = rx.recv().await.unwrap();
            if matches!(
                event.0.as_str(),
                "request.completed" | "request.failed" | "request.cancelled"
            ) {
                return event;
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn lifecycle_waits_before_delivery_and_retries_exact_bytes_with_patched_header() {
    let (url, upstream) = fake_upstream(vec![200, 200]).await;
    let (config, provider, model) = super::tests::router_config(url);
    let server = test_server(&config);
    let mut downstream = CapturedDownstream::default();
    let delivered = downstream.delivered.clone();
    let resumes = Arc::new(AtomicUsize::new(0));
    let observed_resumes = resumes.clone();
    let (plugin, mut events) = recording_plugin(move |method, params| match method {
        "request.beforeSend" => {
            assert!(!delivered.load(Ordering::SeqCst));
            assert!(params.get("credentials").is_none());
            assert!(params["headers"].get("authorization").is_none());
            if params["attempt"] == 1 {
                assert_eq!(params["headers"]["x-test"], "replacement");
            }
            json!({"action":"continue","headers":[{"name":"x-test","value":format!("attempt-{}",params["attempt"])}]})
        }
        "request.afterHeaders" if params["attempt"] == 0 => {
            assert_eq!(params["response"]["status"], 200);
            assert!(!delivered.load(Ordering::SeqCst));
            json!({"action":"wait","token":"find-state","pollAfterMs":50})
        }
        "request.resume" => {
            assert!(!delivered.load(Ordering::SeqCst));
            observed_resumes.fetch_add(1, Ordering::SeqCst);
            json!({"action":"retry","headers":[{"name":"x-test","value":"replacement"}]})
        }
        _ => json!({"action":"continue"}),
    });
    with_test_plugins(
        vec![plugin],
        invoke(
            &server,
            &model_alias(&provider, &model),
            false,
            &mut downstream,
        ),
    )
    .await
    .unwrap();
    assert_eq!(resumes.load(Ordering::SeqCst), 1);
    assert_eq!(downstream.status, Some(200));
    let requests = upstream.await.unwrap();
    assert_eq!(requests[0].body, requests[1].body);
    assert_eq!(incoming_header(&requests[0], "x-test"), Some("attempt-0"));
    assert_eq!(incoming_header(&requests[1], "x-test"), Some("attempt-1"));
    let event = terminal(&mut events).await;
    assert_eq!(event.0, "request.completed");
    assert_eq!(event.1["status"], 200);
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn plugin_retry_keeps_the_body_when_plugins_are_disabled_during_upload() {
    let (url, upstream) = fake_upstream(vec![200, 200]).await;
    let (plugin, events) = recording_plugin(|method, params| {
        if method == "request.afterHeaders" && params["attempt"] == 0 {
            json!({"action":"retry"})
        } else {
            json!({"action":"continue"})
        }
    });
    let mut lifecycle = with_test_plugins(vec![plugin], async {
        LifecycleRequest::new(json!({"requestId": "retry-after-disable"}), None)
    })
    .await;
    assert!(lifecycle.is_active());
    let body = Bytes::from_static(br#"{"input":"same"}"#);
    with_test_plugins(Vec::new(), async {
        assert!(!has_plugins());
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut headers = HeaderMap::new();
        let mut downstream = CapturedDownstream::default();
        let mut attempt = 0;
        send_lifecycle_http(
            &mut downstream,
            &mut lifecycle,
            &client,
            &format!("{url}/responses"),
            &mut headers,
            body,
            &mut || {},
            &mut attempt,
            Duration::from_secs(5),
            None,
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    })
    .await;
    drop(events);
    let requests = upstream.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, requests[1].body);
}

#[tokio::test]
async fn lifecycle_retry_limit_and_reasoning_fallback_share_one_replay() {
    for retry_again in [false, true] {
        let (url, upstream) = fake_upstream(vec![200, 400]).await;
        let (config, provider, model) = super::tests::router_config(url);
        let server = test_server(&config);
        let mut downstream = CapturedDownstream::default();
        let (plugin, mut events) = recording_plugin(move |method, params| {
            if method == "request.afterHeaders" && (params["attempt"] == 0 || retry_again) {
                json!({"action":"retry"})
            } else {
                json!({"action":"continue"})
            }
        });
        with_test_plugins(
            vec![plugin],
            invoke(
                &server,
                &model_alias(&provider, &model),
                false,
                &mut downstream,
            ),
        )
        .await
        .unwrap();
        assert_eq!(upstream.await.unwrap().len(), 2);
        assert_eq!(downstream.status, Some(if retry_again { 409 } else { 400 }));
        assert!(!downstream.delivered.load(Ordering::SeqCst));
        assert_eq!(terminal(&mut events).await.0, "request.failed");
    }
}

#[tokio::test]
async fn lifecycle_abort_and_detected_websocket_cancel_do_not_send() {
    for cancelled in [false, true] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let (config, provider, model) =
            super::tests::router_config(format!("http://{}/v1", listener.local_addr().unwrap()));
        let server = test_server(&config);
        let mut downstream = CapturedDownstream {
            websocket: cancelled,
            cancel_at: cancelled.then(|| tokio::time::Instant::now() + Duration::from_millis(80)),
            ..Default::default()
        };
        let (plugin, mut events) = recording_plugin(move |method, _| {
            if method == "request.beforeSend" || method == "request.resume" {
                if cancelled {
                    json!({"action":"wait","token":"pending","pollAfterMs":50})
                } else {
                    json!({"action":"abort","status":422,"code":"test_abort"})
                }
            } else {
                json!({"action":"continue"})
            }
        });
        let result = with_test_plugins(
            vec![plugin],
            invoke(
                &server,
                &model_alias(&provider, &model),
                cancelled,
                &mut downstream,
            ),
        )
        .await;
        if cancelled {
            assert!(result.unwrap_err().is::<DownstreamClosed>());
        } else {
            result.unwrap();
            assert_eq!(downstream.status, Some(422));
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(10), listener.accept())
                .await
                .is_err()
        );
        assert_eq!(
            terminal(&mut events).await.0,
            if cancelled {
                "request.cancelled"
            } else {
                "request.failed"
            }
        );
    }
}

#[tokio::test]
async fn lifecycle_uses_http_fallback_only_when_plugin_is_active() {
    for active in [false, true] {
        let (url, upstream) = fake_upstream(vec![200]).await;
        let (config, provider, model) = super::tests::router_config(url);
        let server = test_server(&config);
        let mut downstream = CapturedDownstream {
            websocket: true,
            ..Default::default()
        };
        let plugins = if active {
            vec![TestPlugin::new("transparent", |_, _| {
                Ok(json!({"action":"continue"}))
            })]
        } else {
            vec![]
        };
        with_test_plugins(
            plugins,
            invoke(
                &server,
                &model_alias(&provider, &model),
                true,
                &mut downstream,
            ),
        )
        .await
        .unwrap();
        assert_eq!(downstream.websocket_attempts, usize::from(!active));
        assert_eq!(downstream.fallback_attempts, usize::from(active));
        if active {
            assert_eq!(upstream.await.unwrap().len(), 1);
        } else {
            upstream.abort();
        }
    }
}

#[tokio::test]
async fn lifecycle_http_and_network_errors_finish_failed() {
    for status in [400, 500] {
        let statuses = if status == 400 {
            vec![400, 400]
        } else {
            vec![500]
        };
        let expected_attempts = statuses.len();
        let (url, upstream) = fake_upstream(statuses).await;
        let (config, provider, model) = super::tests::router_config(url);
        let server = test_server(&config);
        let mut downstream = CapturedDownstream::default();
        let (plugin, mut events) = recording_plugin(|_, _| json!({"action":"continue"}));
        with_test_plugins(
            vec![plugin],
            invoke(
                &server,
                &model_alias(&provider, &model),
                false,
                &mut downstream,
            ),
        )
        .await
        .unwrap();
        assert_eq!(upstream.await.unwrap().len(), expected_attempts);
        let terminal = terminal(&mut events).await;
        assert_eq!(terminal.0, "request.failed");
        assert_eq!(terminal.1["status"], status);
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let (config, provider, model) = super::tests::router_config(format!("http://{address}/v1"));
    let server = test_server(&config);
    let mut downstream = CapturedDownstream::default();
    let (plugin, mut events) = recording_plugin(|_, _| json!({"action":"continue"}));
    with_test_plugins(
        vec![plugin],
        invoke(
            &server,
            &model_alias(&provider, &model),
            false,
            &mut downstream,
        ),
    )
    .await
    .unwrap();
    let terminal = terminal(&mut events).await;
    assert_eq!(terminal.0, "request.failed");
    assert_eq!(terminal.1["status"], 424);
}

#[tokio::test]
async fn lifecycle_stream_failures_are_not_reported_as_completed() {
    for protocol in [
        UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
        UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
        UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
    ] {
        let (url, upstream, _) = super::retry_contract_tests::error_upstream(
            200,
            format!("data: {}\n\n", json!({"type":"error","status":429,"error":{"code":"rate_limit_exceeded","message":"limited"}})),
            "text/event-stream",
        ).await;
        let (mut config, provider, model) = super::tests::router_config(url);
        config.profiles[0].upstream_protocol = protocol.into();
        config.profiles[0].normalize();
        let server = test_server(&config);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let reader = tokio::spawn(async move {
            let mut bytes = Vec::new();
            client.read_to_end(&mut bytes).await.unwrap();
            bytes
        });
        let mut downstream = HttpResponsesDownstream::new(socket);
        downstream.set_streaming_errors(true);
        let (plugin, mut events) = recording_plugin(|_, _| json!({"action":"continue"}));
        let _ = with_test_plugins(
            vec![plugin],
            invoke(
                &server,
                &model_alias(&provider, &model),
                true,
                &mut downstream,
            ),
        )
        .await;
        drop(downstream);
        assert!(
            String::from_utf8(reader.await.unwrap())
                .unwrap()
                .contains("response.failed")
        );
        let terminal = terminal(&mut events).await;
        assert_eq!(terminal.0, "request.failed", "{protocol}");
        assert_eq!(terminal.1["status"], 429, "{protocol}: {}", terminal.1);
        upstream.abort();
    }
}

#[tokio::test]
async fn api_key_override_preserves_native_responses_body_and_sse_across_retry() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let endpoint = format!("{url}/responses");
    let sse = b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"native-response\",\"status\":\"completed\",\"output\":[]}}\n\n";
    let upstream = tokio::spawn(async move {
        let mut requests = Vec::new();
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            requests.push(read_http_request(&mut socket).await.unwrap());
            socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", sse.len()).as_bytes()).await.unwrap();
            socket.write_all(sse).await.unwrap();
        }
        requests
    });
    let (config, provider, model) = super::tests::router_config(url);
    let server = test_server(&config);
    let selections = Arc::new(AtomicUsize::new(0));
    let selected = selections.clone();
    let endpoint_for_event = endpoint.clone();
    let (plugin, mut events) = recording_plugin(move |method, params| {
        if method == "request.beforeSend" {
            assert_eq!(params["metadata"]["upstreamUrl"], endpoint_for_event);
            assert_eq!(params["metadata"]["officialAccount"], false);
            assert_eq!(params["metadata"]["protocol"], "OpenAI Responses");
            assert_eq!(params["metadata"]["apiKeyAuthorized"], true);
            assert_eq!(params["metadata"]["apiKeySelected"], params["attempt"] == 1);
            assert!(params["headers"].get("authorization").is_none());
            if params["metadata"]["apiKeySelected"] == false {
                selected.fetch_add(1, Ordering::SeqCst);
                return json!({"action":"continue","apiKey":"mock-selected-key"});
            }
        }
        if method == "request.afterHeaders" && params["attempt"] == 0 {
            json!({"action":"retry"})
        } else {
            json!({"action":"continue"})
        }
    });
    let plugin = plugin.with_api_key_urls(&[&endpoint]);
    let mut downstream = CapturedDownstream::default();
    with_test_plugins(
        vec![plugin],
        invoke(
            &server,
            &model_alias(&provider, &model),
            true,
            &mut downstream,
        ),
    )
    .await
    .unwrap();
    assert_eq!(downstream.status, Some(200));
    assert_eq!(downstream.body, sse);
    let requests = upstream.await.unwrap();
    assert_eq!(requests[0].body, requests[1].body);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        body["input"],
        json!([{"type":"reasoning","encrypted_content":"test","summary":[]},{"role":"user","content":"test"}])
    );
    assert_eq!(body["stream"], true);
    assert_eq!(selections.load(Ordering::SeqCst), 1);
    for request in requests {
        assert_eq!(
            incoming_header(&request, "authorization"),
            Some("Bearer mock-selected-key")
        );
        assert!(incoming_header(&request, "x-api-key").is_none());
        assert!(incoming_header(&request, "api-key").is_none());
    }
    let terminal = terminal(&mut events).await;
    assert_eq!(terminal.0, "request.completed");
    assert!(!terminal.1.to_string().contains("mock-selected-key"));
    assert!(terminal.1["metadata"].get("apiKeySelected").is_none());
    let mut sensitive = HeaderMap::new();
    let mut key = HeaderValue::from_static("Bearer mock-selected-key");
    key.set_sensitive(true);
    sensitive.insert(AUTHORIZATION, key);
    assert!(!super::responses::format_upstream_headers(&sensitive).contains("mock-selected-key"));
}

#[tokio::test]
async fn selected_api_key_does_not_follow_a_redirect_to_another_path() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = format!("http://{address}/v1/responses");
    let upstream = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut socket).await.unwrap();
        assert_eq!(
            incoming_header(&request, "authorization"),
            Some("Bearer mock-key")
        );
        socket.write_all(format!("HTTP/1.1 302 Found\r\nlocation: http://{address}/v1/responses/compact\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").as_bytes()).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    let plugin = TestPlugin::new("redirect", |method, _| {
        Ok(if method == "request.beforeSend" {
            json!({"action":"continue","apiKey":"mock-key"})
        } else {
            json!({"action":"continue"})
        })
    })
    .with_api_key_urls(&[&endpoint]);
    with_test_plugins(vec![plugin], async {
        let mut lifecycle = LifecycleRequest::new(json!({"requestId":"redirect-test"}), None);
        lifecycle.set_transport_context(false, true);
        let client = super::server::upstream_http_client_builder()
            .no_proxy()
            .build()
            .unwrap();
        let mut headers = HeaderMap::new();
        let mut downstream = CapturedDownstream::default();
        let response = send_lifecycle_http(
            &mut downstream,
            &mut lifecycle,
            &client,
            &endpoint,
            &mut headers,
            Bytes::from_static(b"{\"input\":\"original\"}"),
            &mut || {},
            &mut 0,
            Duration::from_secs(2),
            None,
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        lifecycle.finish(
            crate::codey_plugins::lifecycle::LifecycleOutcome::Completed,
            Some(302),
            None,
        );
    })
    .await;
    upstream.await.unwrap();
}

#[tokio::test]
async fn api_key_scope_excludes_plugin_transport_even_for_a_matching_url() {
    const ENDPOINT: &str = "https://token.sensenova.cn/v1/responses";
    for plugin_transport in [false, true] {
        let (config, _, model) = super::tests::router_config(ENDPOINT.into());
        let snapshot = RouterSnapshot::from_config(&config);
        let mut resolved = snapshot.target_for_model(&model).unwrap();
        if plugin_transport {
            Arc::make_mut(&mut resolved.route).plugin_transport =
                Some(plugin_transport::Target::new(
                    "test.transport".into(),
                    codey_plugin_sdk::transport::TransportOptions {
                        account_email: "user@example.com".into(),
                        models: BTreeMap::new(),
                        image_generation: false,
                        image_edit: false,
                    },
                ));
        }
        let plugin = TestPlugin::new("scope-test", move |method, params| {
            if method == "request.beforeSend" {
                assert_eq!(params["metadata"]["apiKeyAuthorized"], !plugin_transport);
                assert_eq!(params["metadata"]["officialAccount"], false);
                // Returning a key on the transport route must fail closed.
                return Ok(json!({"action":"continue","apiKey":"mock-key"}));
            }
            Ok(json!({}))
        })
        .with_api_key_urls(&[ENDPOINT]);
        with_test_plugins(vec![plugin], async {
            let mut lifecycle = request_lifecycle(
                &HeaderMap::new(),
                &resolved,
                ProtocolBridge::NativeResponses,
                ResponsesRequestKind::Create,
                true,
                false,
            );
            lifecycle
                .set_upstream_target(&reqwest::Url::parse(ENDPOINT).unwrap())
                .unwrap();
            let result = lifecycle
                .dispatch(LifecycleStage::BeforeSend, 0, BTreeMap::new(), None)
                .await;
            let mut outgoing = HeaderMap::new();
            outgoing.insert(AUTHORIZATION, HeaderValue::from_static("Bearer original"));
            if plugin_transport {
                assert_eq!(result.unwrap_err().code, "plugin_api_key_unauthorized");
                lifecycle.apply_api_key(&mut outgoing).unwrap();
                assert_eq!(outgoing[AUTHORIZATION], "Bearer original");
            } else {
                result.unwrap();
                lifecycle.apply_api_key(&mut outgoing).unwrap();
                assert_eq!(outgoing[AUTHORIZATION], "Bearer mock-key");
            }
        })
        .await;
    }
}
