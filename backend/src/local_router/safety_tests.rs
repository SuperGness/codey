use super::tests::{local_websocket_pair, router_config};
use super::*;

const TERMINAL: &[u8] = b"data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"status\":\"completed\",\"output\":[]}}\n\n";

async fn upstream_response(
    body: Vec<u8>,
    hold_open: bool,
) -> (reqwest::Response, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_http_request(&mut socket).await.unwrap();
        socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n").await.unwrap();
        write_chunked_frame(&mut socket, &body, "test response")
            .await
            .unwrap();
        if hold_open {
            std::future::pending::<()>().await;
        }
        socket.write_all(b"0\r\n\r\n").await.unwrap();
    });
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("http://{address}/responses"))
        .send()
        .await
        .unwrap();
    (response, task)
}

async fn tcp_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (server, _) = listener.accept().await.unwrap();
    (server, client)
}

#[test]
fn native_sse_completion_handles_fragmentation_and_rejects_false_completion() {
    for size in 1..=TERMINAL.len() {
        let mut tracker = NativeSseTerminal::default();
        for chunk in TERMINAL.chunks(size) {
            tracker.observe(chunk).unwrap();
        }
        tracker.finish().unwrap();
    }
    for incomplete in [
        b"data: {\"type\":\"response.created\"}\n\n".as_slice(),
        b"data: [DONE]\n\n",
    ] {
        let mut tracker = NativeSseTerminal::default();
        assert!(tracker.observe(incomplete).is_err() || tracker.finish().is_err());
    }
    let mut tracker = NativeSseTerminal::default();
    tracker.observe(&TERMINAL[..TERMINAL.len() - 2]).unwrap();
    tracker.finish().unwrap();

    // Native completed events can include the full response, larger than the
    // adapted stream's per-frame limit. Fragmentation must remain linear.
    let large = format!(
        "data: {{\"type\":\"response.completed\",\"payload\":\"{}\"}}\n\n",
        "x".repeat(MAX_UPSTREAM_SSE_BUFFER_BYTES + 1)
    );
    let mut tracker = NativeSseTerminal::default();
    for chunk in large.as_bytes().chunks(4096) {
        tracker.observe(chunk).unwrap();
    }
    tracker.finish().unwrap();
}

#[tokio::test]
async fn both_native_transports_finish_without_waiting_for_http_eof() {
    let (response, upstream) = upstream_response(TERMINAL.to_vec(), true).await;
    let (mut writer, mut reader) = tcp_pair().await;
    let proxy =
        tokio::spawn(async move { write_proxy_response(&mut writer, response, None, true).await });
    let mut body = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), reader.read_to_end(&mut body))
        .await
        .unwrap()
        .unwrap();
    proxy.await.unwrap().unwrap();
    assert!(body.ends_with(b"0\r\n\r\n"));
    upstream.abort();

    let (response, upstream) = upstream_response(TERMINAL.to_vec(), true).await;
    let (socket, mut peer) = local_websocket_pair().await;
    let proxy = tokio::spawn(async move {
        WebSocketResponsesDownstream::new(socket)
            .proxy_response(response)
            .await
    });
    let event = tokio::time::timeout(Duration::from_secs(2), peer.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(event, WebSocketMessage::Text(_)));
    tokio::time::timeout(Duration::from_secs(2), proxy)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    upstream.abort();
}

#[tokio::test]
async fn truncated_native_sse_does_not_emit_a_clean_http_end() {
    let (response, upstream) =
        upstream_response(b"data: {\"type\":\"response.created\"}\n\n".to_vec(), false).await;
    let (mut writer, mut reader) = tcp_pair().await;
    let proxy =
        tokio::spawn(async move { write_proxy_response(&mut writer, response, None, true).await });
    let mut body = Vec::new();
    reader.read_to_end(&mut body).await.unwrap();
    assert!(proxy.await.unwrap().is_err());
    assert!(!body.ends_with(b"0\r\n\r\n"));
    upstream.await.unwrap();
}

#[tokio::test]
async fn cumulative_stream_limit_is_not_reset_by_small_complete_frames() {
    let (response, upstream) = upstream_response(TERMINAL.to_vec(), false).await;
    let mut prepared = prepare_upstream_response(response, "test", None)
        .await
        .unwrap();
    prepared.bytes_read = MAX_UPSTREAM_RESPONSE_BYTES - 1;
    prepared.retained = Some(RetainedMemoryBudget::default());
    let error = read_prepared_upstream_chunk(&mut prepared, "test", None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("累计"));
    prepared.bytes_read = 0;
    prepared.deadline = tokio::time::Instant::now() - Duration::from_secs(1);
    prepared.prefix.push_back(Bytes::from_static(TERMINAL));
    let error = read_prepared_upstream_chunk(&mut prepared, "test", None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("总时限"));
    upstream.await.unwrap();
}

#[test]
fn history_rejects_oversize_without_mutating_input_and_requires_known_continuation() {
    let mut history = AdaptedResponsesHistory::default();
    let mut body = json!({"input": "hello"});
    history.prepare(&mut body).unwrap();
    history
        .remember(
            "resp_codey_known",
            &[json!({"role":"assistant","content":"ok"})],
        )
        .unwrap();
    let mut unknown = json!({"input":"next", "previous_response_id":"resp_codey_unknown"});
    assert!(history.prepare(&mut unknown).is_err());
    assert_eq!(unknown["input"], "next");
    let mut oversized =
        json!({"input":"x".repeat(MAX_REQUEST_BYTES), "previous_response_id":"resp_codey_known"});
    assert!(history.prepare(&mut oversized).is_err());
    assert_eq!(oversized["previous_response_id"], "resp_codey_known");
    let mut continuation = json!({"input":"next", "previous_response_id":"resp_codey_known"});
    assert!(history.prepare(&mut continuation).unwrap());
    assert_eq!(continuation["input"][0], "hello");
    history.clear_pending();
    assert!(history.pending_input.is_none());
    assert!(history.last.is_some());
}

#[tokio::test(start_paused = true)]
async fn idle_reclamation_preserves_stateful_continuations() {
    let (socket, mut client) = local_websocket_pair().await;
    let mut downstream = WebSocketResponsesDownstream::new(socket);
    downstream
        .adapted_history
        .prepare(&mut json!({"input":"hello"}))
        .unwrap();
    downstream
        .adapted_history
        .remember("resp_codey_saved", &[])
        .unwrap();
    let waiting = tokio::spawn(async move { downstream.next_message().await });
    tokio::task::yield_now().await;
    tokio::time::advance(DOWNSTREAM_WEBSOCKET_IDLE_TIMEOUT + Duration::from_secs(1)).await;
    assert!(!waiting.is_finished());
    client
        .send(WebSocketMessage::Text("continue".into()))
        .await
        .unwrap();
    assert!(waiting.await.unwrap().unwrap().is_some());
}

#[tokio::test(start_paused = true)]
async fn http_stream_heartbeat_does_not_extend_the_upstream_deadline() {
    let (mut writer, mut reader) = tcp_pair().await;
    let waiting = tokio::spawn(async move {
        await_http_stream_upstream(&mut writer, async {
            tokio::time::sleep(Duration::from_secs(40)).await;
            "finished"
        })
        .await
        .unwrap()
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(16)).await;
    let mut chunk = [0; 128];
    let count = reader.read(&mut chunk).await.unwrap();
    assert!(
        std::str::from_utf8(&chunk[..count])
            .unwrap()
            .contains(": keep-alive")
    );
    tokio::time::advance(Duration::from_secs(25)).await;
    assert_eq!(waiting.await.unwrap(), "finished");
}

#[test]
fn unknown_websocket_capability_has_one_probe_and_cancellation_releases_it() {
    let shared = Arc::new(Mutex::new(UpstreamWebSocketBackoffs::default()));
    let key = UpstreamWebSocketBackoffKey::new("route", "ws://test/responses", Default::default());
    let first = UpstreamWebSocketProbe::acquire(&shared, &key).unwrap();
    assert!(UpstreamWebSocketProbe::acquire(&shared, &key).is_none());
    drop(first);
    let second = UpstreamWebSocketProbe::acquire(&shared, &key).unwrap();
    shared.lock().unwrap().record_success(&key);
    assert!(UpstreamWebSocketProbe::acquire(&shared, &key).is_some());
    drop(second);
    let known = UpstreamWebSocketProbe::acquire(&shared, &key).unwrap();
    shared
        .lock()
        .unwrap()
        .record_failure(key.clone(), Instant::now());
    // Acquisition must recheck cooldown under the same lock as probe ownership.
    assert!(UpstreamWebSocketProbe::acquire(&shared, &key).is_none());
    shared.lock().unwrap().entries.get_mut(&key).unwrap().until = Instant::now();
    let retry = UpstreamWebSocketProbe::acquire(&shared, &key).unwrap();
    drop(known);
    assert!(UpstreamWebSocketProbe::acquire(&shared, &key).is_none());
    drop(retry);
    assert!(UpstreamWebSocketProbe::acquire(&shared, &key).is_some());
}

#[test]
fn websocket_config_tracks_custom_identity_but_not_display_changes() {
    let (mut config, id, _) = router_config("http://127.0.0.1:9/v1".into());
    config.profiles[0].supports_websockets = true;
    let initial = RouterSnapshot::from_config(&config);
    let old = initial.routes[&id].websocket_config;
    config.profiles[0].name = "new label".into();
    assert_eq!(
        RouterSnapshot::from_config(&config).routes[&id].websocket_config,
        old
    );
    config.profiles[0]
        .model_request_headers
        .insert("x-tenant".into(), "tenant-b".into());
    assert_ne!(
        RouterSnapshot::from_config(&config).routes[&id].websocket_config,
        old
    );
}

#[test]
fn stale_probe_cannot_release_a_probe_after_route_is_reenabled() {
    let (mut config, id, _) = router_config("http://127.0.0.1:9/v1".into());
    config.profiles[0].supports_websockets = true;
    let snapshot = RouterSnapshot::from_config(&config);
    let shared = Arc::new(Mutex::new(UpstreamWebSocketBackoffs::default()));
    shared.lock().unwrap().update_routes(&snapshot);
    let key = UpstreamWebSocketBackoffKey::for_route(
        &snapshot.routes[&id],
        "ws://127.0.0.1:9/v1/responses",
        Default::default(),
    );
    let old = UpstreamWebSocketProbe::acquire(&shared, &key).unwrap();
    config.profiles[0].supports_websockets = false;
    shared
        .lock()
        .unwrap()
        .update_routes(&RouterSnapshot::from_config(&config));
    assert!(UpstreamWebSocketProbe::acquire(&shared, &key).is_none());
    shared.lock().unwrap().update_routes(&snapshot);
    let current = UpstreamWebSocketProbe::acquire(&shared, &key).unwrap();
    drop(old);
    assert!(UpstreamWebSocketProbe::acquire(&shared, &key).is_none());
    drop(current);
    assert!(UpstreamWebSocketProbe::acquire(&shared, &key).is_some());
}

#[tokio::test]
async fn disabling_route_releases_idle_cached_socket_without_another_request() {
    let (mut config, id, _) = router_config("http://127.0.0.1:9/v1".into());
    config.profiles[0].supports_websockets = true;
    let snapshot = RouterSnapshot::from_config(&config);
    let shared = Arc::new(Mutex::new(UpstreamWebSocketBackoffs::default()));
    shared.lock().unwrap().update_routes(&snapshot);
    let (socket, mut client) = local_websocket_pair().await;
    let (mut upstream_peer, upstream_socket) = local_websocket_pair().await;
    let mut downstream = WebSocketResponsesDownstream::with_shared_backoffs(
        socket,
        Arc::clone(&shared),
        Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS)),
    );
    downstream.upstream = Some(CachedUpstreamWebSocket {
        route_id: id.clone(),
        url: "ws://127.0.0.1:9/v1/responses".into(),
        auth_identity: Default::default(),
        config_identity: snapshot.routes[&id].websocket_config,
        response_ids: VecDeque::new(),
        liveness: UpstreamWebSocketLiveness::new(Instant::now()),
        socket: upstream_socket,
    });
    let waiting = tokio::spawn(async move {
        let result = downstream.next_message().await;
        (result, downstream)
    });
    tokio::task::yield_now().await;
    config.profiles[0].supports_websockets = false;
    shared
        .lock()
        .unwrap()
        .update_routes(&RouterSnapshot::from_config(&config));
    let closed = tokio::time::timeout(Duration::from_secs(2), upstream_peer.next())
        .await
        .unwrap();
    assert!(closed.is_none() || closed.unwrap().is_err());
    client
        .send(WebSocketMessage::Text("next".into()))
        .await
        .unwrap();
    let (result, downstream) = waiting.await.unwrap();
    assert!(result.unwrap().is_some());
    assert!(downstream.upstream.is_none());
}
