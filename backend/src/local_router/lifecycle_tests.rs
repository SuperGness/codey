use super::*;
use crate::codey_plugins::lifecycle::{
    LifecycleRequest, TestPlugin, has_plugins, with_test_plugins,
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
        client: reqwest::Client::builder().no_proxy().build().unwrap(),
        proxied_clients: Mutex::default(),
        official_auth_path: PathBuf::from("/nonexistent/codey-lifecycle-test-auth"),
        account_usage_cache: Arc::default(),
        official_auth_cache: Arc::default(),
        request_log: Arc::new(RouteRequestLogController::new()),
    }
}

async fn invoke(
    server: &RouterServer,
    model: &str,
    stream: bool,
    downstream: &mut CapturedDownstream,
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
