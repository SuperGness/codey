use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn registry(limit: u32) -> Arc<OfficialConcurrency> {
    let registry = Arc::new(OfficialConcurrency::default());
    registry.gates.lock().unwrap().insert(
        "account:a".into(),
        Arc::new(OfficialGate {
            state: Mutex::new(GateState {
                enabled: true,
                limit,
                ..GateState::default()
            }),
        }),
    );
    registry
}

fn ready(registry: &OfficialConcurrency) -> OfficialPermit {
    match registry.acquire("account:a").unwrap() {
        OfficialAdmission::Ready(permit) => permit,
        _ => panic!("expected immediate permit"),
    }
}

fn queued(registry: &OfficialConcurrency) -> OfficialWait {
    match registry.acquire("account:a").unwrap() {
        OfficialAdmission::Waiting(waiter) => waiter,
        _ => panic!("expected queued request"),
    }
}

fn change_limit(registry: &OfficialConcurrency, limit: u32) {
    let gates = registry.gates.lock().unwrap();
    let mut state = gates["account:a"].state.lock().unwrap();
    state.limit = limit;
    state.dispatch();
}

fn counts(registry: &OfficialConcurrency) -> (usize, usize) {
    let gates = registry.gates.lock().unwrap();
    let state = gates["account:a"].state.lock().unwrap();
    (state.active, state.waiters.len())
}

#[tokio::test]
async fn fifo_and_cancelled_grants_release_exactly_once() {
    let registry = registry(1);
    let first = ready(&registry);
    let second = queued(&registry);
    let third = queued(&registry);
    assert_eq!(counts(&registry), (1, 2));
    drop(first);
    assert_eq!(second.waiter.state.load(Ordering::Acquire), GRANTED);
    assert_eq!(third.waiter.state.load(Ordering::Acquire), WAITING);
    // 许可已授予但接收 Future 尚未取得时取消，必须交给下一位。
    drop(second);
    let third = third.wait().await.unwrap();
    assert_eq!(counts(&registry), (1, 0));
    drop(third);
    assert_eq!(counts(&registry), (0, 0));
}

#[tokio::test]
async fn unlimited_tracks_inflight_and_limit_changes_wake_waiters() {
    let registry = registry(0);
    let a = ready(&registry);
    let b = ready(&registry);
    change_limit(&registry, 1);
    let c = queued(&registry);
    drop(a);
    assert_eq!(c.waiter.state.load(Ordering::Acquire), WAITING);
    drop(b);
    let c = c.wait().await.unwrap();
    let d = queued(&registry);
    let e = queued(&registry);
    change_limit(&registry, 0);
    let d = d.wait().await.unwrap();
    let e = e.wait().await.unwrap();
    assert_eq!(counts(&registry), (3, 0));
    drop((c, d, e));
    assert_eq!(counts(&registry), (0, 0));
}

#[tokio::test]
async fn dropping_wait_future_removes_queue_position() {
    let registry = registry(1);
    let first = ready(&registry);
    let waiting = queued(&registry);
    assert!(
        tokio::time::timeout(Duration::from_millis(5), waiting.wait())
            .await
            .is_err()
    );
    assert_eq!(counts(&registry), (1, 0));
    drop(first);
    assert_eq!(counts(&registry), (0, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn competing_tasks_never_exceed_limit_or_leak_on_abort() {
    let registry = registry(3);
    let running = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut tasks = JoinSet::new();
    for _ in 0..500 {
        let registry = Arc::clone(&registry);
        let running = Arc::clone(&running);
        let peak = Arc::clone(&peak);
        tasks.spawn(async move {
            let permit = match registry.acquire("account:a").unwrap() {
                OfficialAdmission::Ready(permit) => permit,
                OfficialAdmission::Waiting(waiter) => waiter.wait().await.unwrap(),
            };
            let active = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(active, Ordering::SeqCst);
            tokio::task::yield_now().await;
            running.fetch_sub(1, Ordering::SeqCst);
            drop(permit);
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
    assert!(peak.load(Ordering::SeqCst) <= 3);
    assert_eq!(counts(&registry), (0, 0));
    let first = ready(&registry);
    change_limit(&registry, 1);
    let queued = queued(&registry);
    let aborted = tokio::spawn(queued.wait());
    aborted.abort();
    let _ = aborted.await;
    drop(first);
    assert_eq!(counts(&registry), (0, 0));
}

#[tokio::test]
async fn http_write_half_close_does_not_cancel_admission() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (mut server, _) = listener.accept().await.unwrap();
    client.shutdown().await.unwrap();
    assert_eq!(
        await_http_admission(&mut server, async {
            tokio::time::sleep(Duration::from_millis(75)).await;
            7
        })
        .await
        .unwrap(),
        7
    );
}

#[tokio::test]
async fn http_reset_cancels_queued_request_without_upstream_work() {
    for prefix in ["none", "fin", "pipelined", "pipelined_fin"] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        if prefix.starts_with("pipelined") {
            client.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
        }
        if prefix.ends_with("fin") {
            client.shutdown().await.unwrap();
        }
        if prefix != "none" {
            let mut byte = [0];
            assert_eq!(
                server.peek(&mut byte).await.unwrap(),
                usize::from(prefix != "fin"),
                "{prefix}"
            );
        }
        let registry = registry(1);
        let first = ready(&registry);
        let waiter = queued(&registry);
        let admission = await_http_admission(&mut server, waiter.wait());
        tokio::pin!(admission);
        // 先保留 FIN / 流水线数据等待一轮监测周期，再发送 reset。
        assert!(
            tokio::time::timeout(Duration::from_millis(50), admission.as_mut())
                .await
                .is_err(),
            "{prefix}: admission ended before reset"
        );
        let mut byte = [0];
        assert!(
            tokio::time::timeout(Duration::from_millis(10), client.read(&mut byte))
                .await
                .is_err(),
            "{prefix}: admission monitoring wrote unexpected response bytes"
        );
        socket2::SockRef::from(&client)
            .set_linger(Some(Duration::ZERO))
            .unwrap();
        drop(client);
        let error = tokio::time::timeout(Duration::from_secs(1), admission)
            .await
            .unwrap_or_else(|_| panic!("{prefix}: reset did not cancel queued admission"))
            .err()
            .unwrap();
        assert!(error.is::<DownstreamClosed>(), "{prefix}: {error:#}");
        assert_eq!(counts(&registry), (1, 0), "{prefix}");
        drop(first);
    }
}

#[cfg(windows)]
#[tokio::test]
async fn http_tcp_state_distinguishes_half_close_from_reset() {
    use windows::Win32::Networking::WinSock::{TCPSTATE_CLOSE_WAIT, TCPSTATE_CLOSED};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (server, _) = listener.accept().await.unwrap();
    client.shutdown().await.unwrap();
    let mut byte = [0];
    assert_eq!(server.peek(&mut byte).await.unwrap(), 0);
    assert_eq!(
        windows_http_tcp_state(&server).unwrap(),
        Some(TCPSTATE_CLOSE_WAIT)
    );
    socket2::SockRef::from(&client)
        .set_linger(Some(Duration::ZERO))
        .unwrap();
    drop(client);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            // SO_ERROR 可以始终为空；此处直接验证 TCP 状态，不依赖 reactor 标记。
            let _ = server.take_error().unwrap();
            match windows_http_tcp_state(&server) {
                Ok(Some(TCPSTATE_CLOSED)) => break,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::NotConnected
                    ) =>
                {
                    break;
                }
                state => assert!(
                    matches!(state, Ok(Some(TCPSTATE_CLOSE_WAIT))),
                    "unexpected state after reset: {state:?}"
                ),
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("TCP state did not report reset after FIN");
}

#[cfg(windows)]
#[tokio::test]
async fn http_reset_with_empty_so_error_still_cancels_admission() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (mut server, _) = listener.accept().await.unwrap();
    socket2::SockRef::from(&client)
        .set_linger(Some(Duration::ZERO))
        .unwrap();
    drop(client);
    // 确认 Windows reactor 已收到 AFD_POLL_ABORT，随后清空 SO_ERROR。
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if server
                .ready(tokio::io::Interest::WRITABLE)
                .await
                .unwrap()
                .is_write_closed()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("Windows reactor did not report WRITE_CLOSED after reset");
    let _ = server.take_error().unwrap();
    assert!(server.take_error().unwrap().is_none());
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        await_http_admission(&mut server, std::future::pending::<()>()),
    )
    .await
    .expect("reset with empty SO_ERROR did not cancel admission")
    .unwrap_err();
    assert!(error.is::<DownstreamClosed>(), "{error:#}");
}

#[tokio::test]
async fn http_admission_preserves_pipelined_bytes() {
    for fin in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        let pipelined = b"GET / HTTP/1.1\r\n\r\n";
        client.write_all(pipelined).await.unwrap();
        if fin {
            client.shutdown().await.unwrap();
        }
        assert_eq!(
            await_http_admission(&mut server, async {
                tokio::time::sleep(Duration::from_millis(75)).await;
                7
            })
            .await
            .unwrap(),
            7
        );
        let mut received = vec![0; pipelined.len()];
        server.read_exact(&mut received).await.unwrap();
        assert_eq!(received, pipelined);
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
        server.write_all(response).await.unwrap();
        let mut received = vec![0; response.len()];
        client.read_exact(&mut received).await.unwrap();
        assert_eq!(
            received, response,
            "fin={fin}: monitoring changed the HTTP response"
        );
    }
}

#[test]
#[ignore = "manual admission overhead microbenchmark"]
fn admission_overhead_benchmark() {
    for limit in [0, 8] {
        let registry = registry(limit);
        let mut samples = Vec::new();
        for _ in 0..5 {
            let started = Instant::now();
            for _ in 0..1_000_000 {
                drop(ready(&registry));
            }
            samples.push(started.elapsed().as_nanos() as f64 / 1_000_000.0);
        }
        samples.sort_by(f64::total_cmp);
        eprintln!(
            "official admission limit={limit}, 5 x 1000000 requests: median={:.0}, min={:.0}, max={:.0} ns/request",
            samples[2], samples[0], samples[4],
        );
    }
}

fn official_config(url: String, limit: u32) -> (CodeyConfig, String, String) {
    let (mut config, provider, model) = super::super::tests::router_config(url);
    config.profiles[0].official_account = true;
    config.profiles[0].max_concurrency = limit;
    config.official_account_available_this_launch = true;
    (config, provider, model)
}

#[tokio::test]
async fn config_reload_rollback_and_removal_preserve_active_permits() {
    let (mut config, provider, _) = official_config("http://127.0.0.1:9/v1".into(), 1);
    let router = LocalRouter::start(&config).await.unwrap();
    let key = format!("provider:{provider}");
    let OfficialAdmission::Ready(first) = router.official_concurrency.acquire(&key).unwrap() else {
        panic!()
    };
    let OfficialAdmission::Waiting(second) = router.official_concurrency.acquire(&key).unwrap()
    else {
        panic!()
    };
    config.profiles[0].max_concurrency = 2;
    let swap = router.update_config(&config);
    let second = second.wait().await.unwrap();
    assert!(router.revert_config(swap));
    let OfficialAdmission::Waiting(third) = router.official_concurrency.acquire(&key).unwrap()
    else {
        panic!()
    };
    drop(first);
    assert_eq!(third.waiter.state.load(Ordering::Acquire), WAITING);
    let stale = router.update_config(&config);
    config.profiles[0].max_concurrency = 3;
    router.update_config(&config);
    assert!(!router.revert_config(stale));
    let third = third.wait().await.unwrap();
    config.profiles[0].max_concurrency = 1;
    router.update_config(&config);
    let OfficialAdmission::Waiting(removed) = router.official_concurrency.acquire(&key).unwrap()
    else {
        panic!()
    };
    config.profiles[0].enabled = false;
    router.update_config(&config);
    assert!(
        removed
            .wait()
            .await
            .err()
            .unwrap()
            .is::<OfficialRouteUnavailable>()
    );
    config.profiles[0].enabled = true;
    router.update_config(&config);
    assert_eq!(router.official_concurrency.status()[0]["running"], 2);
    drop((second, third));
    assert_eq!(router.official_concurrency.status()[0]["running"], 0);
    router.stop().await.unwrap();
}

struct MockUpstream {
    url: String,
    started: mpsc::UnboundedReceiver<String>,
    release: Arc<Semaphore>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn mock_upstream() -> MockUpstream {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (started_tx, started) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let response_release = Arc::clone(&release);
    let task = tokio::spawn(async move {
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
                connection = listener.accept() => {
                    let (mut stream, _) = connection.unwrap();
                    let release = Arc::clone(&response_release);
                    let started = started_tx.clone();
                    tasks.spawn(async move {
                        let request = read_http_request(&mut stream).await.unwrap();
                        let _ = started.send(request.path);
                        stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n").await.unwrap();
                        write_chunked_frame(&mut stream, b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"first\"}\n\n", "mock delta").await.unwrap();
                        release.acquire().await.unwrap().forget();
                        let _ = write_chunked_frame(&mut stream, b"data: {\"type\":\"response.completed\",\"response\":{\"id\":\"mock\",\"status\":\"completed\",\"output\":[]}}\n\n", "mock terminal").await;
                        let _ = finish_chunked_response(&mut stream).await;
                    });
                }
                _ = tasks.join_next(), if !tasks.is_empty() => {}
            }
        }
    });
    MockUpstream {
        url,
        started,
        release,
        task,
    }
}

fn send_request(
    router: &LocalRouter,
    model: &str,
) -> tokio::task::JoinHandle<reqwest::Result<String>> {
    let url = format!("{}/responses", router.endpoint.base_url);
    let token = router.endpoint.token.clone();
    let model = model.to_string();
    tokio::spawn(async move {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(url)
            .header(ROUTER_AUTH_HEADER, token)
            .bearer_auth("fake-official-test-token")
            .json(&json!({"model":model,"input":"hello","stream":true}))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await
    })
}

async fn wait_for_queue(router: &LocalRouter, count: usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if router
                .official_concurrency
                .status()
                .as_array()
                .unwrap()
                .iter()
                .any(|state| state["waiting"] == count)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}

async fn wait_for_idle(router: &LocalRouter) {
    // 客户端收到响应结束标记后，服务端仍可能在收尾并释放并发许可。
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if router
                .official_concurrency
                .status()
                .as_array()
                .unwrap()
                .iter()
                .all(|state| state["running"] == 0 && state["waiting"] == 0)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "official permits were not released: {}",
        router.official_concurrency.status()
    );
}

#[tokio::test]
async fn streaming_holds_permit_until_body_finishes_and_other_route_progresses() {
    let mut upstream = mock_upstream().await;
    let (mut config, provider, model) = official_config(upstream.url.clone(), 1);
    let mut other = config.profiles[0].clone();
    other.id = "route-b".into();
    other.official_account = false;
    other.max_concurrency = 1;
    config.profiles.push(other);
    config
        .selected_models_by_provider
        .insert("route-b".into(), vec!["other-model".into()]);
    let router = LocalRouter::start(&config).await.unwrap();
    let first = send_request(&router, &model_alias(&provider, &model));
    upstream.started.recv().await.unwrap();
    let second = send_request(&router, &model_alias(&provider, &model));
    wait_for_queue(&router, 1).await;
    let other = send_request(&router, "other-model");
    tokio::time::timeout(Duration::from_secs(2), upstream.started.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(!second.is_finished());
    upstream.release.add_permits(2);
    first.await.unwrap().unwrap();
    other.await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(2), upstream.started.recv())
        .await
        .unwrap()
        .unwrap();
    upstream.release.add_permits(1);
    second.await.unwrap().unwrap();
    wait_for_idle(&router).await;
    router.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sixty_fifth_request_queues_when_all_execution_slots_are_busy() {
    let mut upstream = mock_upstream().await;
    let (config, _, model) = official_config(upstream.url.clone(), 64);
    let router = LocalRouter::start(&config).await.unwrap();
    let mut requests = Vec::new();
    for _ in 0..64 {
        requests.push(send_request(&router, &model));
        tokio::time::timeout(Duration::from_secs(3), upstream.started.recv())
            .await
            .unwrap()
            .unwrap();
    }
    let last = send_request(&router, &model);
    wait_for_queue(&router, 1).await;
    assert!(!last.is_finished());
    upstream.release.add_permits(65);
    for request in requests {
        request.await.unwrap().unwrap();
    }
    last.await.unwrap().unwrap();
    wait_for_idle(&router).await;
    router.stop().await.unwrap();
}

#[tokio::test]
async fn websocket_cancel_and_interrupt_remove_waiter_even_with_no_body_budget() {
    for kind in ["response.cancel", "response.interrupt"] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let connect =
            tokio::spawn(async move { tokio_tungstenite::connect_async(url).await.unwrap().0 });
        let (server, _) = listener.accept().await.unwrap();
        let server = tokio_tungstenite::accept_async_with_config(
            WebSocketIngress::new(
                server,
                Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS)),
            ),
            Some(downstream_websocket_config()),
        )
        .await
        .unwrap();
        let mut client = connect.await.unwrap();
        let mut downstream = WebSocketResponsesDownstream::new(BudgetedWebSocket::new(server));
        downstream.request_body_budget = Arc::new(Semaphore::new(0));
        downstream
            .socket
            .set_budget(Arc::clone(&downstream.request_body_budget));
        let registry = registry(1);
        let first = ready(&registry);
        let waiter = queued(&registry);
        client
            .feed(WebSocketMessage::Ping(vec![1, 2].into()))
            .await
            .unwrap();
        client
            .send(WebSocketMessage::Text(
                json!({"type":kind}).to_string().into(),
            ))
            .await
            .unwrap();
        let error = downstream
            .wait_for_admission(waiter.wait())
            .await
            .err()
            .unwrap();
        assert!(error.is::<DownstreamResponseInterrupt>());
        assert!(
            matches!(client.next().await, Some(Ok(WebSocketMessage::Pong(payload))) if payload.as_ref() == [1, 2])
        );
        assert_eq!(counts(&registry), (1, 0));
        assert!(downstream.pending_interrupt.is_none());
        drop(first);
    }
}

#[derive(Default)]
struct AdmissionDownstream {
    error: Option<(u16, String)>,
    probe: Option<RouteRequestLogProbe>,
}

#[async_trait]
impl ResponsesDownstream for AdmissionDownstream {
    fn request_log_probe(&self) -> Option<&RouteRequestLogProbe> {
        self.probe.as_ref()
    }
    async fn write_error(
        &mut self,
        status: u16,
        code: &str,
        _: String,
        _: Option<&RouteTarget>,
    ) -> Result<()> {
        self.error = Some((status, code.into()));
        Ok(())
    }
    async fn write_json(&mut self, _: u16, _: &Value) -> Result<()> {
        unreachable!()
    }
    async fn start_event_stream(&mut self) -> Result<()> {
        unreachable!()
    }
    async fn write_event(&mut self, _: &Value) -> Result<()> {
        unreachable!()
    }
    async fn finish_event_stream(&mut self) -> Result<()> {
        unreachable!()
    }
    async fn proxy_response_with_probe(
        &mut self,
        _: reqwest::Response,
        _: Option<&RouteRequestLogProbe>,
    ) -> Result<()> {
        unreachable!()
    }
}

#[tokio::test(start_paused = true)]
async fn queue_timeout_is_distinct_and_records_wait_without_upstream_send() {
    let (config, provider, _) = official_config("http://127.0.0.1:9/v1".into(), 1);
    let server = super::super::lifecycle_tests::test_server(&config);
    let route = Arc::clone(&server.snapshot.read().unwrap().routes[&provider]);
    let mut first = AdmissionDownstream::default();
    let permit = server.admit_official(&route, &mut first).await.unwrap();
    let probe = RouteRequestLogProbe::detached_test_probe();
    let mut second = AdmissionDownstream {
        probe: Some(probe.clone()),
        ..Default::default()
    };
    let error = server
        .admit_official(&route, &mut second)
        .await
        .err()
        .unwrap();
    assert!(error.is::<AdmissionEnded>());
    assert_eq!(second.error, Some((504, "queue_wait_timeout".into())));
    // Tokio 虚拟时钟推进不推进 std::Instant，真实耗时字段仍然存在。
    assert!(probe.concurrency_wait_for_test().is_some());
    assert_eq!(server.official_concurrency.status()[0]["waiting"], 0);
    assert_eq!(server.official_concurrency.status()[0]["running"], 1);
    drop(permit);
}

#[tokio::test(start_paused = true)]
async fn global_wait_shares_original_deadline_and_releases_reserved_account() {
    let (config, provider, _) = official_config("http://127.0.0.1:9/v1".into(), 1);
    let mut server = super::super::lifecycle_tests::test_server(&config);
    server.connection_limit = Arc::new(Semaphore::new(0));
    let route = Arc::clone(&server.snapshot.read().unwrap().routes[&provider]);
    let mut downstream = AdmissionDownstream::default();
    assert!(
        server
            .admit_official(&route, &mut downstream)
            .await
            .err()
            .unwrap()
            .is::<AdmissionEnded>()
    );
    assert_eq!(server.official_concurrency.status()[0]["running"], 0);
    assert_eq!(downstream.error, Some((504, "queue_wait_timeout".into())));
}

#[tokio::test]
async fn compact_and_image_requests_share_the_generation_queue() {
    for path in ["responses/compact", "images/generations", "images/edits"] {
        let mut upstream = mock_upstream().await;
        let (mut config, _, model) = official_config(upstream.url.clone(), 1);
        config.profiles[0].supports_remote_compaction = true;
        let router = LocalRouter::start(&config).await.unwrap();
        let first = send_request(&router, &model);
        upstream.started.recv().await.unwrap();
        let url = format!("{}/{path}", router.endpoint.base_url);
        let token = router.endpoint.token.clone();
        let second = tokio::spawn(async move {
            reqwest::Client::builder()
                .no_proxy()
                .build()
                .unwrap()
                .post(url)
                .header(ROUTER_AUTH_HEADER, token)
                .bearer_auth("fake-official-test-token")
                .json(&json!({"model":model,"input":[],"prompt":"test","image":"placeholder"}))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        });
        wait_for_queue(&router, 1).await;
        assert!(!second.is_finished(), "{path} must wait rather than fail");
        upstream.release.add_permits(1);
        first.await.unwrap().unwrap();
        let received = tokio::time::timeout(Duration::from_secs(3), upstream.started.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(received.ends_with(path), "{received} vs {path}");
        upstream.release.add_permits(1);
        second.await.unwrap();
        wait_for_idle(&router).await;
        router.stop().await.unwrap();
    }
}
