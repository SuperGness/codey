use super::*;
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::AtomicUsize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::{Message, handshake::server::Request};

struct MockCdpState {
    discovery_available: AtomicBool,
    healthy: AtomicBool,
    generation: AtomicUsize,
    discovery_requests: AtomicUsize,
    health_checks: AtomicUsize,
    injections: AtomicUsize,
    close_bridges: tokio::sync::Notify,
}

struct MockCdp {
    port: u16,
    state: Arc<MockCdpState>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for MockCdp {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl MockCdp {
    async fn start() -> Self {
        let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = http_listener.local_addr().unwrap().port();
        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_address = ws_listener.local_addr().unwrap();
        let state = Arc::new(MockCdpState {
            discovery_available: AtomicBool::new(true),
            healthy: AtomicBool::new(true),
            generation: AtomicUsize::new(0),
            discovery_requests: AtomicUsize::new(0),
            health_checks: AtomicUsize::new(0),
            injections: AtomicUsize::new(0),
            close_bridges: tokio::sync::Notify::new(),
        });
        let http_state = state.clone();
        let http_task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let (mut socket, _) = http_listener.accept().await.unwrap();
                let state = http_state.clone();
                connections.spawn(async move {
                    let mut request = [0; 4096];
                    let length = socket.read(&mut request).await.unwrap();
                    // The independent asset patch service asks for a browser
                    // endpoint. This fixture only exposes the renderer under test.
                    if request[..length].starts_with(b"GET /json/version ") {
                        let _ = socket
                            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                        return;
                    }
                    state.discovery_requests.fetch_add(1, Ordering::SeqCst);
                    let (status, body) = if state.discovery_available.load(Ordering::SeqCst) {
                        let generation = state.generation.load(Ordering::SeqCst);
                        (
                            "200 OK",
                            serde_json::json!([{
                                "id": format!("page-{generation}"),
                                "type": "page",
                                "title": "Codex",
                                "url": "app://-/index.html",
                                "webSocketDebuggerUrl": format!("ws://{ws_address}/devtools/page/{generation}"),
                            }])
                            .to_string(),
                        )
                    } else {
                        ("503 Service Unavailable", String::new())
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len(),
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
                while connections.try_join_next().is_some() {}
            }
        });
        let ws_state = state.clone();
        let ws_task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let (socket, _) = ws_listener.accept().await.unwrap();
                let state = ws_state.clone();
                connections.spawn(async move {
                    let mut path = String::new();
                    #[allow(
                        clippy::result_large_err,
                        reason = "tungstenite 握手回调要求直接返回 HTTP 响应，无法改用装箱错误"
                    )]
                    let Ok(mut socket) = tokio_tungstenite::accept_hdr_async(
                        socket,
                        |request: &Request, response| {
                            path = request.uri().path().to_string();
                            Ok(response)
                        },
                    )
                    .await
                    else {
                        return;
                    };
                    let mut bridge = false;
                    loop {
                        // Dropping the socket without a close frame mimics
                        // Codex resetting the persistent bridge connection.
                        let message = tokio::select! {
                            message = socket.next() => message,
                            _ = state.close_bridges.notified(), if bridge => return,
                        };
                        let Some(Ok(message)) = message else {
                            return;
                        };
                        if message.is_close() {
                            let _ = socket.close(None).await;
                            return;
                        }
                        let Ok(text) = message.to_text() else {
                            continue;
                        };
                        let command: serde_json::Value = serde_json::from_str(text).unwrap();
                        let is_health_check = command["method"] == "Runtime.evaluate"
                            && command["params"]["expression"]
                                == codey_runtime_core::bridge::bridge_health_check_script();
                        let result = match command["method"].as_str() {
                            Some("Runtime.addBinding") => {
                                bridge = true;
                                state.injections.fetch_add(1, Ordering::SeqCst);
                                serde_json::json!({})
                            }
                            Some("Page.addScriptToEvaluateOnNewDocument") => {
                                serde_json::json!({"identifier": command["id"].to_string()})
                            }
                            Some("Runtime.evaluate") => {
                                let value = if is_health_check {
                                    let current_path = format!(
                                        "/devtools/page/{}",
                                        state.generation.load(Ordering::SeqCst)
                                    );
                                    if state.healthy.load(Ordering::SeqCst) && path == current_path
                                    {
                                        "healthy".to_string()
                                    } else {
                                        "unhealthy".to_string()
                                    }
                                } else {
                                    serde_json::json!({
                                        "href": "app://-/index.html",
                                        "bridge": true,
                                        "overlay": true,
                                    })
                                    .to_string()
                                };
                                serde_json::json!({"result": {"type": "string", "value": value}})
                            }
                            _ => serde_json::json!({}),
                        };
                        if socket
                            .send(Message::Text(
                                serde_json::json!({"id": command["id"], "result": result})
                                    .to_string()
                                    .into(),
                            ))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        if is_health_check {
                            // Count completed probes after the client consumes
                            // the reply and drops its diagnostic connection.
                            let _ = socket.next().await;
                            state.health_checks.fetch_add(1, Ordering::SeqCst);
                            return;
                        }
                    }
                });
                while connections.try_join_next().is_some() {}
            }
        });
        Self {
            port,
            state,
            tasks: vec![http_task, ws_task],
        }
    }

    async fn watchdog(&self) -> InjectionWatchdog {
        let scripts = cdp::prepare_injection_scripts(false, false, false, &[]);
        let handler = cdp::bridge_handler(|_, _| async { serde_json::Value::Null });
        let initial = cdp::retry_inject_with_scripts(
            self.port,
            handler.clone(),
            &scripts,
            cdp::InjectionRetrySource::Startup,
            None,
        )
        .await
        .expect("initial mock renderer injection");
        let watchdog = spawn_injection_watchdog(initial, self.port, handler, scripts);
        // Let the spawned task register its initial timer before pausing time.
        tokio::time::sleep(Duration::from_millis(10)).await;
        watchdog
    }
}

// Keep the executor runnable while the clock is paused so socket and blocking
// log IO cannot cause Tokio to automatically skip a watchdog deadline.
struct ManualClock(tokio::task::JoinHandle<()>);

impl ManualClock {
    fn pause() -> Self {
        tokio::time::pause();
        Self(tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        }))
    }
}

impl Drop for ManualClock {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "mock CDP operation did not settle"
        );
        tokio::task::yield_now().await;
    }
}

async fn tick_health(mock: &MockCdp, interval: Duration) {
    let previous = mock.state.health_checks.load(Ordering::SeqCst);
    // Tokio rounds timer deadlines to milliseconds; include that resolution.
    tokio::time::advance(interval + Duration::from_millis(1)).await;
    wait_until(|| mock.state.health_checks.load(Ordering::SeqCst) > previous).await;
}

async fn fail_reinjection(mock: &MockCdp, watchdog: &InjectionWatchdog, interval: Duration) {
    let previous = watchdog.statuses.read().await.clone();
    let discoveries = mock.state.discovery_requests.load(Ordering::SeqCst);
    tick_health(mock, interval).await;
    wait_until(|| mock.state.discovery_requests.load(Ordering::SeqCst) > discoveries).await;
    tokio::time::advance(cdp::PREFETCH_BUDGET + Duration::from_millis(1)).await;
    wait_until(|| {
        watchdog
            .statuses
            .try_read()
            .is_ok_and(|current| !Arc::ptr_eq(&current, &previous))
    })
    .await;
}

async fn enter_backoff(mock: &MockCdp, watchdog: &InjectionWatchdog) {
    mock.state
        .discovery_available
        .store(false, Ordering::SeqCst);
    mock.state.healthy.store(false, Ordering::SeqCst);
    // The live pump first needs two unhealthy verdicts to start recovery.
    tick_health(mock, CDP_WATCHDOG_INTERVAL).await;
    for _ in 0..CDP_WATCHDOG_ENDPOINT_LOSS_LIMIT {
        fail_reinjection(mock, watchdog, CDP_WATCHDOG_INTERVAL).await;
    }
    assert!(
        !watchdog.task.is_finished(),
        "endpoint loss stopped recovery"
    );
    assert!(watchdog.statuses.read().await.iter().all(|status| {
        status.error.as_deref() == Some("Codex 调试连接暂时不可用，Codey 将继续自动恢复")
    }));
}

async fn stop_watchdog(watchdog: InjectionWatchdog) {
    let started = tokio::time::Instant::now();
    watchdog.shutdown.send(()).expect("watchdog still running");
    wait_until(|| watchdog.task.is_finished()).await;
    watchdog.task.await.unwrap();
    assert_eq!(
        tokio::time::Instant::now(),
        started,
        "shutdown waited for a timer"
    );
}

#[tokio::test]
async fn cdp_watchdog_recovers_after_repeated_endpoint_loss_and_resets_interval() {
    let mock = MockCdp::start().await;
    let watchdog = mock.watchdog().await;
    let _clock = ManualClock::pause();
    tokio::task::yield_now().await;
    enter_backoff(&mock, &watchdog).await;

    let checks = mock.state.health_checks.load(Ordering::SeqCst);
    tokio::time::advance(CDP_WATCHDOG_INTERVAL).await;
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert_eq!(mock.state.health_checks.load(Ordering::SeqCst), checks);
    // Complete a fourth failed recovery cycle beyond the old terminal limit.
    fail_reinjection(
        &mock,
        &watchdog,
        CDP_WATCHDOG_ENDPOINT_RETRY_INTERVAL - CDP_WATCHDOG_INTERVAL,
    )
    .await;
    assert!(!watchdog.task.is_finished());

    mock.state.generation.store(1, Ordering::SeqCst);
    mock.state.healthy.store(true, Ordering::SeqCst);
    mock.state.discovery_available.store(true, Ordering::SeqCst);
    tick_health(&mock, CDP_WATCHDOG_ENDPOINT_RETRY_INTERVAL).await;
    wait_until(|| {
        watchdog
            .websocket_url
            .try_read()
            .is_ok_and(|url| url.ends_with("/devtools/page/1"))
    })
    .await;
    assert_eq!(mock.state.injections.load(Ordering::SeqCst), 2);
    // Wait for closing the previous pump before advancing the next interval.
    for _ in 0..1000 {
        tokio::task::yield_now().await;
    }
    tick_health(&mock, CDP_WATCHDOG_INTERVAL).await;
    stop_watchdog(watchdog).await;
}

#[tokio::test]
async fn cdp_watchdog_rebuilds_a_closed_bridge_pump_without_waiting_for_the_interval() {
    let mock = MockCdp::start().await;
    let watchdog = mock.watchdog().await;
    let _clock = ManualClock::pause();
    tokio::task::yield_now().await;

    mock.state.close_bridges.notify_waiters();
    // The paused clock never reaches the watchdog interval, so only the pump
    // closure itself can trigger this reinjection.
    wait_until(|| mock.state.injections.load(Ordering::SeqCst) == 2).await;
    assert_eq!(mock.state.health_checks.load(Ordering::SeqCst), 0);
    for _ in 0..1000 {
        tokio::task::yield_now().await;
    }
    tick_health(&mock, CDP_WATCHDOG_INTERVAL).await;
    assert_eq!(mock.state.injections.load(Ordering::SeqCst), 2);
    stop_watchdog(watchdog).await;
}

#[tokio::test]
async fn cdp_watchdog_confirms_an_unhealthy_bridge_without_waiting_a_full_interval() {
    let mock = MockCdp::start().await;
    let watchdog = mock.watchdog().await;
    let _clock = ManualClock::pause();
    tokio::task::yield_now().await;

    mock.state.healthy.store(false, Ordering::SeqCst);
    tick_health(&mock, CDP_WATCHDOG_INTERVAL).await;
    assert_eq!(mock.state.injections.load(Ordering::SeqCst), 1);
    tick_health(&mock, CDP_WATCHDOG_UNHEALTHY_CONFIRM_INTERVAL).await;
    wait_until(|| mock.state.injections.load(Ordering::SeqCst) == 2).await;
    stop_watchdog(watchdog).await;
}

#[tokio::test]
async fn cdp_watchdog_healthy_target_resets_backoff_without_reinjection() {
    let mock = MockCdp::start().await;
    let watchdog = mock.watchdog().await;
    let initial_statuses = watchdog.statuses.read().await.clone();
    let _clock = ManualClock::pause();
    tokio::task::yield_now().await;
    enter_backoff(&mock, &watchdog).await;

    mock.state.healthy.store(true, Ordering::SeqCst);
    tick_health(&mock, CDP_WATCHDOG_ENDPOINT_RETRY_INTERVAL).await;
    wait_until(|| {
        watchdog
            .statuses
            .try_read()
            .is_ok_and(|current| Arc::ptr_eq(&current, &initial_statuses))
    })
    .await;
    tick_health(&mock, CDP_WATCHDOG_INTERVAL).await;
    assert_eq!(mock.state.injections.load(Ordering::SeqCst), 1);
    stop_watchdog(watchdog).await;
}

#[tokio::test]
async fn cdp_watchdog_shutdown_cancels_backoff_wait() {
    let mock = MockCdp::start().await;
    let watchdog = mock.watchdog().await;
    let _clock = ManualClock::pause();
    tokio::task::yield_now().await;
    enter_backoff(&mock, &watchdog).await;
    stop_watchdog(watchdog).await;
}

#[tokio::test]
async fn cdp_watchdog_shutdown_cancels_active_recovery() {
    let mock = MockCdp::start().await;
    let watchdog = mock.watchdog().await;
    let _clock = ManualClock::pause();
    tokio::task::yield_now().await;
    enter_backoff(&mock, &watchdog).await;
    let discoveries = mock.state.discovery_requests.load(Ordering::SeqCst);
    tick_health(&mock, CDP_WATCHDOG_ENDPOINT_RETRY_INTERVAL).await;
    wait_until(|| mock.state.discovery_requests.load(Ordering::SeqCst) > discoveries).await;
    stop_watchdog(watchdog).await;
}
