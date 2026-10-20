use super::*;
use crate::codey_plugins::lifecycle::{
    LifecycleRequest, LifecycleStage, TestPlugin, has_plugins, with_test_plugins,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
struct CapturedDownstream {
    probe: Option<RouteRequestLogProbe>,
    status: Option<u16>,
    body: Vec<u8>,
    delivered: Arc<AtomicBool>,
    websocket: bool,
    websocket_attempts: usize,
    fallback_attempts: usize,
    cancel_at: Option<tokio::time::Instant>,
    cancel_signal: Option<Arc<tokio::sync::Notify>>,
    response_started: bool,
    error_write_budget: Option<(Arc<Semaphore>, usize)>,
    cancel_compaction_delivery: bool,
}

#[async_trait]
impl ResponsesDownstream for CapturedDownstream {
    fn request_log_probe(&self) -> Option<&RouteRequestLogProbe> {
        self.probe.as_ref()
    }
    fn is_websocket(&self) -> bool {
        self.websocket
    }
    fn event_stream_started(&self) -> bool {
        self.response_started
    }
    async fn wait_for_upstream<T, F>(&mut self, future: F) -> Result<T>
    where
        T: Send,
        F: std::future::Future<Output = T> + Send,
    {
        if let Some(signal) = self.cancel_signal.as_ref() {
            tokio::select! { biased; _ = signal.notified() => Err(DownstreamClosed.into()), value = future => Ok(value) }
        } else if let Some(deadline) = self.cancel_at {
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
        if let Some((budget, available)) = self.error_write_budget.as_ref() {
            assert_eq!(
                budget.available_permits(),
                *available,
                "request memory must be released before writing the error response"
            );
        }
        self.status = Some(status);
        self.body = code.as_bytes().to_vec();
        Ok(())
    }
    async fn write_json(&mut self, status: u16, value: &Value) -> Result<()> {
        if self.cancel_compaction_delivery {
            return Err(DownstreamClosed.into());
        }
        self.status = Some(status);
        self.body = serde_json::to_vec(value)?;
        Ok(())
    }
    async fn start_event_stream(&mut self) -> Result<()> {
        self.response_started = true;
        Ok(())
    }
    async fn write_event(&mut self, _: &Value) -> Result<()> {
        if self.cancel_compaction_delivery {
            return Err(DownstreamClosed.into());
        }
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

pub(super) fn test_server(config: &CodeyConfig) -> RouterServer {
    let official_concurrency = Arc::new(OfficialConcurrency::default());
    official_concurrency.update(&RouterSnapshot::from_config(config));
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
        official_concurrency,
        admitted_connections: Arc::new(Semaphore::new(MAX_ADMITTED_CONNECTIONS)),
        rejection_limit: Arc::new(Semaphore::new(4)),
        request_body_budget: Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS)),
        bindings: Arc::default(),
        websocket_backoffs: Arc::default(),
        native_history_cache: Arc::default(),
        portable_history_cache: Arc::default(),
        idle_downstreams: Arc::default(),
        client: super::server::upstream_http_client_builder()
            .no_proxy()
            .build()
            .unwrap(),
        proxied_clients: Mutex::default(),
        official_auth_path: PathBuf::from("/nonexistent/codey-lifecycle-test-auth"),
        account_usage_cache: Arc::default(),
        official_auth_cache: Arc::default(),
        siwc_auth_cache: Arc::default(),
        request_log: Arc::new(RouteRequestLogController::new()),
    }
}

#[tokio::test]
async fn native_compaction_commits_binding_only_after_validated_complete_delivery() {
    for stage in [
        "upstream_cancel",
        "delivery_cancel",
        "rate_limited",
        "invalid",
        "completed",
    ] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let (mut config, source, model) =
            super::tests::router_config(format!("http://{}/v1", listener.local_addr().unwrap()));
        config.profiles[0].supports_remote_compaction = true;
        let mut target = config.profiles[0].clone();
        target.id = "compaction-target".into();
        let target_id = target.provider_id().to_string();
        config.profiles.push(target);
        config
            .selected_models_by_provider
            .insert(target_id.clone(), vec![model.clone()]);
        let server = test_server(&config);
        let source_route = server
            .snapshot
            .read()
            .unwrap()
            .target_for_request(&model, Some(&source), None)
            .unwrap();
        let keys = ["thread-id:compact-parent".into()];
        server.bindings.lock().unwrap().remember_resolved(
            &keys,
            &source_route,
            false,
            false,
            Some(true),
        );
        let unrelated = (0..MAX_ROUTE_BINDINGS - 1)
            .map(|index| format!("thread-id:unrelated-{index}"))
            .collect::<Vec<_>>();
        server.bindings.lock().unwrap().remember_resolved(
            &unrelated,
            &source_route,
            false,
            false,
            Some(true),
        );
        let before = server.bindings.lock().unwrap().clone();
        let signal = Arc::new(tokio::sync::Notify::new());
        let upstream_signal = Arc::clone(&signal);
        let upstream = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _request = read_http_request(&mut stream).await.unwrap();
            if stage == "upstream_cancel" {
                upstream_signal.notify_one();
            } else if stage == "rate_limited" {
                write_json_response(
                    &mut stream,
                    429,
                    &json!({"error":{"message":"quota exceeded"}}),
                )
                .await
                .unwrap();
            } else if stage == "invalid" {
                write_json_response(&mut stream, 200, &json!({"id":"invalid","output":[]}))
                    .await
                    .unwrap();
            } else {
                write_json_response(
                    &mut stream,
                    200,
                    &json!({
                        "id":"compacted", "status":"completed", "output":[
                            {"type":"compaction","encrypted_content":"target-opaque-history"}
                        ]
                    }),
                )
                .await
                .unwrap();
            }
        });
        let mut downstream = CapturedDownstream {
            websocket: true,
            cancel_signal: (stage == "upstream_cancel").then_some(signal),
            cancel_compaction_delivery: stage == "delivery_cancel",
            ..Default::default()
        };
        let result = tokio::time::timeout(Duration::from_secs(5), server.proxy_parsed_responses_inner(
            HttpRequest { method:"POST".into(), path:"/responses".into(),
                headers:vec![("thread-id".into(),"compact-parent".into()),
                    ("session-id".into(),"new-compact-session".into())], body:vec![],
                _body_budget_permit:None },
            json!({"model":model_alias(&target_id,&model),"input":[
                {"role":"user","content":"complete original history"},{"type":"compaction_trigger"}
            ]}), None, ResponsesRequestKind::Create, &mut downstream,
        )).await.unwrap();
        upstream.await.unwrap();
        let bindings = server.bindings.lock().unwrap();
        let actual = &bindings.routes[&keys[0]];
        if stage == "completed" {
            assert!(result.is_ok());
            assert_eq!(actual.provider_id, target_id);
            assert!(!actual.portable);
        } else {
            if stage.ends_with("cancel") {
                assert!(result.unwrap_err().is::<DownstreamClosed>(), "{stage}");
            } else {
                assert!(result.is_ok());
                assert_eq!(
                    downstream.status,
                    Some(if stage == "rate_limited" { 429 } else { 502 })
                );
            }
            assert_eq!(bindings.next_generation, before.next_generation, "{stage}");
            assert_eq!(bindings.order, before.order, "{stage}");
            assert_eq!(bindings.routes.len(), before.routes.len());
            for (key, expected) in &before.routes {
                let actual = &bindings.routes[key];
                assert_eq!(actual.provider_id, expected.provider_id);
                assert_eq!(actual.generation, expected.generation);
                assert_eq!(actual.upstream_model, expected.upstream_model);
                assert_eq!(actual.context_config, expected.context_config);
                assert_eq!(actual.portable, expected.portable);
            }
        }
    }
}

#[tokio::test]
async fn pending_cross_route_native_compaction_does_not_authorize_source_response_id() {
    let listener = Arc::new(TcpListener::bind(("127.0.0.1", 0)).await.unwrap());
    let (mut config, source, model) =
        super::tests::router_config(format!("http://{}/v1", listener.local_addr().unwrap()));
    config.profiles[0].supports_remote_compaction = true;
    let mut target = config.profiles[0].clone();
    target.id = "pending-compaction-target".into();
    let target_id = target.provider_id().to_string();
    config.profiles.push(target);
    config
        .selected_models_by_provider
        .insert(target_id.clone(), vec![model.clone()]);
    let server = Arc::new(test_server(&config));
    let source_route = server
        .snapshot
        .read()
        .unwrap()
        .target_for_request(&model, Some(&source), None)
        .unwrap();
    let keys = ["thread-id:compact-parent".into()];
    server.bindings.lock().unwrap().remember_resolved(
        &keys,
        &source_route,
        false,
        false,
        Some(true),
    );
    let before = server.bindings.lock().unwrap().routes[&keys[0]].clone();
    let (arrived_tx, arrived_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let upstream_listener = Arc::clone(&listener);
    let upstream = tokio::spawn(async move {
        let (mut stream, _) = upstream_listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        assert_eq!(request.path, "/v1/responses/compact");
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert!(body.get("previous_response_id").is_none());
        arrived_tx.send(()).unwrap();
        release_rx.await.unwrap();
        write_json_response(
            &mut stream,
            200,
            &json!({
                "id":"compacted", "status":"completed", "output":[
                    {"type":"compaction","encrypted_content":"target-opaque-history"}
                ]
            }),
        )
        .await
        .unwrap();
    });
    let compact_server = Arc::clone(&server);
    let target_model = model_alias(&target_id, &model);
    let compact_model = target_model.clone();
    let compact = tokio::spawn(async move {
        let mut downstream = CapturedDownstream::default();
        let result = compact_server
            .proxy_parsed_responses_inner(
                HttpRequest {
                    method: "POST".into(),
                    path: "/responses/compact".into(),
                    headers: vec![("thread-id".into(), "compact-parent".into())],
                    body: vec![],
                    _body_budget_permit: None,
                },
                json!({"model":compact_model,"input":[
                    {"role":"user","content":"complete original history"}
                ]}),
                None,
                ResponsesRequestKind::Compact,
                &mut downstream,
            )
            .await;
        (result, downstream)
    });
    tokio::time::timeout(Duration::from_secs(5), arrived_rx)
        .await
        .unwrap()
        .unwrap();
    {
        let bindings = server.bindings.lock().unwrap();
        assert_eq!(bindings.routes[&keys[0]].provider_id, source);
        assert_eq!(bindings.routes[&keys[0]].generation, before.generation);
    }
    for websocket in [false, true] {
        let mut downstream = CapturedDownstream {
            websocket,
            ..Default::default()
        };
        tokio::time::timeout(Duration::from_secs(5), server.proxy_parsed_responses_inner(
            HttpRequest { method:"POST".into(), path:"/responses".into(),
                headers:vec![("thread-id".into(),"compact-parent".into())], body:vec![],
                _body_budget_permit:None },
            json!({"model":target_model,"input":"delta","previous_response_id":"source-private-id"}),
            None, ResponsesRequestKind::Create, &mut downstream,
        )).await.unwrap().unwrap();
        assert_eq!(downstream.status, Some(400));
        assert_eq!(downstream.body, b"context_not_portable");
        assert_eq!(downstream.websocket_attempts, 0);
        assert_eq!(downstream.fallback_attempts, 0);
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(75), listener.accept())
            .await
            .is_err()
    );
    assert_eq!(
        server.bindings.lock().unwrap().routes[&keys[0]].generation,
        before.generation
    );
    release_tx.send(()).unwrap();
    let (result, downstream) = tokio::time::timeout(Duration::from_secs(5), compact)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_ok());
    assert_eq!(downstream.status, Some(200));
    upstream.await.unwrap();
    let bindings = server.bindings.lock().unwrap();
    assert_eq!(bindings.routes[&keys[0]].provider_id, target_id);
    assert!(!bindings.routes[&keys[0]].portable);
}

#[tokio::test]
async fn cross_route_websocket_delta_is_rejected_before_transport_attempt() {
    let (mut config, source, model) = super::tests::router_config("http://127.0.0.1:9/v1".into());
    let mut target = config.profiles[0].clone();
    target.id = "native-target".into();
    let target_id = target.provider_id().to_string();
    config.profiles.push(target);
    config
        .selected_models_by_provider
        .insert(target_id.clone(), vec![model.clone()]);
    let server = test_server(&config);
    let source_route = server
        .snapshot
        .read()
        .unwrap()
        .target_for_request(&model, Some(&source), None)
        .unwrap();
    let keys = ["thread-id:delta-parent".into()];
    server.bindings.lock().unwrap().remember_resolved(
        &keys,
        &source_route,
        false,
        false,
        Some(true),
    );
    let before = server.bindings.lock().unwrap().routes[&keys[0]].generation;
    let mut downstream = CapturedDownstream {
        websocket: true,
        ..Default::default()
    };
    server
        .proxy_parsed_responses_inner(
            HttpRequest {
                method: "POST".into(),
                path: "/responses".into(),
                headers: vec![("thread-id".into(), "delta-parent".into())],
                body: vec![],
                _body_budget_permit: None,
            },
            json!({"model":model_alias(&target_id,&model),"input":"delta",
            "previous_response_id":"source-private-id"}),
            None,
            ResponsesRequestKind::Create,
            &mut downstream,
        )
        .await
        .unwrap();
    assert_eq!(downstream.status, Some(400));
    assert_eq!(downstream.body, b"context_not_portable");
    assert_eq!(downstream.websocket_attempts, 0);
    assert_eq!(downstream.fallback_attempts, 0);
    assert_eq!(
        server.bindings.lock().unwrap().routes[&keys[0]].generation,
        before
    );
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

fn managed_account_record(
    id: &str,
    issued_at: u64,
) -> crate::official_accounts::OfficialAccountRecord {
    use base64::Engine as _;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "iat": issued_at,
            "https://api.openai.com/auth": { "chatgpt_account_id": id }
        }))
        .unwrap(),
    );
    crate::official_accounts::OfficialAccountRecord::from_auth(
        json!({"tokens": {"access_token": format!("header.{payload}.signature"), "account_id": id}}),
        1,
    )
    .unwrap()
}

fn managed_account_server(auth: OfficialRouteAuth) -> (RouterServer, RouteTarget) {
    let (server, _) = official_history_server("http://127.0.0.1:9/v1".into());
    let mut route = server
        .snapshot
        .read()
        .unwrap()
        .routes
        .values()
        .next()
        .unwrap()
        .as_ref()
        .clone();
    route.official_auth = Some(auth);
    (server, route)
}

fn managed_account_request(
    record: &crate::official_accounts::OfficialAccountRecord,
) -> HttpRequest {
    HttpRequest {
        method: "POST".into(),
        path: "/v1/responses".into(),
        headers: vec![
            (
                "authorization".into(),
                format!(
                    "Bearer {}",
                    record.auth["tokens"]["access_token"].as_str().unwrap()
                ),
            ),
            (
                CHATGPT_ACCOUNT_ID_HEADER.into(),
                record.account_id.clone().unwrap(),
            ),
        ],
        body: Vec::new(),
        _body_budget_permit: None,
    }
}

#[tokio::test]
async fn deleted_managed_accounts_reject_old_routes_even_with_cached_credentials() {
    for mode in ["default", "stored", "siwc"] {
        let directory = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let store = crate::official_accounts::OfficialAccountStore::new(directory.path());
        let record = managed_account_record("selected", 100);
        store.upsert(&record).unwrap();
        if mode != "stored" {
            store.activate(&record.id, home.path()).unwrap();
        }
        let siwc_path = if mode == "siwc" {
            let credential = serde_json::from_value(json!({
                "clientId": "test-client", "scopes": [], "accessToken": "siwc-token",
                "expiresAt": crate::official_accounts::unix_timestamp() + 3600,
            }))
            .unwrap();
            store.attach_siwc(&record.id, &credential).unwrap();
            Some(store.siwc_path(&record.id))
        } else {
            None
        };
        let (server, old_route) = managed_account_server(OfficialRouteAuth {
            account_id: record.id.clone(),
            email: None,
            path: store.credential_path(home.path(), &record.id),
            record_path: store.account_path(&record.id),
            siwc_path,
            accepts_incoming_authorization: mode == "default",
        });
        let request = managed_account_request(&record);
        // 先填充凭据缓存，删除后继续使用同一快照和同一请求头。
        assert!(
            server
                .prepare_upstream_request_headers(&request, &old_route)
                .await
                .is_ok()
        );
        store
            .remove_with_codex_login(&record.id, home.path())
            .unwrap();
        let error = server
            .prepare_upstream_request_headers(&request, &old_route)
            .await
            .unwrap_err();
        assert_eq!(error.0, 401, "{mode}");
        assert_eq!(error.1, "official_account_unavailable", "{mode}");
        assert!(error.2.contains("已移除"), "{mode}");
    }
}

#[tokio::test]
async fn stale_default_route_keeps_selected_account_after_default_switch() {
    let directory = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let store = crate::official_accounts::OfficialAccountStore::new(directory.path());
    let selected = managed_account_record("selected", 100);
    let other = managed_account_record("other", 200);
    store.upsert(&selected).unwrap();
    store.upsert(&other).unwrap();
    store.activate(&selected.id, home.path()).unwrap();
    let (server, old_route) = managed_account_server(OfficialRouteAuth {
        account_id: selected.id.clone(),
        email: None,
        path: store.credential_path(home.path(), &selected.id),
        record_path: store.account_path(&selected.id),
        siwc_path: None,
        accepts_incoming_authorization: true,
    });
    store.activate(&other.id, home.path()).unwrap();
    let headers = server
        .prepare_upstream_request_headers(&managed_account_request(&other), &old_route)
        .await
        .unwrap();
    assert_eq!(
        headers[AUTHORIZATION],
        format!(
            "Bearer {}",
            selected.auth["tokens"]["access_token"].as_str().unwrap()
        )
    );
    assert_eq!(headers[CHATGPT_ACCOUNT_ID_HEADER], "selected");
}

#[tokio::test]
async fn managed_default_route_accepts_same_account_refresh_and_rejects_foreign_tokens() {
    let directory = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let store = crate::official_accounts::OfficialAccountStore::new(directory.path());
    let selected = managed_account_record("selected", 100);
    let refreshed_login = managed_account_record("selected", 200);
    let refreshed_request = managed_account_record("selected", 300);
    let other = managed_account_record("other", 400);
    store.upsert(&selected).unwrap();
    store.activate(&selected.id, home.path()).unwrap();
    crate::official_accounts::OfficialAccountStore::write_codex_login(
        home.path(),
        &refreshed_login,
    )
    .unwrap();
    let (server, route) = managed_account_server(OfficialRouteAuth {
        account_id: selected.id.clone(),
        email: None,
        path: store.credential_path(home.path(), &selected.id),
        record_path: store.account_path(&selected.id),
        siwc_path: None,
        accepts_incoming_authorization: true,
    });
    for request_record in [&selected, &refreshed_request, &other] {
        let headers = server
            .prepare_upstream_request_headers(&managed_account_request(request_record), &route)
            .await
            .unwrap();
        let expected = if request_record == &refreshed_request {
            &refreshed_request
        } else {
            &refreshed_login
        };
        assert_eq!(
            headers[AUTHORIZATION],
            format!(
                "Bearer {}",
                expected.auth["tokens"]["access_token"].as_str().unwrap()
            )
        );
        assert_eq!(headers[CHATGPT_ACCOUNT_ID_HEADER], "selected");
    }
    // 非文件的记录路径应安全失败，不能借助已有缓存继续授权。
    std::fs::remove_file(store.account_path(&selected.id)).unwrap();
    std::fs::create_dir(store.account_path(&selected.id)).unwrap();
    let error = server
        .prepare_upstream_request_headers(&managed_account_request(&selected), &route)
        .await
        .unwrap_err();
    assert_eq!(error.1, "official_account_unavailable");
    assert!(error.2.contains("不是文件"));
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
        } else {
            history.as_array_mut().unwrap().push(json!({
                "type":"web_search_call",
                "status":"completed",
                "action":{"type":"search","query":"test"}
            }));
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
        assert_eq!(items.len(), if websocket { 6 } else { 7 });
        assert_eq!(items[0], json!({"role":"user","content":"original task"}));
        assert_portable_official_history(&items[1..5]);
        if !websocket {
            assert_eq!(items[5]["type"], "web_search_call");
        }
        assert_eq!(
            items.last().unwrap(),
            &json!({"role":"user","content":"follow up"})
        );
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
        let mut attempts = AttemptBudget::default();
        send_lifecycle_http(
            &mut downstream,
            &mut lifecycle,
            &client,
            &format!("{url}/responses"),
            &mut headers,
            body,
            &mut || {},
            &mut attempts,
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
async fn diagnostic_attempts_match_actual_plugin_and_reasoning_sends() {
    use crate::route_request_log::{AttemptOutcome, AttemptTrigger};
    for plugin_retry in [false, true] {
        let (url, upstream) = fake_upstream(vec![400, 200]).await;
        let (config, provider, model) = super::tests::router_config(url);
        let server = test_server(&config);
        let probe = RouteRequestLogProbe::detached_test_probe();
        let mut downstream = CapturedDownstream {
            probe: Some(probe.clone()),
            ..Default::default()
        };
        let (plugin, _events) = recording_plugin(move |method, params| {
            if plugin_retry && method == "request.afterHeaders" && params["attempt"] == 0 {
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
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), upstream)
                .await
                .expect("two upstream sends")
                .unwrap()
                .len(),
            2
        );
        let attempts = probe.attempts_for_test();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].sequence, 1);
        assert_eq!(attempts[0].status_code, Some(400));
        assert_eq!(attempts[0].outcome, AttemptOutcome::Rejected);
        assert_eq!(attempts[1].sequence, 2);
        assert_eq!(attempts[1].status_code, Some(200));
        assert_eq!(
            attempts[1].trigger,
            if plugin_retry {
                AttemptTrigger::PluginRetry
            } else {
                AttemptTrigger::ReasoningRepair
            }
        );
    }
}

#[tokio::test]
async fn rejected_before_send_hook_does_not_create_an_upstream_attempt() {
    let (config, provider, model) = super::tests::router_config("http://127.0.0.1:9/v1".into());
    let server = test_server(&config);
    let probe = RouteRequestLogProbe::detached_test_probe();
    let mut downstream = CapturedDownstream {
        probe: Some(probe.clone()),
        ..Default::default()
    };
    let (plugin, mut events) = recording_plugin(|method, _| {
        if method == "request.beforeSend" {
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
    // 插件层在发送前校验非法 retry 动作，网关没有机会调用上游。
    assert_eq!(downstream.status, Some(502));
    let mut saw_before_send = false;
    while let Ok((method, _)) = events.try_recv() {
        saw_before_send |= method == "request.beforeSend";
    }
    assert!(saw_before_send);
    assert!(probe.attempts_for_test().is_empty());
}

#[tokio::test]
async fn cancellation_before_retry_send_preserves_the_rejected_first_attempt() {
    use crate::route_request_log::{AttemptOutcome, AttemptPhase};
    let (url, upstream) = fake_upstream(vec![400]).await;
    let (config, provider, model) = super::tests::router_config(url);
    let server = test_server(&config);
    let probe = RouteRequestLogProbe::detached_test_probe();
    let cancelled = Arc::new(tokio::sync::Notify::new());
    let signal = Arc::clone(&cancelled);
    let mut downstream = CapturedDownstream {
        websocket: true,
        probe: Some(probe.clone()),
        cancel_signal: Some(cancelled),
        ..Default::default()
    };
    let (plugin, _events) = recording_plugin(move |method, params| {
        if method == "request.afterHeaders" {
            return json!({"action":"retry"});
        }
        if method == "request.beforeSend" && params["attempt"] == 1 {
            signal.notify_one();
        }
        json!({"action":"continue"})
    });
    let error = tokio::time::timeout(
        Duration::from_secs(3),
        with_test_plugins(
            vec![plugin],
            invoke(
                &server,
                &model_alias(&provider, &model),
                false,
                &mut downstream,
            ),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.is::<DownstreamClosed>());
    assert_eq!(upstream.await.unwrap().len(), 1);
    let attempts = probe.attempts_for_test();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status_code, Some(400));
    assert_eq!(attempts[0].outcome, AttemptOutcome::Rejected);
    assert_eq!(attempts[0].phase, AttemptPhase::ResponseHeaders);
}

#[tokio::test]
async fn reasoning_repair_consumes_the_replay_before_a_plugin_can_retry() {
    let (url, upstream) = fake_upstream(vec![400, 200]).await;
    let (config, provider, model) = super::tests::router_config(url);
    let server = test_server(&config);
    let mut downstream = CapturedDownstream::default();
    let (plugin, mut events) = recording_plugin(|method, params| {
        if method == "request.afterHeaders" && params["attempt"] == 1 {
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
    let requests = upstream.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_ne!(
        requests[0].body, requests[1].body,
        "second request must contain the reasoning repair"
    );
    let repaired: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert!(repaired.to_string().contains("reasoning_text"));
    assert_eq!(downstream.status, Some(409));
    assert_eq!(downstream.body, b"plugin_retry_limit");
    assert!(!downstream.delivered.load(Ordering::SeqCst));
    assert_eq!(terminal(&mut events).await.0, "request.failed");
}

#[tokio::test]
async fn started_downstream_forbids_both_plugin_retry_and_reasoning_repair() {
    for plugin_retry in [false, true] {
        let (url, upstream) = fake_upstream(vec![400]).await;
        let (config, provider, model) = super::tests::router_config(url);
        let server = test_server(&config);
        let mut downstream = CapturedDownstream {
            response_started: true,
            ..Default::default()
        };
        let (plugin, mut events) = recording_plugin(move |method, _| {
            if method == "request.afterHeaders" && plugin_retry {
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
        assert_eq!(upstream.await.unwrap().len(), 1);
        assert_eq!(
            downstream.status,
            Some(if plugin_retry { 409 } else { 400 })
        );
        assert!(!downstream.delivered.load(Ordering::SeqCst));
        assert_eq!(terminal(&mut events).await.0, "request.failed");
    }
}

#[tokio::test]
async fn cancellation_after_headers_releases_the_replay_body_and_reports_cancelled() {
    let (url, upstream) = fake_upstream(vec![200]).await;
    let (config, provider, model) = super::tests::router_config(url);
    let server = test_server(&config);
    let cancel = Arc::new(tokio::sync::Notify::new());
    let notify = cancel.clone();
    let mut downstream = CapturedDownstream {
        websocket: true,
        cancel_signal: Some(cancel),
        ..Default::default()
    };
    let (plugin, mut events) = recording_plugin(move |method, _| {
        if method == "request.afterHeaders" {
            notify.notify_one();
            json!({"action":"wait","token":"after-headers","pollAfterMs":50})
        } else if method == "request.resume" {
            json!({"action":"wait","token":"after-headers","pollAfterMs":50})
        } else {
            json!({"action":"continue"})
        }
    });
    let available = server.request_body_budget.available_permits();
    let permit = server
        .request_body_budget
        .clone()
        .try_acquire_many_owned(4)
        .unwrap();
    let body = json!({"model":model_alias(&provider, &model),"stream":true,"input":"cancel after headers"});
    let encoded = serde_json::to_vec(&body).unwrap();
    let request = HttpRequest {
        method: "POST".into(),
        path: "/v1/responses".into(),
        headers: vec![],
        body: Vec::new(),
        _body_budget_permit: Some(permit),
    };
    let result = with_test_plugins(
        vec![plugin],
        server.proxy_parsed_responses_inner(
            request,
            body,
            Some(encoded),
            ResponsesRequestKind::Create,
            &mut downstream,
        ),
    )
    .await;
    assert!(result.unwrap_err().is::<DownstreamClosed>());
    assert_eq!(upstream.await.unwrap().len(), 1);
    assert_eq!(server.request_body_budget.available_permits(), available);
    assert_eq!(downstream.status, None);
    assert!(!downstream.delivered.load(Ordering::SeqCst));
    assert_eq!(terminal(&mut events).await.0, "request.cancelled");
}

#[tokio::test]
async fn rejected_response_releases_request_memory_before_reading_its_body() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (headers_sent, headers_received) = tokio::sync::oneshot::channel();
    let (release_body, body_released) = tokio::sync::oneshot::channel();
    let error_body = br#"{"error":{"message":"quota exhausted","code":"rate_limit_exceeded"}}"#;
    let upstream = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_http_request(&mut socket).await.unwrap();
        socket.write_all(format!(
            "HTTP/1.1 429 Too Many Requests\r\ncontent-length: {}\r\nx-request-id: rejected-request\r\nretry-after: 3\r\nconnection: close\r\n\r\n", error_body.len(),
        ).as_bytes()).await.unwrap();
        headers_sent.send(()).unwrap();
        body_released.await.unwrap();
        socket.write_all(error_body).await.unwrap();
    });
    let (config, provider, model) = super::tests::router_config(url.clone());
    let server = test_server(&config);
    let resolved = RouterSnapshot::from_config(&config)
        .target_for_model(&model_alias(&provider, &model))
        .unwrap();
    let endpoint = format!("{url}/responses");
    let execution = HttpExecution {
        server: &server,
        resolved: &resolved,
        bridge: ProtocolBridge::NativeResponses,
        request_kind: ResponsesRequestKind::Create,
        compacting: false,
        native_compaction: false,
        compaction_phase: GatewayCompactionPhase::GatewaySummary,
        upstream_url: &endpoint,
        upstream_stream_requested: false,
    };
    let available = server.request_body_budget.available_permits();
    let permit = server
        .request_body_budget
        .clone()
        .try_acquire_many_owned(4)
        .unwrap();
    let request = EncodedHttpRequest {
        body: Bytes::from_static(b"{\"input\":\"test\"}"),
        admission: Some(permit),
    };
    let mut downstream = CapturedDownstream::default();
    let mut lifecycle = LifecycleRequest::inert();
    let mut headers = HeaderMap::new();
    let send = execution.send(
        &mut downstream,
        &mut lifecycle,
        &server.client,
        &mut headers,
        request,
        &[],
    );
    tokio::pin!(send);
    let released = tokio::time::timeout(Duration::from_secs(2), async {
        headers_received.await.unwrap();
        let all = server
            .request_body_budget
            .clone()
            .acquire_many_owned(available as u32)
            .await
            .unwrap();
        drop(all);
    });
    tokio::select! {
        result = &mut send => panic!("error body has not arrived: {}", result.is_ok()),
        result = released => result.expect("request memory must be free before reading the error body"),
    }
    release_body.send(()).unwrap();
    let received = send.await.unwrap().unwrap();
    let ReceivedHttpResponse::Rejected { head, body } = received else {
        panic!("expected the original upstream rejection");
    };
    assert_eq!(head.status, 429);
    assert_eq!(head.request_id.as_deref(), Some("rejected-request"));
    assert_eq!(head.retry_advice.unwrap().header, "3");
    assert_eq!(body, error_body);
    assert_eq!(server.request_body_budget.available_permits(), available);
    upstream.await.unwrap();
}

#[tokio::test]
async fn interrupted_error_bodies_keep_the_websocket_failure_contract_without_replay() {
    for status in [400, 502] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let upstream = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_http_request(&mut socket).await.unwrap();
            socket.write_all(format!(
                "HTTP/1.1 {status} Error\r\ncontent-length: 100\r\nconnection: close\r\n\r\n{{\"error\":",
            ).as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(50), listener.accept())
                    .await
                    .is_err()
            );
        });
        let (config, provider, model) = super::tests::router_config(url);
        let server = test_server(&config);
        let available = server.request_body_budget.available_permits();
        let permit = server
            .request_body_budget
            .clone()
            .try_acquire_many_owned(4)
            .unwrap();
        let body = json!({"model":model_alias(&provider, &model),"stream":true,"input":[{"type":"reasoning","encrypted_content":"test","summary":[]},{"role":"user","content":"test"}]});
        let encoded = serde_json::to_vec(&body).unwrap();
        let request = HttpRequest {
            method: "POST".into(),
            path: "/v1/responses".into(),
            headers: vec![],
            body: Vec::new(),
            _body_budget_permit: Some(permit),
        };
        let mut downstream = CapturedDownstream {
            websocket: true,
            error_write_budget: Some((server.request_body_budget.clone(), available)),
            ..Default::default()
        };
        let plugin = TestPlugin::new("http-error", |_, _| Ok(json!({"action":"continue"})));
        with_test_plugins(
            vec![plugin],
            server.proxy_parsed_responses_inner(
                request,
                body,
                Some(encoded),
                ResponsesRequestKind::Create,
                &mut downstream,
            ),
        )
        .await
        .unwrap();
        assert_eq!(downstream.status, Some(502));
        assert_eq!(downstream.body, b"upstream_response_failed");
        assert!(!downstream.delivered.load(Ordering::SeqCst));
        upstream.await.unwrap();
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
            &mut AttemptBudget::default(),
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
