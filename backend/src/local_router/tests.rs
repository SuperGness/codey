use super::*;
use crate::config::ProviderProfile;

#[test]
fn request_log_catalog_exposes_login_status_independently_of_profiles() {
    let mut official = ProviderProfile::new("OpenAI 官方直登");
    official.source_provider_id = Some("openai".into());
    official.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.into();
    official.normalize();
    let mut config = CodeyConfig {
        profiles: vec![official, ProviderProfile::new("Third party")],
        ..CodeyConfig::default()
    };
    let response = serde_json::to_value(RequestLogCatalog::from_config(&config)).unwrap();
    assert_eq!(response["officialAccountAvailable"], false);
    assert_eq!(response["profiles"][0]["sourceProviderId"], "openai");
    assert!(response["profiles"][0].get("apiKey").is_none());
    config.official_account_available_this_launch = true;
    config.profiles.clear();
    let response = serde_json::to_value(RequestLogCatalog::from_config(&config)).unwrap();
    assert_eq!(response["officialAccountAvailable"], true);
}

#[test]
fn disabled_route_has_no_request_target() {
    let mut route = ProviderProfile::new("Disabled");
    route.id = "disabled".into();
    route.base_url = "https://example.com/v1".into();
    route.enabled = false;
    let mut config = CodeyConfig {
        profiles: vec![route],
        ..CodeyConfig::default()
    };
    config
        .selected_models_by_provider
        .insert("disabled".into(), vec!["model".into()]);
    let snapshot = RouterSnapshot::from_config(&config);
    assert!(snapshot.routes.is_empty());
    assert!(
        snapshot
            .target_for_request("disabled/model", None, None)
            .is_err()
    );
}

#[test]
fn fragmented_sse_preserves_frames_tail_and_scan_progress() {
    for large in [false, true] {
        let first = if large {
            "x".repeat(70 * 1024)
        } else {
            "中文🙂".into()
        };
        let source = format!("data: {first}\r\n\r\ndata: second\n\n: heartbeat\r\n\r\ndata: tail");
        for chunk_size in [1, 2, 3, 4, 7, 127, 4096, 65536] {
            let mut buffer = Vec::new();
            let mut cursor = SseCursor::default();
            let mut frames = Vec::new();
            for chunk in source.as_bytes().chunks(chunk_size) {
                compact_sse_buffer(&mut buffer, &mut cursor);
                buffer.extend_from_slice(chunk);
                while let Some(frame) = take_next_sse_frame(&buffer, &mut cursor) {
                    frames.push(frame.to_vec());
                }
                assert!(cursor.scanned >= buffer.len().saturating_sub(3));
                assert!(cursor.scanned >= cursor.consumed);
            }
            assert_eq!(
                frames,
                vec![
                    format!("data: {first}").into_bytes(),
                    b"data: second".to_vec(),
                    b": heartbeat".to_vec()
                ]
            );
            assert_eq!(&buffer[cursor.consumed..], b"data: tail");
            buffer.extend_from_slice(b"\n\n");
            assert_eq!(
                take_next_sse_frame(&buffer, &mut cursor),
                Some(b"data: tail".as_slice())
            );
            compact_sse_buffer(&mut buffer, &mut cursor);
            assert!(buffer.is_empty());
            assert_eq!(cursor.scanned, 0);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn downstream_backpressure_times_out_and_closed_reader_fails() {
    let (mut writer, reader) = tokio::io::duplex(1);
    let start = tokio::time::Instant::now();
    let error = write_all_with_timeout(&mut writer, b"too large", "test write")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("超过写入期限"));
    assert_eq!(start.elapsed(), DOWNSTREAM_WRITE_TIMEOUT);
    drop(reader);
    let error = write_all_with_timeout(&mut writer, b"x", "closed reader")
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<std::io::Error>().is_some());
    assert_eq!(start.elapsed(), DOWNSTREAM_WRITE_TIMEOUT);
}

#[tokio::test]
async fn upstream_body_idle_timeout_is_bounded() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let (release, wait) = oneshot::channel::<()>();
    let mock = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_http_request(&mut socket).await.unwrap();
        socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n").await.unwrap();
        let _ = wait.await;
    });
    let mut response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("http://{address}/responses"))
        .send()
        .await
        .unwrap();
    tokio::time::pause();
    let start = tokio::time::Instant::now();
    let error = read_upstream_chunk(&mut response, "idle test", None)
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<UpstreamReadIdleTimeout>().is_some());
    // Tokio timers round deadlines up to the next millisecond.
    assert!(start.elapsed() >= UPSTREAM_READ_IDLE_TIMEOUT);
    assert!(start.elapsed() <= UPSTREAM_READ_IDLE_TIMEOUT + Duration::from_millis(2));
    drop(release);
    mock.await.unwrap();
}

#[test]
fn sse_sniffer_handles_fragmented_and_mislabeled_prefixes() {
    assert_eq!(classify_upstream_sse_prefix(b"d"), None);
    assert_eq!(classify_upstream_sse_prefix(b"data"), None);
    assert_eq!(classify_upstream_sse_prefix(b"data: {}\n\n"), Some(true));
    assert_eq!(
        classify_upstream_sse_prefix(b"\xef\xbb\xbf event: message\n"),
        Some(true)
    );
    assert_eq!(classify_upstream_sse_prefix(br#"{"ok":true}"#), Some(false));
}

#[test]
fn downstream_content_timing_ignores_control_events() {
    assert!(!responses_event_has_user_content(
        &json!({"type":"response.created"})
    ));
    assert!(!responses_event_has_user_content(
        &json!({"type":"response.output_text.delta","delta":""})
    ));
    assert!(responses_event_has_user_content(
        &json!({"type":"response.output_text.delta","delta":"hello"})
    ));
}

#[test]
fn request_log_projector_waits_for_a_nonempty_content_delta() {
    let probe = RouteRequestLogProbe::detached_test_probe();
    let mut projector = RequestLogMetadataProjector::default();
    projector
        .observe(
            br#"data: {"type":"response.created"}\n\ndata: {"type":"response.output_text.delta","delta":""}\n\n"#,
            &probe,
            Instant::now(),
        )
        .unwrap();
    assert!(!probe.downstream_content_observed_for_test());

    for chunk in br#"data: {"delta":"hello","type":"response.output_text.delta"}\n\n"#.chunks(7) {
        projector.observe(chunk, &probe, Instant::now()).unwrap();
    }
    assert!(probe.downstream_content_observed_for_test());
}

#[test]
fn custom_tool_bridge_description_is_bounded_without_embedding_the_definition() {
    let tool = json!({
        "type":"custom",
        "name":"apply_patch",
        "description":"d".repeat(MAX_CUSTOM_TOOL_BRIDGE_DESCRIPTION_BYTES * 2),
        "format":{"type":"grammar","syntax":"lark","definition":"start: WORD"},
        "unrelated":"must-not-be-forwarded",
    });
    let description = custom_tool_bridge_description(tool.as_object().unwrap()).unwrap();
    assert!(description.len() <= MAX_CUSTOM_TOOL_BRIDGE_DESCRIPTION_BYTES);
    assert!(description.contains("Codey compatibility bridge"));
    assert!(description.contains("\"syntax\":\"lark\""));
    assert!(!description.contains("must-not-be-forwarded"));
}

#[tokio::test]
async fn large_request_json_is_parsed_off_the_router_worker() {
    let encoded = serde_json::to_vec(&json!({
        "model":"test-model",
        "input":"x".repeat(REQUEST_JSON_OFFLOAD_BYTES),
    }))
    .unwrap();
    let budget = Arc::new(Semaphore::new(REQUEST_BODY_BUDGET_PERMITS));
    let permit = acquire_request_body_budget(&budget, encoded.len()).unwrap();
    let held = permit.as_ref().unwrap().num_permits();
    let (encoded, parsed, permit) = parse_responses_request_body(encoded, permit).await.unwrap();
    assert!(encoded.len() >= REQUEST_JSON_OFFLOAD_BYTES);
    assert_eq!(parsed.unwrap()["model"], "test-model");
    assert_eq!(
        budget.available_permits(),
        REQUEST_BODY_BUDGET_PERMITS - held
    );
    drop((encoded, permit));
    assert_eq!(budget.available_permits(), REQUEST_BODY_BUDGET_PERMITS);
}

#[test]
fn request_log_projector_extracts_usage_after_large_terminal_payload() {
    let probe = RouteRequestLogProbe::detached_test_probe();
    let mut projector = RequestLogMetadataProjector::default();
    let event = format!(
        "data: {{\"type\":\"response.completed\",\"response\":{{\"output\":[{{\"text\":\"{}\"}}],\"usage\":{{\"input_tokens\":11,\"output_tokens\":7,\"total_tokens\":18}}}}}}\n\n",
        "x".repeat(128 * 1024)
    );
    for chunk in event.as_bytes().chunks(997) {
        projector.observe(chunk, &probe, Instant::now()).unwrap();
    }
    let usage = probe.token_usage_for_test();
    assert_eq!(usage.input_tokens, Some(11));
    assert_eq!(usage.output_tokens, Some(7));
    assert_eq!(usage.total_tokens, Some(18));
}

#[test]
fn request_log_projector_skips_large_unknown_usage_values() {
    let probe = RouteRequestLogProbe::detached_test_probe();
    let mut projector = RequestLogMetadataProjector::default();
    let event = format!(
        "{{\"usage\":{{\"padding\":[{{\"nested\":[{}0]}}],\"input_tokens\":11,\"output_tokens\":7,\"input_tokens_details\":{{\"cached_tokens\":3}},\"cache_creation_input_tokens\":2,\"output_tokens_details\":{{\"reasoning_tokens\":4}},\"total_tokens\":18}}}}",
        "0,".repeat(128 * 1024)
    );
    for chunk in event.as_bytes().chunks(997) {
        projector.observe(chunk, &probe, Instant::now()).unwrap();
    }

    let usage = probe.token_usage_for_test();
    assert_eq!(usage.input_tokens, Some(11));
    assert_eq!(usage.output_tokens, Some(7));
    assert_eq!(usage.cached_input_tokens, Some(3));
    assert_eq!(usage.cache_creation_input_tokens, Some(2));
    assert_eq!(usage.reasoning_output_tokens, Some(4));
    assert_eq!(usage.total_tokens, Some(18));
}

#[test]
fn request_log_projector_ignores_large_usage_strings() {
    let probe = RouteRequestLogProbe::detached_test_probe();
    let mut projector = RequestLogMetadataProjector::default();
    let event = format!(
        "{{\"usage\":{{\"padding\":\"{}\",\"input_tokens\":11,\"output_tokens\":7,\"total_tokens\":18}}}}",
        "x".repeat(128 * 1024)
    );
    for chunk in event.as_bytes().chunks(997) {
        projector.observe(chunk, &probe, Instant::now()).unwrap();
    }

    let usage = probe.token_usage_for_test();
    assert_eq!(usage.input_tokens, Some(11));
    assert_eq!(usage.output_tokens, Some(7));
    assert_eq!(usage.total_tokens, Some(18));
}

#[test]
fn request_log_projector_preserves_terminal_status_and_error_metadata() {
    let probe = RouteRequestLogProbe::detached_test_probe();
    let mut projector = RequestLogMetadataProjector::default();
    let event = br#"data: {"type":"response.failed","response":{"status":"failed","output":[],"error":{"code":"quota_exhausted"}}}\n\n"#;
    for chunk in event.chunks(13) {
        projector.observe(chunk, &probe, Instant::now()).unwrap();
    }

    let (status, error_code, unavailable_reason) = probe.projected_metadata_for_test();
    assert_eq!(status.as_deref(), Some("failed"));
    assert_eq!(error_code.as_deref(), Some("quota_exhausted"));
    assert_eq!(unavailable_reason, None);
}

#[test]
fn request_log_response_tap_accepts_one_large_upstream_chunk() {
    let probe = RouteRequestLogProbe::detached_test_probe();
    let (sender, _receiver) =
        mpsc::channel::<RequestLogObservedChunk>(REQUEST_LOG_TAP_QUEUE_CHUNKS);
    let mut tap = RequestLogResponseTap {
        sender: Some(sender),
        queue_budget: Arc::new(Semaphore::new(REQUEST_LOG_TAP_QUEUE_CHUNKS)),
        probe: probe.clone(),
    };
    let chunk = Bytes::from(vec![
        b'x';
        REQUEST_LOG_TAP_CHUNK_BYTES
            * (REQUEST_LOG_TAP_QUEUE_CHUNKS + 1)
    ]);

    tap.observe(&chunk);

    let (_, _, unavailable_reason) = probe.projected_metadata_for_test();
    assert_eq!(unavailable_reason, None);
}

#[test]
fn request_log_response_tap_drops_observation_without_backpressure_when_full() {
    let probe = RouteRequestLogProbe::detached_test_probe();
    let (sender, _receiver) =
        mpsc::channel::<RequestLogObservedChunk>(REQUEST_LOG_TAP_QUEUE_CHUNKS);
    let mut tap = RequestLogResponseTap {
        sender: Some(sender),
        queue_budget: Arc::new(Semaphore::new(REQUEST_LOG_TAP_QUEUE_CHUNKS)),
        probe: probe.clone(),
    };
    let chunk = Bytes::from(vec![b'x'; REQUEST_LOG_TAP_CHUNK_BYTES]);

    for _ in 0..=REQUEST_LOG_TAP_QUEUE_CHUNKS {
        tap.observe(&chunk);
    }
    let (_, _, unavailable_reason) = probe.projected_metadata_for_test();
    assert_eq!(unavailable_reason.as_deref(), Some("observer_queue_full"));
}

#[test]
fn request_log_response_tap_preserves_worker_reason_when_closed() {
    let probe = RouteRequestLogProbe::detached_test_probe();
    probe.mark_usage_unavailable("usage_projection_limit_exceeded");
    let (sender, receiver) = mpsc::channel::<RequestLogObservedChunk>(1);
    drop(receiver);
    let mut tap = RequestLogResponseTap {
        sender: Some(sender),
        queue_budget: Arc::new(Semaphore::new(REQUEST_LOG_TAP_QUEUE_CHUNKS)),
        probe: probe.clone(),
    };

    tap.observe(&Bytes::from_static(b"x"));

    let (_, _, unavailable_reason) = probe.projected_metadata_for_test();
    assert_eq!(
        unavailable_reason.as_deref(),
        Some("usage_projection_limit_exceeded")
    );
}

fn client_tool_search_definition() -> Value {
    json!({
        "type":"tool_search",
        "execution":"client",
        "description":"Search the client tool catalog",
        "parameters":{
            "type":"object",
            "properties":{"goal":{"type":"string"}},
            "required":["goal"],
            "additionalProperties":false
        }
    })
}

pub(super) fn router_config(base_url: String) -> (CodeyConfig, String, String) {
    let mut route = ProviderProfile::new("Relay");
    route.id = "route-a".into();
    route.base_url = base_url;
    route.api_key = "sk-upstream".into();
    route.normalize();
    let provider_id = route.provider_id().to_string();
    let model = "provider-model".to_string();
    let mut config = CodeyConfig {
        active_profile_id: route.id.clone(),
        profiles: vec![route],
        ..CodeyConfig::default()
    }
    .normalize();
    config
        .selected_models_by_provider
        .insert(provider_id.clone(), vec![model.clone()]);
    (config, provider_id, model)
}

#[test]
fn outbound_proxy_matcher_detects_effective_proxy_for_official_route() {
    let proxied = SystemProxyMatcher::builder()
        .https("http://127.0.0.1:7890")
        .build();
    assert!(outbound_proxy_applies_to_url_with_matcher(
        CHATGPT_CODEX_BASE_URL,
        &proxied
    ));

    let bypassed = SystemProxyMatcher::builder()
        .https("http://127.0.0.1:7890")
        .no("chatgpt.com")
        .build();
    assert!(!outbound_proxy_applies_to_url_with_matcher(
        CHATGPT_CODEX_BASE_URL,
        &bypassed
    ));

    let http_only = SystemProxyMatcher::builder()
        .http("http://127.0.0.1:7890")
        .build();
    assert!(!outbound_proxy_applies_to_url_with_matcher(
        CHATGPT_CODEX_BASE_URL,
        &http_only
    ));
}

pub(super) async fn connect_router_websocket(
    endpoint: &RuntimeRouterEndpoint,
) -> WebSocketStream<MaybeTlsStream<TcpStream>> {
    connect_router_websocket_with_headers(endpoint, &[]).await
}

pub(super) async fn connect_router_websocket_with_headers(
    endpoint: &RuntimeRouterEndpoint,
    headers: &[(&str, &str)],
) -> WebSocketStream<MaybeTlsStream<TcpStream>> {
    let url = format!(
        "{}/responses",
        endpoint.base_url.replacen("http://", "ws://", 1)
    );
    let mut request = url.into_client_request().unwrap();
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", endpoint.token)).unwrap(),
    );
    for (name, value) in headers {
        request.headers_mut().insert(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }
    connect_async_with_config(request, None, false)
        .await
        .unwrap()
        .0
}

pub(super) async fn local_websocket_pair() -> (
    WebSocketStream<TcpStream>,
    WebSocketStream<MaybeTlsStream<TcpStream>>,
) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = async {
        let (stream, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(stream).await.unwrap()
    };
    let client = async {
        connect_async_with_config(format!("ws://{address}/responses"), None, false)
            .await
            .unwrap()
            .0
    };
    tokio::join!(server, client)
}

async fn send_router_websocket_request(
    socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
    model: &str,
    input: &str,
) -> Vec<Value> {
    send_router_websocket_request_on_stream(socket, model, input, None).await
}

async fn send_router_websocket_request_on_stream(
    socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
    model: &str,
    input: &str,
    stream_id: Option<&str>,
) -> Vec<Value> {
    let mut request = json!({
        "type":"response.create",
        "model":model,
        "input":input,
    });
    if let Some(stream_id) = stream_id {
        request
            .as_object_mut()
            .unwrap()
            .insert("stream_id".into(), Value::String(stream_id.into()));
    }
    socket
        .send(WebSocketMessage::Text(
            serde_json::to_string(&request).unwrap().into(),
        ))
        .await
        .unwrap();
    let mut events = Vec::new();
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let WebSocketMessage::Text(text) = message else {
            continue;
        };
        let event = serde_json::from_str::<Value>(text.as_str()).unwrap();
        let terminal = responses_event_is_terminal(&event);
        events.push(event);
        if terminal {
            return events;
        }
    }
}

async fn write_test_http_chunk(stream: &mut TcpStream, payload: &str) {
    stream
        .write_all(format!("{:x}\r\n", payload.len()).as_bytes())
        .await
        .unwrap();
    stream.write_all(payload.as_bytes()).await.unwrap();
    stream.write_all(b"\r\n").await.unwrap();
    stream.flush().await.unwrap();
}

#[tokio::test]
async fn local_router_errors_expose_a_safe_request_id_for_correlation() {
    let (mut reader, mut writer) = tokio::io::duplex(4096);
    ROUTER_REQUEST_ID
        .scope("request-123".to_string(), async {
            write_text_error_response(&mut writer, 504, "upstream_timeout", "上游响应超时")
                .await
                .unwrap();
        })
        .await;
    drop(writer);
    let mut response = String::new();
    reader.read_to_string(&mut response).await.unwrap();

    assert!(response.contains("x-codey-request-id: request-123\r\n"));
    assert!(response.contains("请求 ID：request-123"));
}

#[test]
fn runtime_endpoint_validation_allows_http_but_rejects_url_credentials() {
    assert!(responses_endpoint("http://api.example.com/v1").is_ok());
    assert!(responses_endpoint("https://user:pass@api.example.com/v1").is_err());
    assert!(responses_endpoint("http://127.0.0.1:11434/v1").is_ok());
    assert_eq!(
        responses_websocket_endpoint("https://api.example.com/v1").unwrap(),
        "wss://api.example.com/v1/responses"
    );
    assert_eq!(
        responses_websocket_endpoint("http://127.0.0.1:11434/v1").unwrap(),
        "ws://127.0.0.1:11434/v1/responses"
    );
}

#[test]
fn upstream_websocket_handshake_carries_route_auth_and_beta_header() {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer route-key"));
    headers.insert(
        HeaderName::from_static(CHATGPT_ACCOUNT_ID_HEADER),
        HeaderValue::from_static("account-test"),
    );
    headers.insert(
        HeaderName::from_static("openai-beta"),
        HeaderValue::from_static("another_feature=v1"),
    );

    let request = upstream_websocket_request("wss://relay.example/v1/responses", &headers).unwrap();

    assert_eq!(request.uri().path(), "/v1/responses");
    assert_eq!(request.headers()[AUTHORIZATION], "Bearer route-key");
    assert_eq!(request.headers()[CHATGPT_ACCOUNT_ID_HEADER], "account-test");
    let beta = request.headers()["openai-beta"].to_str().unwrap();
    assert!(beta.contains("another_feature=v1"));
    assert!(beta.contains(RESPONSES_WEBSOCKET_BETA));
}

#[tokio::test]
async fn upstream_websocket_connection_disables_nagle() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let _ = socket.next().await;
    });

    let mut socket = connect_upstream_responses_websocket(
        &format!("ws://{address}/v1/responses"),
        &HeaderMap::new(),
        None,
    )
    .await
    .unwrap();
    let MaybeTlsStream::Plain(stream) = socket.get_ref() else {
        panic!("loopback WebSocket must use a plain TCP stream");
    };
    assert!(stream.nodelay().unwrap());

    socket.close(None).await.unwrap();
    server.await.unwrap();
}

#[test]
fn upstream_websocket_sse_wrapper_parser_accepts_complete_and_unterminated_frames() {
    let text = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-sse\"}}\n\n",
        "event: response.output_text.delta\r\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}"
    );

    let events = parse_responses_websocket_sse_events(text).unwrap();

    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["type"], "response.created");
    assert_eq!(events[0]["response"]["id"], "resp-sse");
    assert_eq!(events[1]["type"], "response.output_text.delta");
    assert_eq!(events[1]["delta"], "ok");
}

#[allow(clippy::result_large_err)]
#[tokio::test]
async fn upstream_websocket_normalizes_sse_wrapped_events_to_json_frames() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        assert!(matches!(
            socket.next().await.unwrap().unwrap(),
            WebSocketMessage::Text(_)
        ));
        for event in [
            json!({
                "type":"response.created",
                "response":{
                    "id":"resp-sse-wrapped",
                    "object":"response",
                    "status":"in_progress",
                    "output":[],
                }
            }),
            json!({
                "type":"response.completed",
                "response":{
                    "id":"resp-sse-wrapped",
                    "object":"response",
                    "status":"completed",
                    "output":[],
                }
            }),
        ] {
            let event_type = event["type"].as_str().unwrap();
            let frame = format!(
                "event: {event_type}\ndata: {}\n\n",
                serde_json::to_string(&event).unwrap()
            );
            socket
                .send(WebSocketMessage::Text(frame.into()))
                .await
                .unwrap();
        }
    });

    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].supports_websockets = true;
    let snapshot = RouterSnapshot::from_config(&config);
    let route = Arc::clone(&snapshot.routes[&provider_id]);
    let (downstream_socket, mut downstream_peer) = local_websocket_pair().await;
    let mut downstream = WebSocketResponsesDownstream::new(downstream_socket);
    let mut body = json!({"model":model,"input":"hello"});

    assert_eq!(
        downstream
            .proxy_upstream_websocket(&route, &HeaderMap::new(), &mut body, false, None)
            .await
            .unwrap(),
        UpstreamWebSocketAttempt::Completed
    );

    let mut received = Vec::new();
    for _ in 0..2 {
        let WebSocketMessage::Text(text) = downstream_peer.next().await.unwrap().unwrap() else {
            panic!("expected downstream JSON text frame");
        };
        received.push(serde_json::from_str::<Value>(text.as_str()).unwrap());
    }
    assert_eq!(received[0]["type"], "response.created");
    assert_eq!(received[1]["type"], "response.completed");
    assert!(
        downstream
            .upstream
            .as_ref()
            .unwrap()
            .response_ids
            .contains(&Sha256::digest(b"resp-sse-wrapped").into())
    );
    upstream_task.await.unwrap();
}

#[tokio::test]
async fn websocket_http_fallback_parses_sse_with_a_json_content_type() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let first_sse = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-mislabeled\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[]}}\n\n"
    );
    let final_sse = concat!(
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-mislabeled\",\"object\":\"response\",\"status\":\"completed\",\"output\":[]}}\n\n"
    );
    let (first_event_sent, first_event_observed) = oneshot::channel();
    let (release_upstream, wait_for_release) = oneshot::channel();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).await.unwrap();
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        write_test_http_chunk(&mut stream, first_sse).await;
        first_event_sent.send(()).unwrap();
        wait_for_release.await.unwrap();
        write_test_http_chunk(&mut stream, final_sse).await;
        stream.write_all(b"0\r\n\r\n").await.unwrap();
    });
    let response = reqwest::get(format!("http://{upstream_address}/responses"))
        .await
        .unwrap();
    let (downstream_socket, mut downstream_peer) = local_websocket_pair().await;
    let mut downstream = WebSocketResponsesDownstream::new(downstream_socket);
    let proxy_task = tokio::spawn(async move {
        proxy_native_response_to_websocket(&mut downstream, response, None)
            .await
            .unwrap();
    });

    first_event_observed.await.unwrap();
    let WebSocketMessage::Text(text) =
        tokio::time::timeout(Duration::from_secs(2), downstream_peer.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    else {
        panic!("expected downstream JSON text frame");
    };
    let event = serde_json::from_str::<Value>(text.as_str()).unwrap();
    assert_eq!(event["type"], "response.created");
    assert_eq!(event["response"]["id"], "resp-mislabeled");

    release_upstream.send(()).unwrap();
    let WebSocketMessage::Text(text) = downstream_peer.next().await.unwrap().unwrap() else {
        panic!("expected downstream JSON text frame");
    };
    let event = serde_json::from_str::<Value>(text.as_str()).unwrap();
    assert_eq!(event["type"], "response.completed");
    assert_eq!(event["response"]["id"], "resp-mislabeled");
    proxy_task.await.unwrap();
    upstream_task.await.unwrap();
}

#[test]
fn websocket_error_events_are_terminal_and_connection_limits_force_reconnect() {
    let request_error = json!({
        "type":"error",
        "error":{"code":"invalid_stream_id"},
    });
    assert!(responses_event_is_terminal(&request_error));
    assert!(responses_websocket_connection_is_reusable(&request_error));

    let connection_limit = json!({
        "type":"error",
        "error":{"code":"websocket_connection_limit_reached"},
    });
    assert!(responses_event_is_terminal(&connection_limit));
    assert!(!responses_websocket_connection_is_reusable(
        &connection_limit
    ));
}

#[test]
fn upstream_websocket_liveness_sends_heartbeat_and_expires_missing_pong() {
    let connected_at = Instant::now();
    let mut liveness = UpstreamWebSocketLiveness::new(connected_at);

    assert_eq!(
        liveness.maintenance_deadline(),
        connected_at + UPSTREAM_WEBSOCKET_HEARTBEAT_INTERVAL
    );
    assert_eq!(
        liveness.maintenance_action(
            connected_at + UPSTREAM_WEBSOCKET_HEARTBEAT_INTERVAL - Duration::from_millis(1)
        ),
        UpstreamWebSocketMaintenanceAction::None
    );
    let heartbeat_at = connected_at + UPSTREAM_WEBSOCKET_HEARTBEAT_INTERVAL;
    assert_eq!(
        liveness.maintenance_action(heartbeat_at),
        UpstreamWebSocketMaintenanceAction::SendPing
    );

    liveness.record_heartbeat_sent(heartbeat_at);
    liveness.record_activity(heartbeat_at + Duration::from_secs(1));
    assert_eq!(liveness.heartbeat_sent_at, Some(heartbeat_at));
    assert_eq!(
        liveness.maintenance_deadline(),
        heartbeat_at + UPSTREAM_WEBSOCKET_PONG_TIMEOUT
    );
    assert_eq!(
        liveness.maintenance_action(
            heartbeat_at + UPSTREAM_WEBSOCKET_PONG_TIMEOUT - Duration::from_millis(1)
        ),
        UpstreamWebSocketMaintenanceAction::None
    );
    assert_eq!(
        liveness.maintenance_action(heartbeat_at + UPSTREAM_WEBSOCKET_PONG_TIMEOUT),
        UpstreamWebSocketMaintenanceAction::Drop
    );

    let pong_at = heartbeat_at + Duration::from_secs(1);
    liveness.record_pong(pong_at);
    assert!(liveness.heartbeat_sent_at.is_none());
    assert_eq!(
        liveness.maintenance_deadline(),
        pong_at + UPSTREAM_WEBSOCKET_HEARTBEAT_INTERVAL
    );
    assert_eq!(
        liveness.maintenance_action(connected_at + UPSTREAM_WEBSOCKET_MAX_REUSE_AGE),
        UpstreamWebSocketMaintenanceAction::Drop
    );
}

#[test]
fn upstream_websocket_backoff_is_shared_and_scoped_to_route_and_auth() {
    let now = Instant::now();
    let mut backoffs = UpstreamWebSocketBackoffs::default();
    let key = UpstreamWebSocketBackoffKey::new(
        "route-a",
        "wss://a.example/responses",
        UpstreamWebSocketAuthIdentity::default(),
    );
    let mut retry_at = now;
    for (index, seconds) in [5, 15, 30, 60, 60].into_iter().enumerate() {
        let expected = (index as u32 + 1, Duration::from_secs(seconds));
        assert_eq!(backoffs.record_failure(key.clone(), retry_at), expected);
        let deadline = retry_at + expected.1;
        assert!(backoffs.is_backing_off(&key, retry_at));
        // A burst of failures must neither escalate nor extend this interval.
        for _ in 0..16 {
            assert_eq!(
                backoffs.record_failure(key.clone(), retry_at + Duration::from_secs(1)),
                (expected.0, expected.1 - Duration::from_secs(1))
            );
        }
        assert_eq!(backoffs.entries[&key].until, deadline);
        assert!(!backoffs.is_backing_off(&key, deadline));
        retry_at = deadline;
    }
    assert_eq!(
        backoffs.record_failure(key.clone(), retry_at + Duration::from_secs(61)),
        (1, Duration::from_secs(5))
    );

    let changed_url = UpstreamWebSocketBackoffKey::new(
        "route-a",
        "wss://b.example/responses",
        UpstreamWebSocketAuthIdentity::default(),
    );
    assert!(!backoffs.is_backing_off(&changed_url, now));
    let changed_route = UpstreamWebSocketBackoffKey::new(
        "route-b",
        "wss://a.example/responses",
        UpstreamWebSocketAuthIdentity::default(),
    );
    assert!(!backoffs.is_backing_off(&changed_route, now));
    let changed_auth = UpstreamWebSocketBackoffKey::new(
        "route-a",
        "wss://a.example/responses",
        UpstreamWebSocketAuthIdentity {
            authorization: Some([7; 32]),
            account_id: Some([9; 32]),
        },
    );
    assert!(!backoffs.is_backing_off(&changed_auth, now));

    backoffs.record_success(&key);
    assert!(!backoffs.is_backing_off(&key, now));

    backoffs.record_unsupported(key.clone(), now);
    backoffs.record_failure(key.clone(), now + Duration::from_secs(1));
    assert!(backoffs.entries[&key].unsupported);
    assert_eq!(
        backoffs.entries[&key].until,
        now + UPSTREAM_WEBSOCKET_UNSUPPORTED_TTL
    );
    assert!(backoffs.is_backing_off(
        &key,
        now + UPSTREAM_WEBSOCKET_UNSUPPORTED_TTL - Duration::from_secs(1)
    ));
    assert!(!backoffs.is_backing_off(&key, now + UPSTREAM_WEBSOCKET_UNSUPPORTED_TTL));
}

#[test]
fn only_endpoint_capability_statuses_use_long_websocket_backoff() {
    for status in [
        WebSocketStatusCode::NOT_FOUND,
        WebSocketStatusCode::METHOD_NOT_ALLOWED,
        WebSocketStatusCode::GONE,
        WebSocketStatusCode::NOT_IMPLEMENTED,
    ] {
        let response = tokio_tungstenite::tungstenite::http::Response::builder()
            .status(status)
            .body(None::<Vec<u8>>)
            .unwrap();
        let error =
            anyhow::Error::new(WebSocketError::Http(response)).context("wrapped websocket failure");
        assert!(upstream_websocket_endpoint_is_unsupported(&error));
    }
    for status in [
        WebSocketStatusCode::UNAUTHORIZED,
        WebSocketStatusCode::FORBIDDEN,
        WebSocketStatusCode::TOO_MANY_REQUESTS,
        WebSocketStatusCode::INTERNAL_SERVER_ERROR,
    ] {
        let response = tokio_tungstenite::tungstenite::http::Response::builder()
            .status(status)
            .body(None::<Vec<u8>>)
            .unwrap();
        let error = anyhow::Error::new(WebSocketError::Http(response));
        assert!(!upstream_websocket_endpoint_is_unsupported(&error));
    }
}

#[tokio::test]
async fn router_config_update_invalidates_only_changed_websocket_routes() {
    let (mut config, provider_id, _) = router_config("http://127.0.0.1:9/v1".into());
    config.profiles[0].supports_websockets = true;
    let router = LocalRouter::start(&config).await.unwrap();
    let now = Instant::now();
    let snapshot = RouterSnapshot::from_config(&config);
    let key = UpstreamWebSocketBackoffKey::for_route(
        &snapshot.routes[&provider_id],
        "ws://127.0.0.1:9/v1/responses",
        UpstreamWebSocketAuthIdentity::default(),
    );
    router
        .websocket_backoffs
        .lock()
        .unwrap()
        .record_unsupported(key.clone(), now);
    assert!(router.websocket_backoffs.lock().unwrap().is_backing_off(
        &key,
        now + UPSTREAM_WEBSOCKET_UNSUPPORTED_TTL - Duration::from_secs(1)
    ));

    router.update_config(&config);
    assert!(
        router
            .websocket_backoffs
            .lock()
            .unwrap()
            .is_backing_off(&key, now)
    );
    config.profiles[0].supports_websockets = false;
    router.update_config(&config);

    assert!(
        !router
            .websocket_backoffs
            .lock()
            .unwrap()
            .is_backing_off(&key, now)
    );
    router.stop().await.unwrap();
}

#[tokio::test]
async fn idle_cached_upstream_heartbeat_confirms_socket_before_next_request() {
    let (downstream_socket, mut downstream_peer) = local_websocket_pair().await;
    let (mut upstream_peer, upstream_socket) = local_websocket_pair().await;
    let heartbeat_due_at = Instant::now() - UPSTREAM_WEBSOCKET_HEARTBEAT_INTERVAL;
    let mut downstream = WebSocketResponsesDownstream::new(downstream_socket);
    downstream.upstream = Some(CachedUpstreamWebSocket {
        route_id: "route-a".to_string(),
        url: "ws://upstream.example/responses".to_string(),
        auth_identity: UpstreamWebSocketAuthIdentity::default(),
        response_ids: VecDeque::new(),
        config_identity: [0; 32],
        liveness: UpstreamWebSocketLiveness::new(heartbeat_due_at),
        socket: upstream_socket,
    });

    let next_message = tokio::spawn(async move {
        let message = downstream.next_message().await.unwrap();
        (message, downstream)
    });
    let heartbeat = tokio::time::timeout(Duration::from_secs(1), upstream_peer.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let WebSocketMessage::Ping(payload) = heartbeat else {
        panic!("expected an idle upstream Ping");
    };
    upstream_peer
        .send(WebSocketMessage::Pong(payload))
        .await
        .unwrap();
    downstream_peer
        .send(WebSocketMessage::Text("next request".into()))
        .await
        .unwrap();

    let (message, downstream) = tokio::time::timeout(Duration::from_secs(1), next_message)
        .await
        .unwrap()
        .unwrap();
    let Some(WebSocketMessage::Text(text)) = message else {
        panic!("expected the downstream request after Pong");
    };
    assert_eq!(text, "next request");
    let cached = downstream
        .upstream
        .expect("confirmed socket must stay cached");
    assert!(cached.liveness.heartbeat_sent_at.is_none());
}

#[tokio::test]
async fn idle_cached_upstream_pong_timeout_drops_socket_before_next_request() {
    let (downstream_socket, mut downstream_peer) = local_websocket_pair().await;
    let (_upstream_peer, upstream_socket) = local_websocket_pair().await;
    let now = Instant::now();
    let mut liveness = UpstreamWebSocketLiveness::new(now - Duration::from_secs(20));
    liveness.heartbeat_sent_at = Some(now - UPSTREAM_WEBSOCKET_PONG_TIMEOUT);
    let mut downstream = WebSocketResponsesDownstream::new(downstream_socket);
    downstream.upstream = Some(CachedUpstreamWebSocket {
        route_id: "route-a".to_string(),
        url: "ws://upstream.example/responses".to_string(),
        auth_identity: UpstreamWebSocketAuthIdentity::default(),
        response_ids: VecDeque::new(),
        config_identity: [0; 32],
        liveness,
        socket: upstream_socket,
    });

    downstream_peer
        .send(WebSocketMessage::Text("next request".into()))
        .await
        .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(1), downstream.next_message())
        .await
        .unwrap()
        .unwrap();

    assert!(matches!(message, Some(WebSocketMessage::Text(_))));
    assert!(downstream.upstream.is_none());
}

#[test]
fn router_snapshot_routes_compact_through_each_protocol_endpoint() {
    let (mut config, _, _) = router_config("https://relay.example/v1".into());
    config.profiles[0].supports_websockets = true;
    let responses = RouterSnapshot::from_config(&config);
    assert!(responses.routes["route-a"].supports_websockets);
    assert_eq!(
        responses.routes["route-a"]
            .upstream_compact_url
            .as_ref()
            .unwrap(),
        "https://relay.example/v1/responses/compact"
    );

    config.profiles[0].upstream_protocol =
        crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
    let chat = RouterSnapshot::from_config(&config);
    assert!(!chat.routes["route-a"].supports_websockets);
    assert_eq!(
        chat.routes["route-a"]
            .upstream_compact_url
            .as_ref()
            .unwrap(),
        "https://relay.example/v1/chat/completions"
    );

    config.profiles[0].upstream_protocol =
        crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES.into();
    let anthropic = RouterSnapshot::from_config(&config);
    assert_eq!(
        anthropic.routes["route-a"]
            .upstream_compact_url
            .as_ref()
            .unwrap(),
        "https://relay.example/v1/messages"
    );
}

#[allow(clippy::result_large_err)]
#[tokio::test]
async fn declared_responses_route_reuses_upstream_websocket() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let handshake = Arc::new(Mutex::new(None));
    let captured_handshake = Arc::clone(&handshake);
    let upstream_task = tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let mut socket = accept_hdr_async_with_config(
            stream,
            move |request: &WebSocketRequest, response: WebSocketResponse| {
                *captured_handshake.lock().unwrap() = Some((
                    request.uri().path().to_string(),
                    request
                        .headers()
                        .get(AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string),
                    request
                        .headers()
                        .get("openai-beta")
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string),
                ));
                Ok(response)
            },
            None,
        )
        .await
        .unwrap();
        let mut requests = Vec::new();
        for sequence in 1..=2 {
            let message = socket.next().await.unwrap().unwrap();
            let WebSocketMessage::Text(text) = message else {
                panic!("expected text response.create");
            };
            let request = serde_json::from_str::<Value>(text.as_str()).unwrap();
            requests.push(request.clone());
            let response = json!({
                "id":format!("resp-{sequence}"),
                "object":"response",
                "status":"completed",
                "model":request["model"],
                "output":[],
            });
            socket
                .send(WebSocketMessage::Text(
                    serde_json::to_string(&json!({
                        "type":"response.created",
                        "response":{
                            "id":format!("resp-{sequence}"),
                            "object":"response",
                            "status":"in_progress",
                            "model":request["model"],
                            "output":[],
                        }
                    }))
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            socket
                .send(WebSocketMessage::Text(
                    serde_json::to_string(&json!({
                        "type":"response.completed",
                        "response":response,
                    }))
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
        }
        requests
    });

    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].supports_websockets = true;
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    assert!(endpoint.supports_websockets);
    let alias = model_alias(&provider_id, &model);
    let mut socket = connect_router_websocket(&endpoint).await;

    let first =
        send_router_websocket_request_on_stream(&mut socket, &alias, "first", Some("main")).await;
    let second = send_router_websocket_request(&mut socket, &alias, "second").await;
    assert_eq!(first.last().unwrap()["type"], "response.completed");
    assert_eq!(second.last().unwrap()["type"], "response.completed");
    assert!(first.iter().all(|event| event["stream_id"] == "main"));
    assert!(second.iter().all(|event| event.get("stream_id").is_none()));

    socket.close(None).await.unwrap();
    let requests = upstream_task.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["type"], "response.create");
    assert_eq!(requests[0]["model"], model);
    assert!(requests[0].get("stream").is_none());
    assert!(requests[0].get("background").is_none());
    assert!(requests[0].get("stream_id").is_none());
    assert_eq!(requests[1]["input"], "second");
    assert!(requests[1].get("stream_id").is_none());
    let handshake = handshake.lock().unwrap().clone().unwrap();
    assert_eq!(handshake.0, "/v1/responses");
    assert_eq!(handshake.1.as_deref(), Some("Bearer sk-upstream"));
    assert!(
        handshake
            .2
            .as_deref()
            .unwrap_or_default()
            .contains(RESPONSES_WEBSOCKET_BETA)
    );
    router.stop().await.unwrap();
}

#[tokio::test]
async fn websocket_service_tier_survives_forwarding_and_connection_reuse() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        for tier in ["priority", "default", "priority"] {
            let message = socket.next().await.unwrap().unwrap();
            let WebSocketMessage::Text(text) = message else {
                panic!("expected response.create text message");
            };
            let request: Value = serde_json::from_str(text.as_str()).unwrap();
            assert_eq!(request["type"], "response.create");
            assert_eq!(request["service_tier"], tier);
            socket
                .send(WebSocketMessage::Text(
                    json!({
                        "type":"response.completed",
                        "response":{
                            "id":format!("resp-{tier}"),
                            "status":"completed",
                            "service_tier":"default",
                            "output":[],
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        }
    });
    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].supports_websockets = true;
    let router = LocalRouter::start(&config).await.unwrap();
    let mut socket = connect_router_websocket(&router.endpoint()).await;
    for tier in ["priority", "default", "priority"] {
        socket
            .send(WebSocketMessage::Text(
                json!({
                    "type":"response.create",
                    "model":model_alias(&provider_id, &model),
                    "input":"hello",
                    "service_tier":tier,
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let WebSocketMessage::Text(text) = message else {
            panic!("expected response.completed text message");
        };
        let event: Value = serde_json::from_str(text.as_str()).unwrap();
        assert_eq!(event["type"], "response.completed");
        assert_eq!(event["response"]["service_tier"], "default");
    }
    socket.close(None).await.unwrap();
    upstream_task.await.unwrap();
    router.stop().await.unwrap();
}

#[allow(clippy::result_large_err)]
#[tokio::test]
async fn subagents_use_isolated_upstream_websockets() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for sequence in 1..=2 {
            let (stream, _) = tokio::time::timeout(Duration::from_secs(1), upstream.accept())
                .await
                .expect("each subagent must open its own upstream WebSocket")
                .unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let WebSocketMessage::Text(text) = socket.next().await.unwrap().unwrap() else {
                panic!("expected subagent response.create text message");
            };
            let request = serde_json::from_str::<Value>(text.as_str()).unwrap();
            assert_eq!(request["type"], "response.create");
            assert!(request.get("stream").is_none());
            socket
                .send(WebSocketMessage::Text(
                    serde_json::to_string(&json!({
                        "type":"response.completed",
                        "response":{
                            "id":format!("resp-subagent-{sequence}"),
                            "object":"response",
                            "status":"completed",
                            "model":request["model"],
                            "output":[],
                        }
                    }))
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            requests.push(request);
        }
        requests
    });

    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].supports_websockets = true;
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let alias = model_alias(&provider_id, &model);

    for (header, value, input) in [
        ("x-openai-subagent", "codey_worker", "first child"),
        ("x-codex-parent-thread-id", "parent-thread", "second child"),
    ] {
        let mut socket = connect_router_websocket_with_headers(&endpoint, &[(header, value)]).await;
        let events = send_router_websocket_request(&mut socket, &alias, input).await;
        assert_eq!(events.last().unwrap()["type"], "response.completed");
        socket.close(None).await.unwrap();
    }

    let requests = upstream_task.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["input"], "first child");
    assert_eq!(requests[1]["input"], "second child");
    router.stop().await.unwrap();
}

#[allow(clippy::result_large_err)]
#[tokio::test]
async fn websocket_request_without_model_uses_configured_default() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let WebSocketMessage::Text(text) = socket.next().await.unwrap().unwrap() else {
            panic!("expected response.create text message");
        };
        let request = serde_json::from_str::<Value>(text.as_str()).unwrap();
        socket
            .send(WebSocketMessage::Text(
                serde_json::to_string(&json!({
                    "type":"response.completed",
                    "response":{
                        "id":"resp-default-model",
                        "object":"response",
                        "status":"completed",
                        "model":request["model"],
                        "output":[],
                    }
                }))
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        request
    });

    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].supports_websockets = true;
    config.default_model = model_alias(&provider_id, &model);
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let mut socket = connect_router_websocket(&endpoint).await;
    socket
        .send(WebSocketMessage::Text(
            serde_json::to_string(&json!({
                "type":"response.create",
                "input":"resume without an explicit model",
            }))
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();

    let event = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let WebSocketMessage::Text(text) = event else {
        panic!("expected response.completed text message");
    };
    let event = serde_json::from_str::<Value>(text.as_str()).unwrap();
    assert_eq!(event["type"], "response.completed");

    socket.close(None).await.unwrap();
    let request = upstream_task.await.unwrap();
    assert_eq!(request["model"], model);
    assert_eq!(request["input"], "resume without an explicit model");
    router.stop().await.unwrap();
}

#[allow(clippy::result_large_err)]
#[tokio::test]
async fn upstream_websocket_reconnects_when_account_identity_changes() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let captured_accounts = Arc::new(Mutex::new(Vec::new()));
    let server_accounts = Arc::clone(&captured_accounts);
    let upstream_task = tokio::spawn(async move {
        let mut sockets = Vec::new();
        for sequence in 1..=2 {
            let (stream, _) = tokio::time::timeout(Duration::from_secs(1), upstream.accept())
                .await
                .expect("changed account identity must open a new upstream connection")
                .unwrap();
            let server_accounts = Arc::clone(&server_accounts);
            let mut socket = accept_hdr_async_with_config(
                stream,
                move |request: &WebSocketRequest, response: WebSocketResponse| {
                    server_accounts.lock().unwrap().push(
                        request
                            .headers()
                            .get(CHATGPT_ACCOUNT_ID_HEADER)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_string(),
                    );
                    Ok(response)
                },
                None,
            )
            .await
            .unwrap();
            let message = socket.next().await.unwrap().unwrap();
            assert!(matches!(message, WebSocketMessage::Text(_)));
            socket
                .send(WebSocketMessage::Text(
                    serde_json::to_string(&json!({
                        "type":"response.completed",
                        "response":{
                            "id":format!("resp-{sequence}"),
                            "object":"response",
                            "status":"completed",
                            "output":[],
                        }
                    }))
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            // Keep the first socket alive so the second handshake proves
            // that identity matching, rather than EOF detection, forced
            // the reconnect.
            sockets.push(socket);
        }
        server_accounts.lock().unwrap().clone()
    });

    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].supports_websockets = true;
    let snapshot = RouterSnapshot::from_config(&config);
    let route = Arc::clone(&snapshot.routes[&provider_id]);
    let (downstream_socket, mut downstream_peer) = local_websocket_pair().await;
    let mut downstream = WebSocketResponsesDownstream::new(downstream_socket);

    for (account_id, input) in [("acct-first", "first"), ("acct-second", "second")] {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer same-token"));
        headers.insert(
            HeaderName::from_static(CHATGPT_ACCOUNT_ID_HEADER),
            HeaderValue::from_str(account_id).unwrap(),
        );
        let mut body = json!({"model":model,"input":input});
        assert_eq!(
            downstream
                .proxy_upstream_websocket(&route, &headers, &mut body, false, None)
                .await
                .unwrap(),
            UpstreamWebSocketAttempt::Completed
        );
        let event = downstream_peer.next().await.unwrap().unwrap();
        assert!(matches!(event, WebSocketMessage::Text(_)));
    }

    assert_eq!(
        upstream_task.await.unwrap(),
        vec!["acct-first".to_string(), "acct-second".to_string()]
    );
}

#[allow(clippy::result_large_err)]
#[tokio::test]
async fn upstream_websocket_keeps_continuation_on_original_account_connection() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let captured_account = Arc::new(Mutex::new(String::new()));
        let server_account = Arc::clone(&captured_account);
        let mut socket = accept_hdr_async_with_config(
            stream,
            move |request: &WebSocketRequest, response: WebSocketResponse| {
                *server_account.lock().unwrap() = request
                    .headers()
                    .get(CHATGPT_ACCOUNT_ID_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                Ok(response)
            },
            None,
        )
        .await
        .unwrap();

        let mut requests = Vec::new();
        for sequence in 1..=2 {
            let WebSocketMessage::Text(text) = socket.next().await.unwrap().unwrap() else {
                panic!("expected response.create text message");
            };
            let request = serde_json::from_str::<Value>(text.as_str()).unwrap();
            requests.push(request);
            socket
                .send(WebSocketMessage::Text(
                    serde_json::to_string(&json!({
                        "type":"response.completed",
                        "response":{
                            "id":format!("resp-{sequence}"),
                            "object":"response",
                            "status":"completed",
                            "output":[],
                        }
                    }))
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
        }

        let opened_replacement =
            tokio::time::timeout(Duration::from_millis(200), upstream.accept())
                .await
                .is_ok();
        (
            captured_account.lock().unwrap().clone(),
            requests,
            opened_replacement,
        )
    });

    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].supports_websockets = true;
    let snapshot = RouterSnapshot::from_config(&config);
    let route = Arc::clone(&snapshot.routes[&provider_id]);
    let (downstream_socket, mut downstream_peer) = local_websocket_pair().await;
    let mut downstream = WebSocketResponsesDownstream::new(downstream_socket);

    for (account_id, previous_response_id) in
        [("acct-first", None), ("acct-second", Some("resp-1"))]
    {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer same-token"));
        headers.insert(
            HeaderName::from_static(CHATGPT_ACCOUNT_ID_HEADER),
            HeaderValue::from_str(account_id).unwrap(),
        );
        let mut body = json!({"model":model,"input":account_id});
        if let Some(previous_response_id) = previous_response_id {
            body.as_object_mut().unwrap().insert(
                "previous_response_id".to_string(),
                Value::String(previous_response_id.to_string()),
            );
        }
        assert_eq!(
            downstream
                .proxy_upstream_websocket(&route, &headers, &mut body, false, None)
                .await
                .unwrap(),
            UpstreamWebSocketAttempt::Completed
        );
        let event = downstream_peer.next().await.unwrap().unwrap();
        assert!(matches!(event, WebSocketMessage::Text(_)));
    }

    let (captured_account, requests, opened_replacement) = upstream_task.await.unwrap();
    assert_eq!(captured_account, "acct-first");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1]["previous_response_id"], "resp-1");
    assert!(!opened_replacement);
}

#[allow(clippy::result_large_err)]
#[tokio::test]
async fn unknown_previous_response_id_uses_http_fallback_instead_of_websocket_reuse() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let WebSocketMessage::Text(text) = socket.next().await.unwrap().unwrap() else {
            panic!("expected first response.create text message");
        };
        let first_request = serde_json::from_str::<Value>(text.as_str()).unwrap();
        socket
            .send(WebSocketMessage::Text(
                serde_json::to_string(&json!({
                    "type":"response.completed",
                    "response":{
                        "id":"resp-known-on-websocket",
                        "object":"response",
                        "status":"completed",
                        "output":[],
                    }
                }))
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();

        let reused_existing_socket =
            tokio::time::timeout(Duration::from_millis(200), socket.next())
                .await
                .is_ok();
        let opened_replacement =
            tokio::time::timeout(Duration::from_millis(200), upstream.accept())
                .await
                .is_ok();
        (first_request, reused_existing_socket, opened_replacement)
    });

    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].supports_websockets = true;
    let snapshot = RouterSnapshot::from_config(&config);
    let route = Arc::clone(&snapshot.routes[&provider_id]);
    let (downstream_socket, mut downstream_peer) = local_websocket_pair().await;
    let mut downstream = WebSocketResponsesDownstream::new(downstream_socket);
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer same-token"));
    headers.insert(
        HeaderName::from_static(CHATGPT_ACCOUNT_ID_HEADER),
        HeaderValue::from_static("acct-same"),
    );

    let mut first_body = json!({"model":model,"input":"first"});
    assert_eq!(
        downstream
            .proxy_upstream_websocket(&route, &headers, &mut first_body, false, None)
            .await
            .unwrap(),
        UpstreamWebSocketAttempt::Completed
    );
    let event = downstream_peer.next().await.unwrap().unwrap();
    assert!(matches!(event, WebSocketMessage::Text(_)));

    let mut continuation_from_elsewhere = json!({
        "model":model,
        "input":"second",
        "previous_response_id":"resp-created-over-http"
    });
    assert_eq!(
        downstream
            .proxy_upstream_websocket(
                &route,
                &headers,
                &mut continuation_from_elsewhere,
                false,
                None,
            )
            .await
            .unwrap(),
        UpstreamWebSocketAttempt::UseHttp
    );

    let (first_request, reused_existing_socket, opened_replacement) = upstream_task.await.unwrap();
    assert_eq!(first_request["input"], "first");
    assert!(!reused_existing_socket);
    assert!(!opened_replacement);
}

#[tokio::test]
async fn local_responses_websocket_rejects_missing_router_token() {
    let (config, _, _) = router_config("http://127.0.0.1:9/v1".into());
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let url = format!(
        "{}/responses",
        endpoint.base_url.replacen("http://", "ws://", 1)
    );

    let error = connect_async_with_config(url, None, false)
        .await
        .unwrap_err();
    let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
        panic!("expected HTTP handshake rejection");
    };
    assert_eq!(response.status(), WebSocketStatusCode::UNAUTHORIZED);
    router.stop().await.unwrap();
}

#[tokio::test]
async fn unsupported_websocket_handshake_falls_back_to_http_until_config_changes() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (mut handshake_stream, _) = upstream.accept().await.unwrap();
        let mut handshake_bytes = Vec::new();
        let mut chunk = [0_u8; 2048];
        loop {
            let read = handshake_stream.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            handshake_bytes.extend_from_slice(&chunk[..read]);
            if find_header_end(&handshake_bytes).is_some() {
                break;
            }
        }
        handshake_stream
            .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        drop(handshake_stream);

        let mut requests = Vec::new();
        for sequence in 1..=2 {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let request_body = serde_json::from_slice::<Value>(&request.body).unwrap();
            let response = json!({
                "id":format!("resp-http-{sequence}"),
                "object":"response",
                "status":"completed",
                "model":request_body["model"],
                "output":[],
            })
            .to_string();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            requests.push((request.method, request.path, request_body));
        }
        (String::from_utf8(handshake_bytes).unwrap(), requests)
    });

    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].supports_websockets = true;
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let alias = model_alias(&provider_id, &model);
    let mut socket = connect_router_websocket(&endpoint).await;

    let first =
        send_router_websocket_request_on_stream(&mut socket, &alias, "first", Some("main")).await;
    assert!(
        router
            .websocket_backoffs
            .lock()
            .unwrap()
            .entries
            .values()
            .any(|backoff| backoff.unsupported),
        "404 handshake should suppress WebSocket retries until the capability TTL expires"
    );
    socket.close(None).await.unwrap();
    let mut second_socket = connect_router_websocket(&endpoint).await;
    let second = send_router_websocket_request(&mut second_socket, &alias, "second").await;
    assert_eq!(first.last().unwrap()["type"], "response.completed");
    assert_eq!(second.last().unwrap()["type"], "response.completed");
    assert!(first.iter().all(|event| event["stream_id"] == "main"));
    assert!(second.iter().all(|event| event.get("stream_id").is_none()));

    second_socket.close(None).await.unwrap();
    let (handshake, requests) = upstream_task.await.unwrap();
    assert!(handshake.starts_with("GET /v1/responses HTTP/1.1\r\n"));
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.0 == "POST"));
    assert!(requests.iter().all(|request| request.1 == "/v1/responses"));
    assert_eq!(requests[0].2["model"], model);
    assert_eq!(requests[0].2["stream"], true);
    assert!(requests[0].2.get("stream_id").is_none());
    router.stop().await.unwrap();
}

#[tokio::test]
async fn websocket_entry_reports_http_only_route_failures_as_http() {
    for (status, content_type, body, code, detail) in [
        (
            "200 OK",
            "text/event-stream",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-cut\"}}\n\n",
            "upstream_response_failed",
            "终态事件前断开",
        ),
        (
            "503 Service Unavailable",
            "application/json",
            "{\"error\":{\"message\":\"auth_unavailable: no auth available\"}}",
            "upstream_http_error",
            "auth_unavailable",
        ),
    ] {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = upstream.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            assert_eq!(request.method, "POST");
            assert_eq!(request.path, "/v1/responses");
            assert_eq!(
                serde_json::from_slice::<Value>(&request.body).unwrap()["stream"],
                true
            );
            stream.write_all(format!(
                "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len(),
            ).as_bytes()).await.unwrap();
        });
        let (mut config, provider_id, model) = router_config(format!("http://{address}/v1"));
        config.profiles[0].supports_websockets = false;
        let router = LocalRouter::start(&config).await.unwrap();
        let mut socket = connect_router_websocket(&router.endpoint()).await;
        let events =
            send_router_websocket_request(&mut socket, &model_alias(&provider_id, &model), "hello")
                .await;
        let error = &events.last().unwrap()["response"]["error"];
        assert_eq!(events.last().unwrap()["type"], "response.failed");
        assert_eq!(error["code"], code);
        let message = error["message"].as_str().unwrap();
        assert!(message.contains(detail), "{message}");
        assert!(!message.contains("WebSocket"), "{message}");
        assert_eq!(error["codey"]["routeId"], provider_id);
        upstream_task.await.unwrap();
        socket.close(None).await.unwrap();
        router.stop().await.unwrap();
    }
}

#[tokio::test]
async fn websocket_disconnect_after_response_create_is_not_replayed_over_http() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (stream, _) = upstream.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let message = socket.next().await.unwrap().unwrap();
        let WebSocketMessage::Text(text) = message else {
            panic!("expected response.create text message");
        };
        let request = serde_json::from_str::<Value>(text.as_str()).unwrap();
        socket.close(None).await.unwrap();
        let replayed = tokio::time::timeout(Duration::from_millis(500), upstream.accept())
            .await
            .is_ok();
        (request, replayed)
    });

    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].supports_websockets = true;
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let alias = model_alias(&provider_id, &model);
    let mut socket = connect_router_websocket(&endpoint).await;

    let events = send_router_websocket_request(&mut socket, &alias, "side effect").await;
    assert_eq!(events.last().unwrap()["type"], "response.failed");
    assert_eq!(
        events.last().unwrap()["response"]["error"]["code"],
        "websocket_proxy_failed"
    );

    let (request, replayed) = upstream_task.await.unwrap();
    assert_eq!(request["type"], "response.create");
    assert!(
        !replayed,
        "committed WebSocket request must not be replayed"
    );
    socket.close(None).await.unwrap();
    router.stop().await.unwrap();
}

#[tokio::test]
async fn request_body_budget_rejects_before_allocating_the_declared_body() {
    let (mut reader, mut writer) = tokio::io::duplex(4096);
    writer
        .write_all(
            b"POST /v1/responses HTTP/1.1\r\ncontent-length: 131072\r\nconnection: close\r\n\r\n",
        )
        .await
        .unwrap();
    let budget = Arc::new(Semaphore::new(1));

    let error = read_http_request_with_budget(&mut reader, Some(&budget))
        .await
        .unwrap_err();

    assert!(
        error
            .downcast_ref::<RequestBodyBudgetUnavailable>()
            .is_some()
    );
    assert_eq!(budget.available_permits(), 1);
}

#[tokio::test]
async fn request_budget_accounts_for_json_and_conversion_working_memory() {
    let (mut reader, mut writer) = tokio::io::duplex(4096);
    writer
        .write_all(
            b"POST /v1/responses HTTP/1.1\r\ncontent-length: 65536\r\nconnection: close\r\n\r\n",
        )
        .await
        .unwrap();
    let budget = Arc::new(Semaphore::new(REQUEST_MEMORY_BUDGET_MULTIPLIER - 1));

    let error = read_http_request_with_budget(&mut reader, Some(&budget))
        .await
        .unwrap_err();

    assert!(
        error
            .downcast_ref::<RequestBodyBudgetUnavailable>()
            .is_some()
    );
    assert_eq!(
        budget.available_permits(),
        REQUEST_MEMORY_BUDGET_MULTIPLIER - 1
    );
}

#[tokio::test]
async fn zstd_compressed_responses_request_is_decoded_and_forwarded_as_json() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let content_encoding =
            incoming_header(&request, CONTENT_ENCODING.as_str()).map(str::to_string);
        let content_types = request
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case(CONTENT_TYPE.as_str()))
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>();
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        write_json_response(
            &mut stream,
            200,
            &json!({
                "id":"resp-zstd",
                "object":"response",
                "status":"completed",
                "model":body["model"],
                "output":[],
            }),
        )
        .await
        .unwrap();
        (content_encoding, content_types, body)
    });
    let (config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let request_body = serde_json::to_vec(&json!({
        "model":model_alias(&provider_id, &model),
        "input":[{
            "role":"user",
            "content":[{"type":"input_text","text":"compressed"}],
        }],
        "store":false,
        "stream":false,
    }))
    .unwrap();
    let compressed = zstd::stream::encode_all(Cursor::new(request_body), 3).unwrap();

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .header(CONTENT_TYPE, "application/json")
        .header(CONTENT_ENCODING, "zstd")
        .body(compressed)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.json::<Value>().await.unwrap()["id"], "resp-zstd");
    let (content_encoding, content_types, body) = upstream_task.await.unwrap();
    assert!(content_encoding.is_none());
    assert_eq!(content_types, ["application/json"]);
    assert_eq!(body["model"], model);
    assert_eq!(body["input"][0]["content"][0]["text"], "compressed");
    router.stop().await.unwrap();
}

#[tokio::test]
async fn stopping_router_aborts_connections_that_outlive_the_drain_deadline() {
    let router = LocalRouter::start(&CodeyConfig::default()).await.unwrap();
    let endpoint = router.endpoint();
    let port = endpoint
        .base_url
        .strip_prefix("http://127.0.0.1:")
        .and_then(|value| value.split('/').next())
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let mut connection = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    connection
        .write_all(
            format!(
                "POST /v1/responses HTTP/1.1\r\nauthorization: Bearer {}\r\ncontent-length: 1024\r\n\r\n{{",
                endpoint.token
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    tokio::task::yield_now().await;

    tokio::time::timeout(Duration::from_secs(1), router.stop())
        .await
        .expect("router shutdown should have a bounded drain")
        .unwrap();

    let mut byte = [0_u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(1), connection.read(&mut byte))
        .await
        .expect("managed connection should close with the router");
    assert!(matches!(read, Ok(0) | Err(_)));
}

#[tokio::test]
async fn header_limit_does_not_count_body_buffered_by_the_same_read() {
    let prefix = b"POST /v1/responses HTTP/1.1\r\ncontent-length: 2\r\nx-pad: ";
    let padding = "a".repeat(MAX_HEADER_BYTES - prefix.len());
    let request = format!("{}{}\r\n\r\nok", String::from_utf8_lossy(prefix), padding);
    let mut reader = request.as_bytes();

    let parsed = read_http_request(&mut reader).await.unwrap();

    assert_eq!(parsed.body, b"ok");
}

#[tokio::test]
async fn header_limit_still_rejects_an_oversized_header() {
    let prefix = b"GET /health HTTP/1.1\r\nx-pad: ";
    let padding = "a".repeat(MAX_HEADER_BYTES + 1 - prefix.len());
    let request = format!("{}{}\r\n\r\n", String::from_utf8_lossy(prefix), padding);
    let mut reader = request.as_bytes();

    let error = read_http_request(&mut reader).await.unwrap_err();

    assert!(error.to_string().contains("HTTP 请求头超过"));
}

#[test]
fn route_bindings_refresh_in_amortized_constant_time_and_evict_stale_entries() {
    let mut bindings = RouteBindings::default();
    for index in 0..MAX_ROUTE_BINDINGS {
        bindings.remember(&[format!("thread-id:{index}")], "route-a", false);
    }
    for _ in 0..(MAX_ROUTE_BINDINGS * 5) {
        bindings.remember(&["thread-id:0".to_string()], "route-b", false);
    }

    assert!(bindings.order.len() <= MAX_ROUTE_BINDINGS * 4);
    assert_eq!(
        bindings.route_for_keys(&["thread-id:0".to_string()]),
        Some("route-b".to_string())
    );

    bindings.remember(&["thread-id:new".to_string()], "route-c", false);
    assert_eq!(bindings.routes.len(), MAX_ROUTE_BINDINGS);
    assert!(bindings.routes.contains_key("thread-id:0"));
    assert!(bindings.routes.contains_key("thread-id:new"));
    assert!(!bindings.routes.contains_key("thread-id:1"));
}

#[test]
fn explicit_parent_switch_refreshes_session_fallback_without_child_overwrite() {
    let mut bindings = RouteBindings::default();
    let keys = [
        "thread-id:parent".to_string(),
        "session-id:tree".to_string(),
    ];
    bindings.remember(&keys, "route-a", false);
    bindings.remember(&keys, "route-b", false);

    assert_eq!(
        bindings.route_for_keys(&["session-id:tree".to_string()]),
        Some("route-a".to_string())
    );

    bindings.remember(&keys, "route-b", true);

    assert_eq!(
        bindings.route_for_keys(&["session-id:tree".to_string()]),
        Some("route-b".to_string())
    );
}

#[test]
fn request_log_codex_session_prefers_thread_and_falls_back_to_session() {
    let mut request = HttpRequest {
        method: "POST".into(),
        path: "/v1/responses".into(),
        headers: vec![
            ("session-id".into(), "session-tree".into()),
            ("thread-id".into(), "current-thread".into()),
        ],
        body: b"must-not-be-inspected".to_vec(),
        _body_budget_permit: None,
    };
    assert_eq!(
        request_log_codex_session(&request),
        (Some("current-thread"), false)
    );

    request.headers.retain(|(name, _)| name != "thread-id");
    assert_eq!(
        request_log_codex_session(&request),
        (Some("session-tree"), false)
    );
}

#[test]
fn request_log_parent_session_suppresses_child_identifiers() {
    let mut request = HttpRequest {
        method: "POST".into(),
        path: "/v1/responses".into(),
        headers: vec![
            ("x-codex-parent-thread-id".into(), "parent-thread".into()),
            ("thread-id".into(), "child-thread".into()),
            ("session-id".into(), "child-session".into()),
        ],
        body: b"child prompt must remain unrelated".to_vec(),
        _body_budget_permit: None,
    };
    assert_eq!(
        request_log_codex_session(&request),
        (Some("parent-thread"), true)
    );

    request.headers[0].1.clear();
    assert_eq!(request_log_codex_session(&request), (None, true));

    request.headers = vec![
        ("x-openai-subagent".into(), "codey_worker".into()),
        ("thread-id".into(), "child-thread".into()),
        ("session-id".into(), "child-session".into()),
    ];
    assert_eq!(request_log_codex_session(&request), (None, true));
}

#[test]
fn router_snapshot_maps_route_aliases_to_upstream_models() {
    let (config, provider_id, model) = router_config("https://relay.example/v1".to_string());

    let snapshot = RouterSnapshot::from_config(&config);
    let resolved = snapshot
        .target_for_model(&model_alias(&provider_id, &model))
        .unwrap();

    assert_eq!(
        resolved.route.upstream_url.as_ref().unwrap(),
        "https://relay.example/v1/responses"
    );
    assert_eq!(resolved.upstream_model, model);
    let raw = snapshot.target_for_model("provider-model").unwrap();
    assert_eq!(raw.route.provider_id, provider_id);
}

#[test]
fn historical_aliases_recover_after_route_deletion_disable_and_restart() {
    let (mut config, provider, model) = router_config("https://relay.example/v1".into());
    for legacy in ["codey", "old/relay"] {
        let mut old = config.profiles[0].clone();
        old.id = legacy.into();
        config.profiles.push(old);
        config
            .selected_models_by_provider
            .insert(legacy.into(), vec![model.clone()]);
    }
    config = config.normalize();
    config.profiles.retain(|route| route.id != "old/relay");
    config.selected_models_by_provider.remove("old/relay");
    config.selected_models_by_provider.remove("codey");
    let directory = tempfile::tempdir().unwrap();
    let store = crate::config::ConfigStore::new(directory.path().join("config.json"));
    store.save(&config).unwrap();
    let restored = store.load().unwrap();
    let snapshot = RouterSnapshot::from_config(&restored);
    for requested in [
        model.clone(),
        format!("CODEY/{model}"),
        model_alias("old/relay", &model),
    ] {
        let selected = snapshot
            .target_for_request(&requested, Some("old/relay"), Some("codey"))
            .unwrap();
        assert_eq!(selected.provider_id, provider);
        assert_eq!(selected.upstream_model, model);
        assert_eq!(selected.requested_model, requested);
    }
}

#[test]
fn raw_models_resolve_case_insensitively_and_preserve_slashes() {
    let (mut config, provider, _) = router_config("https://relay.example/v1".into());
    config.selected_models_by_provider.insert(
        provider,
        vec!["Vendor/Model".into(), "codey/vendor/model".into()],
    );
    let snapshot = RouterSnapshot::from_config(&config);
    for (requested, upstream) in [
        ("vendor/model", "Vendor/Model"),
        ("codey/vendor/model", "codey/vendor/model"),
        ("ROUTE-A/VENDOR/MODEL", "Vendor/Model"),
    ] {
        assert_eq!(
            snapshot.target_for_model(requested).unwrap().upstream_model,
            upstream
        );
    }
    assert!(snapshot.target_for_model("unknown/Vendor/Model").is_err());
    // `codey/` is no longer a recognised legacy prefix: without an alias
    // history entry it is just an unknown route prefix.
    assert!(
        snapshot
            .target_for_model("CODEY/codey/vendor/model")
            .is_err()
    );
    assert!(snapshot.target_for_model("codey/missing").is_err());
    assert!(
        snapshot
            .target_for_model("")
            .unwrap_err()
            .to_string()
            .contains("缺少 model")
    );
}

#[test]
fn historical_aliases_require_an_unambiguous_route_and_never_override_active_aliases() {
    let (mut config, provider, model) = router_config("https://relay.example/v1".into());
    let mut second = config.profiles[0].clone();
    second.id = "route-b".into();
    config.profiles.push(second);
    config
        .selected_models_by_provider
        .insert("route-b".into(), vec![model.clone()]);
    let legacy = model_alias("retired-route", &model);
    config
        .model_alias_history
        .insert(legacy.clone(), model.clone());
    let mut config = config.normalize();
    let snapshot = RouterSnapshot::from_config(&config);
    let error = snapshot.target_for_model(&legacy).unwrap_err();
    assert!(format!("{error:#}").contains("缺少明确"));
    for (hint, binding) in [(Some("route-b"), None), (None, Some("route-b"))] {
        assert_eq!(
            snapshot
                .target_for_request(&legacy, hint, binding)
                .unwrap()
                .provider_id,
            "route-b"
        );
    }
    let explicit = model_alias(&provider, &model);
    assert_eq!(
        snapshot
            .target_for_request(&explicit, Some("route-b"), Some("route-b"))
            .unwrap()
            .provider_id,
        provider
    );
    config.profiles[0].name = "Renamed display label".into();
    assert_eq!(
        RouterSnapshot::from_config(&config)
            .target_for_model(&explicit)
            .unwrap()
            .provider_id,
        provider
    );
}

#[test]
fn historical_upstream_ids_are_not_recursively_interpreted_as_active_aliases() {
    let (mut config, provider, _) = router_config("https://relay.example/v1".into());
    config.model_alias_history.insert(
        "old/route-a/provider-model".into(),
        "route-a/provider-model".into(),
    );
    let snapshot = RouterSnapshot::from_config(&config);
    assert!(
        snapshot
            .target_for_model("old/route-a/provider-model")
            .is_err()
    );
    config
        .selected_models_by_provider
        .get_mut(&provider)
        .unwrap()
        .push("route-a/provider-model".into());
    assert_eq!(
        RouterSnapshot::from_config(&config)
            .target_for_model("old/route-a/provider-model")
            .unwrap()
            .upstream_model,
        "route-a/provider-model"
    );
}

#[test]
fn router_snapshot_keeps_all_third_party_routes_active_at_once() {
    let (mut config, provider_a, model) = router_config("https://relay-a.example/v1".to_string());
    let mut route_b = config.profiles[0].clone();
    route_b.id = "route-b".into();
    route_b.name = "Relay B".into();
    route_b.base_url = "https://relay-b.example/v1".into();
    route_b.api_key = "sk-route-b".into();
    route_b.normalize();
    let provider_b = route_b.provider_id().to_string();
    config.profiles.push(route_b);
    config
        .selected_models_by_provider
        .insert(provider_b.clone(), vec![model.clone()]);

    let snapshot = RouterSnapshot::from_config(&config);
    let resolved_a = snapshot
        .target_for_model(&model_alias(&provider_a, &model))
        .unwrap();
    let resolved_b = snapshot
        .target_for_model(&model_alias(&provider_b, &model))
        .unwrap();

    assert_eq!(
        resolved_a.route.upstream_url.as_ref().unwrap(),
        "https://relay-a.example/v1/responses"
    );
    assert_eq!(
        resolved_b.route.upstream_url.as_ref().unwrap(),
        "https://relay-b.example/v1/responses"
    );
    assert_eq!(snapshot.model_aliases().len(), 2);
    assert_eq!(snapshot.model_ids(), vec![model.clone()]);
    assert!(snapshot.target_for_model(&model).is_err());
    let hinted = snapshot
        .target_for_request(&model, Some(&provider_b), None)
        .unwrap();
    assert_eq!(hinted.route.provider_id, provider_b);
    let qualified_alias_with_stale_hint = snapshot
        .target_for_request(&model_alias(&provider_a, &model), Some(&provider_b), None)
        .unwrap();
    assert_eq!(
        qualified_alias_with_stale_hint.route.provider_id,
        provider_a
    );
    assert_eq!(qualified_alias_with_stale_hint.upstream_model, model);
}

#[test]
fn stale_thread_binding_yields_to_only_an_unambiguous_new_model() {
    let (mut config, provider_a, model_a) = router_config("https://relay-a.example/v1".to_string());
    let mut route_b = config.profiles[0].clone();
    route_b.id = "route-b".into();
    route_b.name = "Relay B".into();
    route_b.base_url = "https://relay-b.example/v1".into();
    route_b.api_key = "sk-route-b".into();
    route_b.normalize();
    let provider_b = route_b.provider_id().to_string();
    let model_b = "new-model".to_string();
    config.profiles.push(route_b);
    config
        .selected_models_by_provider
        .insert(provider_b.clone(), vec![model_b.clone()]);

    let snapshot = RouterSnapshot::from_config(&config);
    let switched = snapshot
        .target_for_request(&model_b, None, Some(&provider_a))
        .unwrap();
    assert_eq!(switched.route.provider_id, provider_b);
    assert_eq!(switched.upstream_model, model_b);
    let switched_with_replayed_hint = snapshot
        .target_for_request(&model_b, Some(&provider_a), Some(&provider_a))
        .unwrap();
    assert_eq!(switched_with_replayed_hint.route.provider_id, provider_b);
    assert_eq!(switched_with_replayed_hint.upstream_model, model_b);
    let switched_with_unbound_replayed_hint = snapshot
        .target_for_request("new-model", Some(&provider_a), None)
        .unwrap();
    assert_eq!(
        switched_with_unbound_replayed_hint.route.provider_id,
        provider_b
    );

    let mut route_c = config.profiles[0].clone();
    route_c.id = "route-c".into();
    route_c.name = "Relay C".into();
    route_c.base_url = "https://relay-c.example/v1".into();
    route_c.api_key = "sk-route-c".into();
    route_c.normalize();
    let provider_c = route_c.provider_id().to_string();
    config.profiles.push(route_c);
    config
        .selected_models_by_provider
        .insert(provider_c, vec!["new-model".into()]);
    let ambiguous = RouterSnapshot::from_config(&config)
        .target_for_request("new-model", Some(&provider_a), Some(&provider_a))
        .unwrap_err()
        .to_string();
    assert!(ambiguous.contains("缺少明确"));

    let unchanged = snapshot
        .target_for_request(&model_a, None, Some(&provider_a))
        .unwrap();
    assert_eq!(unchanged.route.provider_id, provider_a);
}

#[test]
fn router_does_not_invent_models_for_an_unconfigured_api_route() {
    let (mut config, provider_id, _) = router_config("https://relay.example/v1".to_string());
    config.selected_models_by_provider.remove(&provider_id);

    let snapshot = RouterSnapshot::from_config(&config);

    assert!(snapshot.model_aliases().is_empty());
    assert!(
        snapshot
            .target_for_model(&model_alias(&provider_id, "gpt-5.6-sol"))
            .is_err()
    );
}

#[test]
fn official_looking_ids_declared_on_an_api_route_remain_route_scoped() {
    let (mut config, provider_id, _) = router_config("https://relay.example/v1".to_string());
    config.selected_models_by_provider.remove(&provider_id);
    config
        .declared_official_models_by_provider
        .insert(provider_id.clone(), vec!["gpt-5.6-sol".into()]);

    let snapshot = RouterSnapshot::from_config(&config);
    let resolved = snapshot
        .target_for_model(&model_alias(&provider_id, "gpt-5.6-sol"))
        .unwrap();

    assert_eq!(resolved.route.provider_id, provider_id);
    assert_eq!(resolved.upstream_model, "gpt-5.6-sol");
}

#[test]
fn official_account_models_enter_the_router_only_when_login_is_available() {
    let mut official = ProviderProfile::new("OpenAI 官方直登");
    official.id = crate::config::DERIVED_OFFICIAL_PROFILE_ID.into();
    official.source_provider_id = Some("openai".into());
    official.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.into();
    official.normalize();
    let mut config = CodeyConfig {
        active_profile_id: official.id.clone(),
        profiles: vec![official],
        official_account_available_this_launch: true,
        ..CodeyConfig::default()
    }
    .normalize();
    config
        .selected_models_by_provider
        .insert("openai".into(), vec!["gpt-5.6-sol".into()]);

    let snapshot = RouterSnapshot::from_config(&config);
    let official_alias = model_alias("openai", "gpt-5.6-sol");
    let resolved = snapshot.target_for_model(&official_alias).unwrap();

    assert_eq!(resolved.route.provider_id, "openai");
    assert!(resolved.route.official_account);
    assert!(resolved.route.supports_websockets);
    assert_eq!(
        resolved.route.upstream_url.as_ref().unwrap(),
        &format!("{CHATGPT_CODEX_BASE_URL}/responses")
    );
    assert_eq!(
        resolved.route.upstream_compact_url.as_ref().unwrap(),
        &format!("{CHATGPT_CODEX_BASE_URL}/responses/compact")
    );
    assert_eq!(
        resolved.route.upstream_websocket_url.as_ref().unwrap(),
        &format!(
            "{}/responses",
            CHATGPT_CODEX_BASE_URL.replacen("https://", "wss://", 1)
        )
    );
    assert_eq!(resolved.upstream_model, "gpt-5.6-sol");
    let raw = snapshot.target_for_model("gpt-5.6-sol").unwrap();
    assert_eq!(raw.route.provider_id, "openai");
    let auto_review = snapshot.target_for_model(CODEX_AUTO_REVIEW_MODEL).unwrap();
    assert_eq!(auto_review.route.provider_id, "openai");
    assert_eq!(auto_review.upstream_model, CODEX_AUTO_REVIEW_MODEL);

    let mut mixed = config.clone();
    let mut relay = ProviderProfile::new("Relay");
    relay.id = "relay".into();
    relay.base_url = "https://relay.example/v1".into();
    relay.api_key = "relay-key".into();
    relay.normalize();
    let relay_id = relay.provider_id().to_string();
    mixed.profiles.push(relay);
    mixed
        .selected_models_by_provider
        .insert(relay_id.clone(), vec!["gpt-5.6-sol".into()]);
    let mixed_snapshot = RouterSnapshot::from_config(&mixed);
    assert_eq!(
        mixed_snapshot
            .target_for_model("gpt-5.6-sol")
            .unwrap()
            .route
            .provider_id,
        "openai"
    );
    assert_eq!(
        mixed_snapshot
            .target_for_request("gpt-5.6-sol", Some(&relay_id), None)
            .unwrap()
            .route
            .provider_id,
        relay_id
    );

    let mut api_key_launch = config;
    api_key_launch.official_account_available_this_launch = false;
    assert!(!api_key_launch.runtime_supports_websockets());
    assert!(
        RouterSnapshot::from_config(&api_key_launch)
            .target_for_model(&official_alias)
            .is_err()
    );
    assert!(
        RouterSnapshot::from_config(&api_key_launch)
            .target_for_model(CODEX_AUTO_REVIEW_MODEL)
            .is_err()
    );
}

#[test]
fn auto_review_uses_a_capable_bound_route_and_otherwise_prefers_official() {
    let (mut config, third_party_provider, _) =
        router_config("https://relay.example/v1".to_string());
    config.profiles[0].supports_auto_review = true;
    let mut official = ProviderProfile::new("OpenAI 官方直登");
    official.id = crate::config::DERIVED_OFFICIAL_PROFILE_ID.into();
    official.source_provider_id = Some("openai".into());
    official.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.into();
    official.normalize();
    config.profiles.push(official);
    config.official_account_available_this_launch = true;
    config
        .selected_models_by_provider
        .insert("openai".into(), vec!["gpt-5.6-sol".into()]);
    let snapshot = RouterSnapshot::from_config(&config);

    let bound = snapshot
        .target_for_request(CODEX_AUTO_REVIEW_MODEL, None, Some(&third_party_provider))
        .unwrap();
    assert_eq!(bound.route.provider_id, third_party_provider);

    let unbound = snapshot
        .target_for_request(CODEX_AUTO_REVIEW_MODEL, None, None)
        .unwrap();
    assert_eq!(unbound.route.provider_id, "openai");
}

#[test]
fn auto_review_is_not_invented_for_an_unsupported_third_party_route() {
    let (mut config, provider_id, _) = router_config("https://relay.example/v1".to_string());
    config
        .upstream_models_by_provider
        .insert(provider_id, vec![CODEX_AUTO_REVIEW_MODEL.into()]);

    assert!(
        RouterSnapshot::from_config(&config)
            .target_for_model(CODEX_AUTO_REVIEW_MODEL)
            .is_err()
    );
}

#[test]
fn third_party_routes_forward_codex_identity_without_chatgpt_account_headers() {
    assert!(should_forward_incoming_header("chatgpt-account-id", true));
    assert!(should_forward_incoming_header("x-openai-originator", true));
    assert!(!should_forward_incoming_header("chatgpt-account-id", false));
    assert!(should_forward_incoming_header("x-openai-originator", false));
    assert!(should_forward_incoming_header("user-agent", false));
    assert!(should_forward_incoming_header(
        "x-codex-installation-id",
        false
    ));
    assert!(should_forward_incoming_header("x-codex-window-id", false));
    assert!(should_forward_incoming_header("originator", false));
    assert!(should_forward_incoming_header("x-stainless-os", false));
    assert!(should_forward_incoming_header("thread-id", false));
    assert!(should_forward_incoming_header("session-id", false));
    assert!(should_forward_incoming_header("prompt-cache-key", false));
    assert!(should_forward_incoming_header("prompt_cache_key", false));
    assert!(should_forward_incoming_header("x-codex-turn-state", false));
    assert!(!should_forward_incoming_header("authorization", true));
    assert!(!should_forward_incoming_header(ROUTER_AUTH_HEADER, true));
    assert!(!should_forward_incoming_header(ROUTE_METADATA_KEY, true));
    assert!(!should_forward_incoming_header(TURN_METADATA_HEADER, false));
    assert!(!should_forward_incoming_header("x-codey-request-id", true));
    assert!(!should_forward_incoming_header("X-Codey-Anything", false));
    assert!(should_forward_incoming_header("accept", false));
}

#[test]
fn routing_hint_model_follows_the_resolved_upstream_model() {
    let mut headers = HeaderMap::new();
    align_routing_hint_model(&mut headers, "provider-model");
    assert!(headers.is_empty());

    headers.insert(
        HeaderName::from_static(ROUTING_HINT_HEADER),
        HeaderValue::from_static("model=route%20a/provider-model;tier=priority"),
    );
    align_routing_hint_model(&mut headers, "provider-model");
    assert_eq!(
        headers[ROUTING_HINT_HEADER],
        HeaderValue::from_static("model=provider-model;tier=priority")
    );

    // 已经一致时不改写；无 model 段时保留原值。
    headers.insert(
        HeaderName::from_static(ROUTING_HINT_HEADER),
        HeaderValue::from_static("tier=priority"),
    );
    align_routing_hint_model(&mut headers, "provider-model");
    assert_eq!(
        headers[ROUTING_HINT_HEADER],
        HeaderValue::from_static("tier=priority")
    );
}

#[test]
fn upstream_response_headers_forward_end_to_end_values_only() {
    assert!(should_forward_upstream_response_header(
        "x-codex-turn-state"
    ));
    assert!(should_forward_upstream_response_header("x-models-etag"));
    assert!(should_forward_upstream_response_header("set-cookie"));
    assert!(!should_forward_upstream_response_header("Content-Type"));
    assert!(!should_forward_upstream_response_header("content-length"));
    assert!(!should_forward_upstream_response_header("Connection"));
    assert!(!should_forward_upstream_response_header(
        "transfer-encoding"
    ));
    assert!(!should_forward_upstream_response_header(
        "X-Codey-Request-Id"
    ));
}

#[test]
fn generated_prompt_cache_key_is_stable_and_scoped() {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer token-a"));
    headers.insert(
        HeaderName::from_static(CHATGPT_ACCOUNT_ID_HEADER),
        HeaderValue::from_static("acct-a"),
    );
    let key = stable_prompt_cache_key(
        "route-a",
        "https://api.example/v1/responses",
        "model-a",
        &headers,
    );
    assert_eq!(
        key,
        stable_prompt_cache_key(
            "route-a",
            "https://api.example/v1/responses",
            "model-a",
            &headers,
        )
    );
    // 生成的缓存键对上游呈现为普通 UUID，不携带 Codey 标识。
    assert!(uuid::Uuid::parse_str(&key).is_ok());
    assert!(!key.to_ascii_lowercase().contains("codey"));

    let mut refreshed_auth = headers.clone();
    refreshed_auth.insert(AUTHORIZATION, HeaderValue::from_static("Bearer token-b"));
    assert_eq!(
        key,
        stable_prompt_cache_key(
            "route-a",
            "https://api.example/v1/responses",
            "model-a",
            &refreshed_auth,
        ),
        "account identity should keep the key stable across token refreshes"
    );
    assert_ne!(
        key,
        stable_prompt_cache_key(
            "route-b",
            "https://api.example/v1/responses",
            "model-a",
            &headers,
        )
    );
    assert_ne!(
        key,
        stable_prompt_cache_key(
            "route-a",
            "https://api.example/v1/responses",
            "model-b",
            &headers,
        )
    );
    let mut changed_account = headers;
    changed_account.insert(
        HeaderName::from_static(CHATGPT_ACCOUNT_ID_HEADER),
        HeaderValue::from_static("acct-b"),
    );
    assert_ne!(
        key,
        stable_prompt_cache_key(
            "route-a",
            "https://api.example/v1/responses",
            "model-a",
            &changed_account,
        )
    );
    let mut api_key_headers = HeaderMap::new();
    api_key_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer token-a"));
    let api_key_identity = stable_prompt_cache_key(
        "route-a",
        "https://api.example/v1/responses",
        "model-a",
        &api_key_headers,
    );
    api_key_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer token-b"));
    assert_ne!(
        api_key_identity,
        stable_prompt_cache_key(
            "route-a",
            "https://api.example/v1/responses",
            "model-a",
            &api_key_headers,
        )
    );
}

#[test]
fn generated_prompt_cache_key_never_overrides_caller_input() {
    let body = json!({"model":"model-a","input":"hello"});
    let mut generated_headers = HeaderMap::new();
    assert!(ensure_native_prompt_cache_key(
        &mut generated_headers,
        &body,
        "route-a",
        "https://api.example/v1/responses",
        "model-a",
    ));
    assert!(generated_headers.contains_key(PROMPT_CACHE_KEY_HEADER));

    let mut caller_headers = HeaderMap::new();
    caller_headers.insert(
        HeaderName::from_static(PROMPT_CACHE_KEY_HEADER),
        HeaderValue::from_static("caller-key"),
    );
    assert!(!ensure_native_prompt_cache_key(
        &mut caller_headers,
        &body,
        "route-a",
        "https://api.example/v1/responses",
        "model-a",
    ));
    assert_eq!(
        caller_headers[PROMPT_CACHE_KEY_HEADER],
        HeaderValue::from_static("caller-key")
    );

    let mut body_key_headers = HeaderMap::new();
    assert!(!ensure_native_prompt_cache_key(
        &mut body_key_headers,
        &json!({"model":"model-a","prompt_cache_key":"body-key"}),
        "route-a",
        "https://api.example/v1/responses",
        "model-a",
    ));
    assert!(!body_key_headers.contains_key(PROMPT_CACHE_KEY_HEADER));
}

#[test]
fn local_router_bearer_token_is_not_reused_as_openai_oauth() {
    let request = HttpRequest {
        method: "POST".to_string(),
        path: "/v1/responses".to_string(),
        headers: vec![
            (
                "authorization".to_string(),
                "Bearer codey-router-token".to_string(),
            ),
            (
                CHATGPT_ACCOUNT_ID_HEADER.to_string(),
                "acct-stale-downstream".to_string(),
            ),
        ],
        body: Vec::new(),
        _body_budget_permit: None,
    };

    assert_eq!(
        incoming_openai_authorization(&request, "Bearer codey-router-token"),
        None
    );
    assert_eq!(
        incoming_openai_authorization(&request, "Bearer another-router-token"),
        Some("Bearer codey-router-token")
    );
}

#[tokio::test]
async fn official_upstream_auth_prefers_incoming_oauth_over_auth_json() {
    let request = HttpRequest {
        method: "POST".to_string(),
        path: "/v1/responses".to_string(),
        headers: vec![
            (
                "authorization".to_string(),
                "Bearer chatgpt-oauth".to_string(),
            ),
            (
                CHATGPT_ACCOUNT_ID_HEADER.to_string(),
                "acct-incoming".to_string(),
            ),
        ],
        body: Vec::new(),
        _body_budget_permit: None,
    };

    let auth_cache = Mutex::new(crate::account_usage::OfficialAuthCache::default());
    let auth = resolve_official_upstream_auth(
        &request,
        "Bearer codey-router-token",
        Path::new("/missing-auth.json"),
        &auth_cache,
    )
    .await
    .unwrap();
    assert_eq!(auth.authorization, "Bearer chatgpt-oauth");
    assert_eq!(auth.account_id.as_deref(), Some("acct-incoming"));
}

#[tokio::test]
async fn official_upstream_auth_loads_codex_auth_json_when_codex_uses_the_router_bearer() {
    let directory = tempfile::tempdir().unwrap();
    let auth_path = directory.path().join("auth.json");
    std::fs::write(
        &auth_path,
        r#"{"auth_mode":"chatgpt","tokens":{"access_token":"chatgpt-access","account_id":"acct-9"}}"#,
    )
    .unwrap();
    let request = HttpRequest {
        method: "POST".to_string(),
        path: "/v1/responses".to_string(),
        headers: vec![(
            "authorization".to_string(),
            "Bearer codey-router-token".to_string(),
        )],
        body: Vec::new(),
        _body_budget_permit: None,
    };

    let auth_cache = Mutex::new(crate::account_usage::OfficialAuthCache::default());
    let auth = resolve_official_upstream_auth(
        &request,
        "Bearer codey-router-token",
        &auth_path,
        &auth_cache,
    )
    .await
    .unwrap();
    assert_eq!(auth.authorization, "Bearer chatgpt-access");
    assert_eq!(auth.account_id.as_deref(), Some("acct-9"));
}

#[tokio::test]
async fn official_upstream_auth_is_missing_without_oauth_or_auth_json() {
    let request = HttpRequest {
        method: "POST".to_string(),
        path: "/v1/responses".to_string(),
        headers: vec![(
            "authorization".to_string(),
            "Bearer codey-router-token".to_string(),
        )],
        body: Vec::new(),
        _body_budget_permit: None,
    };

    let auth_cache = Mutex::new(crate::account_usage::OfficialAuthCache::default());
    assert!(
        resolve_official_upstream_auth(
            &request,
            "Bearer codey-router-token",
            Path::new("/missing-auth.json"),
            &auth_cache,
        )
        .await
        .is_none()
    );
}

#[test]
fn protocol_tokens_are_case_insensitive_without_rewriting_endpoint_paths() {
    assert!(is_hop_by_hop_header("Transfer-Encoding"));
    assert!(!should_forward_incoming_header("ConNection", true));
    assert!(!should_forward_incoming_header("Content-Encoding", true));
    assert!(!should_forward_incoming_header("Content-Type", true));
    assert!(is_sse_content_type("Text/Event-Stream; Charset=UTF-8"));

    let base_url = "https://relay.example/API/V1/Responses?token=private#debug";
    assert_eq!(
        responses_endpoint(base_url).unwrap(),
        "https://relay.example/API/V1/Responses"
    );
    assert_eq!(
        chat_completions_endpoint(base_url).unwrap(),
        "https://relay.example/API/V1/chat/completions"
    );
    assert_eq!(
        anthropic_messages_endpoint(base_url).unwrap(),
        "https://relay.example/API/V1/messages"
    );
}

#[test]
fn router_snapshot_prepares_upstream_url_and_headers() {
    let (mut config, provider_id, model) = router_config("https://relay.example/v1".to_string());
    config.profiles[0].upstream_protocol =
        crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
    config.profiles[0]
        .model_request_headers
        .insert("authorization".into(), "Bearer custom".into());
    config.profiles[0]
        .model_request_headers
        .insert("accept".into(), "application/x-codey-test".into());

    let resolved = RouterSnapshot::from_config(&config)
        .target_for_model(&model_alias(&provider_id, &model))
        .unwrap();
    let headers = resolved.route.upstream_headers.as_ref().unwrap();

    assert_eq!(resolved.protocol, UpstreamProtocol::OpenAiChatCompletions);
    assert_eq!(
        resolved.route.upstream_url.as_ref().unwrap(),
        "https://relay.example/v1/chat/completions"
    );
    assert_eq!(
        headers.get(AUTHORIZATION).unwrap().to_str().unwrap(),
        "Bearer custom"
    );
    assert_eq!(
        headers.get("accept").unwrap().to_str().unwrap(),
        "application/x-codey-test"
    );
}

#[test]
fn route_resolver_keeps_provider_protocol_and_model_selection_explicit() {
    let (mut config, provider_a, model) = router_config("https://responses.example/v1".to_string());
    config.profiles[0].upstream_protocol = crate::config::UPSTREAM_PROTOCOL_OPENAI_RESPONSES.into();

    let mut route_b = config.profiles[0].clone();
    route_b.id = "route-b".into();
    route_b.name = "Chat Relay".into();
    route_b.base_url = "https://chat.example/v1".into();
    route_b.upstream_protocol = crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
    route_b.normalize();
    let provider_b = route_b.provider_id().to_string();

    let mut route_c = config.profiles[0].clone();
    route_c.id = "route-c".into();
    route_c.name = "Anthropic Relay".into();
    route_c.base_url = "https://anthropic.example/v1".into();
    route_c.upstream_protocol = crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES.into();
    route_c.normalize();
    let provider_c = route_c.provider_id().to_string();

    config.profiles.extend([route_b, route_c]);
    config
        .selected_models_by_provider
        .insert(provider_b.clone(), vec![model.clone()]);
    config
        .selected_models_by_provider
        .insert(provider_c.clone(), vec![model.clone()]);

    let snapshot = RouterSnapshot::from_config(&config);
    let selections = [
        (
            provider_a.as_str(),
            UpstreamProtocol::OpenAiResponses,
            "https://responses.example/v1/responses",
        ),
        (
            provider_b.as_str(),
            UpstreamProtocol::OpenAiChatCompletions,
            "https://chat.example/v1/chat/completions",
        ),
        (
            provider_c.as_str(),
            UpstreamProtocol::AnthropicMessages,
            "https://anthropic.example/v1/messages",
        ),
    ];

    for (provider_id, protocol, upstream_url) in selections {
        let requested_model = model_alias(provider_id, &model);
        let resolved = snapshot
            .resolve_request(RouteRequest {
                requested_model: &requested_model,
                route_hint: None,
                bound_route: None,
            })
            .unwrap();

        assert_eq!(resolved.provider_id, provider_id);
        assert_eq!(resolved.protocol, protocol);
        assert_eq!(resolved.requested_model, requested_model);
        assert_eq!(resolved.upstream_model, model);
        assert_eq!(resolved.route.upstream_url.as_ref().unwrap(), upstream_url);
    }

    assert!(snapshot.target_for_model(&model).is_err());
}

#[test]
fn protocol_bridge_converts_only_when_the_upstream_protocol_differs() {
    assert_eq!(
        ProtocolBridge::from_upstream_protocol(UpstreamProtocol::OpenAiResponses),
        ProtocolBridge::NativeResponses
    );
    assert_eq!(
        ProtocolBridge::from_upstream_protocol(UpstreamProtocol::OpenAiChatCompletions),
        ProtocolBridge::ResponsesToChatCompletions
    );
    assert_eq!(
        ProtocolBridge::from_upstream_protocol(UpstreamProtocol::AnthropicMessages),
        ProtocolBridge::ResponsesToAnthropicMessages
    );

    let native = ProtocolBridge::NativeResponses
        .convert_responses_body(&json!({
            "model":"provider-model",
            "input":"search",
            "tools":[{"type":"web_search"}]
        }))
        .unwrap();

    assert!(native.is_none());
}

#[test]
fn router_snapshot_rejects_unsafe_saved_headers_without_per_request_parsing() {
    let (mut config, provider_id, model) = router_config("https://relay.example/v1".to_string());
    config.profiles[0]
        .model_request_headers
        .insert("connection".into(), "keep-alive".into());

    let resolved = RouterSnapshot::from_config(&config)
        .target_for_model(&model_alias(&provider_id, &model))
        .unwrap();

    assert!(
        resolved
            .route
            .upstream_headers
            .as_ref()
            .unwrap_err()
            .contains("不允许覆盖")
    );
}

#[test]
fn codey_route_metadata_is_removed_without_dropping_other_turn_metadata() {
    let mut request = HttpRequest {
        method: "POST".into(),
        path: "/v1/responses".into(),
        headers: vec![(
            TURN_METADATA_HEADER.into(),
            json!({ROUTE_METADATA_KEY:"route-a","keep":"header"}).to_string(),
        )],
        body: Vec::new(),
        _body_budget_permit: None,
    };
    let mut body = json!({
        "client_metadata": {
            "x-codex-turn-metadata": json!({
                ROUTE_METADATA_KEY: "route-a",
                "keep": "body"
            }).to_string()
        }
    });

    let (route, body_mutated) = take_codey_route_metadata(&mut request, &mut body).unwrap();

    assert_eq!(route.as_deref(), Some("route-a"));
    assert!(body_mutated);
    let header = serde_json::from_str::<Value>(&request.headers[0].1).unwrap();
    assert!(header.get(ROUTE_METADATA_KEY).is_none());
    assert_eq!(header["keep"], "header");
    let nested = serde_json::from_str::<Value>(
        body["client_metadata"][TURN_METADATA_HEADER]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(nested.get(ROUTE_METADATA_KEY).is_none());
    assert_eq!(nested["keep"], "body");
}

#[test]
fn codey_route_metadata_header_only_does_not_mark_the_body_mutated() {
    let mut request = HttpRequest {
        method: "POST".into(),
        path: "/v1/responses".into(),
        headers: vec![(
            TURN_METADATA_HEADER.into(),
            json!({ROUTE_METADATA_KEY:"route-a","keep":"header"}).to_string(),
        )],
        body: Vec::new(),
        _body_budget_permit: None,
    };
    let mut body = json!({
        "model": "gpt-5.4",
        "client_metadata": {
            "keep": "body"
        }
    });

    let (route, body_mutated) = take_codey_route_metadata(&mut request, &mut body).unwrap();

    assert_eq!(route.as_deref(), Some("route-a"));
    assert!(!body_mutated);
    assert_eq!(body["client_metadata"]["keep"], "body");
}

#[test]
fn native_responses_passthrough_skips_reserialize_when_unmodified() {
    assert!(should_passthrough_native_responses(
        ProtocolBridge::NativeResponses,
        "gpt-5.4",
        "gpt-5.4",
        false,
    ));
    assert!(!should_passthrough_native_responses(
        ProtocolBridge::NativeResponses,
        "alias/gpt-5.4",
        "gpt-5.4",
        false,
    ));
    assert!(!should_passthrough_native_responses(
        ProtocolBridge::NativeResponses,
        "gpt-5.4",
        "gpt-5.4",
        true,
    ));
    assert!(!should_passthrough_native_responses(
        ProtocolBridge::ResponsesToChatCompletions,
        "gpt-5.4",
        "gpt-5.4",
        false,
    ));
}

#[test]
fn codey_synthetic_previous_response_ids_are_not_forwardable() {
    assert!(is_codey_synthetic_response_id("resp_codey_local"));
    assert!(is_codey_synthetic_response_id(" resp_codey_local "));
    assert!(!is_codey_synthetic_response_id("resp_real_upstream"));
    assert!(!is_codey_synthetic_response_id("resp-1"));

    let synthetic = json!({
        "model": "gpt-5.4",
        "input": "continue",
        "previous_response_id": "resp_codey_wrapped",
    });
    assert!(has_codey_synthetic_previous_response_id(&synthetic));
    assert_eq!(synthetic["previous_response_id"], "resp_codey_wrapped");

    let upstream = json!({
        "model": "gpt-5.4",
        "input": "continue",
        "previous_response_id": "resp_upstream",
    });
    assert!(!has_codey_synthetic_previous_response_id(&upstream));
    assert_eq!(upstream["previous_response_id"], "resp_upstream");
}

#[test]
fn adapted_websocket_continuation_reuses_the_previous_response_context() {
    let mut history = AdaptedResponsesHistory::default();
    let mut first = json!({"input":"hello"});
    assert!(!history.prepare(&mut first).unwrap());
    let first_output = vec![json!({
        "type":"function_call",
        "call_id":"call-1",
        "name":"lookup",
        "arguments":"{}"
    })];
    history.remember("resp_codey_first", &first_output).unwrap();

    let tool_output = json!({
        "type":"function_call_output",
        "call_id":"call-1",
        "output":"done"
    });
    let mut continuation = json!({
        "model":"provider-model",
        "input":[tool_output],
        "previous_response_id":"resp_codey_first"
    });
    assert!(history.prepare(&mut continuation).unwrap());
    assert!(continuation.get("previous_response_id").is_none());
    assert_eq!(
        continuation["input"],
        json!(["hello", first_output[0].clone(), tool_output])
    );
    let converted = responses_to_chat_completions_body(&continuation).unwrap();
    assert_eq!(converted["messages"][0]["content"], "hello");
    assert_eq!(converted["messages"][1]["tool_calls"][0]["id"], "call-1");
    assert_eq!(converted["messages"][2]["tool_call_id"], "call-1");
}

#[test]
fn chat_reasoning_content_round_trips_through_tool_history() {
    for reasoning in ["", "先读取文件。\nThen check 🙂"] {
        for text in [Value::Null, json!("checking")] {
            for tools in [false, true] {
                let mut message = json!({
                    "role":"assistant", "content":text, "reasoning_content":reasoning,
                });
                if tools {
                    message["tool_calls"] = json!([
                        {"id":"call-1","type":"function","function":{"name":"lookup","arguments":"{}"}},
                        {"id":"call-2","type":"function","function":{"name":"lookup","arguments":"{}"}}
                    ]);
                }
                let direct = responses_to_chat_completions_body(&json!({
                    "model":"provider-model", "input":[message.clone()],
                }))
                .unwrap();
                assert_eq!(direct["messages"][0], message);
                let response = chat_completion_to_responses_body(
                    json!({
                        "choices":[{"message":message,"finish_reason":"stop"}]
                    }),
                    "provider-model",
                )
                .unwrap();
                assert_eq!(response["output"][0]["type"], "reasoning");
                assert_eq!(response["output"][0]["content"][0]["text"], reasoning);
                assert_eq!(response["output_text"], text.as_str().unwrap_or_default());

                let mut history = AdaptedResponsesHistory::default();
                history.prepare(&mut json!({"input":"inspect"})).unwrap();
                history
                    .remember(
                        "resp_codey_reasoning",
                        response["output"].as_array().unwrap(),
                    )
                    .unwrap();
                let followup = if tools {
                    json!([
                        {"type":"function_call_output","call_id":"call-1","output":"one"},
                        {"type":"function_call_output","call_id":"call-2","output":"two"}
                    ])
                } else {
                    json!([{"role":"user","content":"continue"}])
                };
                let mut next = json!({
                    "model":"provider-model", "previous_response_id":"resp_codey_reasoning",
                    "input":followup,
                });
                assert!(history.prepare(&mut next).unwrap());
                // The expanded input is also the full-history HTTP/reconnect representation.
                let converted = responses_to_chat_completions_body(&next).unwrap();
                let assistant = &converted["messages"][1];
                assert_eq!(assistant["reasoning_content"], reasoning);
                assert_eq!(assistant["content"], text);
                if tools {
                    assert_eq!(assistant["tool_calls"].as_array().unwrap().len(), 2);
                    assert_eq!(converted["messages"][2]["tool_call_id"], "call-1");
                    assert_eq!(converted["messages"][3]["tool_call_id"], "call-2");
                }
                assert!(converted["messages"][0].get("reasoning_content").is_none());
                assert!(converted["messages"][2].get("reasoning_content").is_none());
            }
        }
    }
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"先读取\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"文件。\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let chat = parse_chat_completion_sse_bytes(sse.as_bytes(), "provider-model").unwrap();
    assert_eq!(
        chat["choices"][0]["message"]["reasoning_content"],
        "先读取文件。"
    );
    let response = chat_completion_to_responses_body(chat, "provider-model").unwrap();
    let converted = responses_to_chat_completions_body(&json!({
        "model":"provider-model", "input":response["output"],
    }))
    .unwrap();
    assert_eq!(
        converted["messages"][0]["reasoning_content"],
        "先读取文件。"
    );
    assert_eq!(converted["messages"][0]["content"], "done");
}

#[test]
fn native_responses_rewrite_preserves_large_raw_fields() {
    let original = br#"{
        "model" : "route-a/gpt-5.4",
        "input" : [ { "role" : "user", "content" : "keep raw spacing" } ],
        "client_metadata" : {"codey_route":"route-a","keep":"yes"},
        "tools" : [ { "type" : "function", "name" : "lookup" } ]
    }"#;
    let updated = json!({
        "model":"gpt-5.4",
        "input":[{"role":"user","content":"keep raw spacing"}],
        "client_metadata":{"keep":"yes"},
        "tools":[{"type":"function","name":"lookup"}],
    });

    let rewritten = rewrite_native_responses_encoded_body(original, &updated).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&rewritten).unwrap(),
        updated
    );
    let rewritten = String::from_utf8(rewritten).unwrap();
    assert!(rewritten.contains(r#"[ { "role" : "user", "content" : "keep raw spacing" } ]"#));
    assert!(rewritten.contains(r#"[ { "type" : "function", "name" : "lookup" } ]"#));
    assert!(!rewritten.contains(ROUTE_METADATA_KEY));
}

#[test]
fn native_responses_rewrite_adds_a_defaulted_model_and_can_remove_metadata() {
    let original =
        br#"{"input":"hello","previous_response_id":"resp_codey_old","client_metadata":{"codey_route":"route-a"}}"#;
    let updated = json!({"model":"gpt-5.6-sol","input":"hello"});

    let rewritten = rewrite_native_responses_encoded_body(original, &updated).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&rewritten).unwrap(),
        updated
    );
    assert!(
        !String::from_utf8(rewritten)
            .unwrap()
            .contains(ROUTE_METADATA_KEY)
    );
}

#[test]
fn native_responses_rewrite_can_update_previous_response_id() {
    let original = br#"{"model":"gpt-5.4","input":"hello"}"#;
    let updated = json!({
        "model":"gpt-5.4",
        "input":"hello",
        "previous_response_id":"resp_upstream",
    });

    let rewritten = rewrite_native_responses_encoded_body(original, &updated).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&rewritten).unwrap(),
        updated
    );
}

#[test]
fn responses_endpoint_reuses_explicit_responses_url() {
    assert_eq!(
        responses_endpoint("https://relay.example/v1/responses").unwrap(),
        "https://relay.example/v1/responses"
    );
    assert_eq!(
        responses_endpoint("https://relay.example/v1").unwrap(),
        "https://relay.example/v1/responses"
    );
    assert_eq!(
        responses_compact_endpoint("https://relay.example/v1/responses").unwrap(),
        "https://relay.example/v1/responses/compact"
    );
    assert_eq!(
        responses_compact_endpoint("https://relay.example/v1").unwrap(),
        "https://relay.example/v1/responses/compact"
    );
    assert_eq!(
        image_generation_endpoint("https://relay.example/v1/responses").unwrap(),
        "https://relay.example/v1/images/generations"
    );
    assert_eq!(
        image_generation_endpoint("https://relay.example/v1/chat/completions").unwrap(),
        "https://relay.example/v1/images/generations"
    );
}

#[test]
fn chat_completions_endpoint_accepts_root_version_and_explicit_paths() {
    assert_eq!(
        chat_completions_endpoint("https://relay.example").unwrap(),
        "https://relay.example/v1/chat/completions"
    );
    assert_eq!(
        chat_completions_endpoint("https://relay.example/v1").unwrap(),
        "https://relay.example/v1/chat/completions"
    );
    assert_eq!(
        chat_completions_endpoint("https://relay.example/v1/responses").unwrap(),
        "https://relay.example/v1/chat/completions"
    );
    assert_eq!(
        chat_completions_endpoint("https://relay.example/v1/chat/completions").unwrap(),
        "https://relay.example/v1/chat/completions"
    );
}

#[test]
fn anthropic_messages_endpoint_accepts_root_version_and_explicit_paths() {
    assert_eq!(
        anthropic_messages_endpoint("https://api.anthropic.com").unwrap(),
        "https://api.anthropic.com/v1/messages"
    );
    assert_eq!(
        anthropic_messages_endpoint("https://relay.example/v1").unwrap(),
        "https://relay.example/v1/messages"
    );
    assert_eq!(
        anthropic_messages_endpoint("https://relay.example/v1/responses").unwrap(),
        "https://relay.example/v1/messages"
    );
    assert_eq!(
        anthropic_messages_endpoint("https://relay.example/v1/messages").unwrap(),
        "https://relay.example/v1/messages"
    );
}

#[test]
fn responses_request_converts_messages_tools_images_and_structured_output_to_chat() {
    let chat = responses_to_chat_completions_body(&json!({
        "model": "provider-model",
        "instructions": "Be concise",
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [
                    {"type":"input_text","text":"inspect"},
                    {"type":"input_image","image_url":"https://example.invalid/a.png","detail":"low"}
                ]
            },
            {"type":"function_call","call_id":"call-1","name":"lookup","arguments":"{\"q\":1}"},
            {"type":"function_call_output","call_id":"call-1","output":"done"}
        ],
        "tools": [{
            "type": "function",
            "name": "lookup",
            "description": "lookup data",
            "parameters": {"type":"object"},
            "strict": true
        }],
        "tool_choice": {"type":"function","name":"lookup"},
        "parallel_tool_calls": true,
        "reasoning": {"effort":"high"},
        "text": {"format": {
            "type": "json_schema",
            "name": "answer",
            "schema": {"type":"object"},
            "strict": true
        }},
        "stream": true
    }))
    .unwrap();

    assert_eq!(chat["model"], "provider-model");
    assert_eq!(chat["messages"][0]["role"], "system");
    assert_eq!(chat["messages"][1]["content"][1]["type"], "image_url");
    assert_eq!(chat["messages"][2]["tool_calls"][0]["id"], "call-1");
    assert_eq!(chat["messages"][3]["tool_call_id"], "call-1");
    assert_eq!(chat["tools"][0]["function"]["name"], "lookup");
    assert_eq!(chat["tool_choice"]["function"]["name"], "lookup");
    assert_eq!(chat["reasoning_effort"], "high");
    assert_eq!(chat["response_format"]["type"], "json_schema");
    assert_eq!(chat["response_format"]["json_schema"]["name"], "answer");
    assert_eq!(chat["stream_options"]["include_usage"], true);
}

#[test]
fn responses_tool_output_images_remain_visible_in_fallback_protocols() {
    let body = json!({
        "model":"provider-model",
        "input":[
            {"type":"function_call","call_id":"call-1","name":"inspect","arguments":"{}"},
            {
                "type":"function_call_output",
                "call_id":"call-1",
                "output":[
                    {"type":"input_text","text":"rendered image:"},
                    {"type":"input_image","image_url":"data:image/png;base64,aGVsbG8="}
                ]
            }
        ],
        "tools":[{
            "type":"function",
            "name":"inspect",
            "parameters":{"type":"object"}
        }]
    });

    let chat = responses_to_chat_completions_body(&body).unwrap();
    assert_eq!(chat["messages"][1]["role"], "tool");
    assert_eq!(chat["messages"][1]["content"], "rendered image:");
    assert_eq!(chat["messages"][2]["role"], "user");
    assert_eq!(chat["messages"][2]["content"][0]["type"], "image_url");
    assert_eq!(
        chat["messages"][2]["content"][0]["image_url"]["url"],
        "data:image/png;base64,aGVsbG8="
    );

    let anthropic = responses_to_anthropic_messages_body(&body).unwrap();
    assert_eq!(anthropic["messages"][1]["role"], "user");
    assert_eq!(
        anthropic["messages"][1]["content"][0]["type"],
        "tool_result"
    );
    assert_eq!(
        anthropic["messages"][1]["content"][0]["content"],
        "rendered image:"
    );
    assert_eq!(anthropic["messages"][1]["content"][1]["type"], "image");
    assert_eq!(
        anthropic["messages"][1]["content"][1]["source"]["data"],
        "aGVsbG8="
    );
}

#[test]
fn configured_duplicate_function_tools_are_deduplicated_without_merging_conflicts() {
    let string_lookup = json!({
        "type":"function",
        "name":"lookup",
        "description":"lookup data",
        "parameters":{"type":"object","properties":{"id":{"type":"string"}}},
        "strict":true
    });
    let number_lookup = json!({
        "type":"function",
        "name":"lookup",
        "description":"lookup data",
        "parameters":{"type":"object","properties":{"id":{"type":"number"}}},
        "strict":true
    });
    let body = json!({
        "model":"provider-model",
        "input":"hello",
        "tools":[string_lookup.clone(), string_lookup, number_lookup]
    });

    let chat = responses_to_chat_completions_body(&body).unwrap();
    let anthropic = responses_to_anthropic_messages_body(&body).unwrap();

    assert_eq!(chat["tools"].as_array().unwrap().len(), 2);
    assert_eq!(anthropic["tools"].as_array().unwrap().len(), 2);
    assert_eq!(
        chat["tools"][0]["function"]["parameters"]["properties"]["id"]["type"],
        "string"
    );
    assert_eq!(
        chat["tools"][1]["function"]["parameters"]["properties"]["id"]["type"],
        "number"
    );
}

#[test]
fn additional_tools_are_merged_without_becoming_chat_or_anthropic_messages() {
    let body = json!({
        "model":"provider-model",
        "tools":[{
            "type":"function",
            "name":"always_available",
            "parameters":{"type":"object","properties":{}}
        }],
        "input":[
            {"type":"message","role":"user","content":"hello"},
            {
                "type":"additional_tools",
                "role":"developer",
                "tools":[{
                    "type":"function",
                    "name":"loaded_later",
                    "description":"Loaded at this point in the Responses input",
                    "parameters":{"type":"object","properties":{"q":{"type":"string"}}}
                }]
            }
        ]
    });

    let chat = responses_to_chat_completions_body(&body).unwrap();
    let anthropic = responses_to_anthropic_messages_body(&body).unwrap();

    assert_eq!(chat["messages"].as_array().unwrap().len(), 1);
    assert_eq!(chat["messages"][0]["content"], "hello");
    assert_eq!(chat["tools"].as_array().unwrap().len(), 2);
    assert_eq!(chat["tools"][1]["function"]["name"], "loaded_later");
    assert_eq!(anthropic["messages"].as_array().unwrap().len(), 1);
    assert_eq!(anthropic["messages"][0]["content"][0]["text"], "hello");
    assert_eq!(anthropic["tools"].as_array().unwrap().len(), 2);
    assert_eq!(anthropic["tools"][1]["name"], "loaded_later");
}

#[test]
fn additional_tools_reject_conflicting_function_definitions() {
    let error = responses_to_anthropic_messages_body(&json!({
        "model":"provider-model",
        "tools":[{
            "type":"function",
            "name":"lookup",
            "parameters":{"type":"object","properties":{"id":{"type":"string"}}}
        }],
        "input":[
            {"role":"user","content":"hello"},
            {
                "type":"additional_tools",
                "role":"developer",
                "tools":[{
                    "type":"function",
                    "name":"lookup",
                    "parameters":{"type":"object","properties":{"id":{"type":"number"}}}
                }]
            }
        ]
    }))
    .unwrap_err();

    assert!(error.to_string().contains("定义冲突的工具 function/lookup"));
}

#[test]
fn additional_tools_require_the_developer_role() {
    let error = responses_to_chat_completions_body(&json!({
        "model":"provider-model",
        "input":[
            {"role":"user","content":"hello"},
            {"type":"additional_tools","role":"user","tools":[]}
        ]
    }))
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("additional_tools.role 必须是 developer")
    );
}

#[test]
fn agent_message_items_convert_to_assistant_history() {
    let body = json!({
        "model":"provider-model",
        "input":[
            {"role":"user","content":"inspect the screenshot"},
            {"type":"agent_message","message":"The visual worker saw an error banner."},
            {"type":"agent_message","content":[{"type":"output_text","text":"The selected route is Chat Completions."}]}
        ]
    });

    let chat = responses_to_chat_completions_body(&body).unwrap();
    assert_eq!(
        chat["messages"],
        json!([
            {"role":"user","content":"inspect the screenshot"},
            {"role":"assistant","content":"The visual worker saw an error banner."},
            {"role":"assistant","content":"The selected route is Chat Completions."}
        ])
    );

    let anthropic = responses_to_anthropic_messages_body(&body).unwrap();
    assert_eq!(anthropic["messages"][1]["role"], "assistant");
    assert_eq!(
        anthropic["messages"][1]["content"],
        json!([
            {"type":"text","text":"The visual worker saw an error banner."},
            {"type":"text","text":"The selected route is Chat Completions."}
        ])
    );
}

#[test]
fn opaque_responses_content_parts_are_ignored_during_chat_fallback_conversion() {
    let body = json!({
        "model":"provider-model",
        "input":[
            {"role":"user","content":"inspect the screenshot"},
            {
                "type":"agent_message",
                "message":"The agent kept visible fallback text.",
                "content":[
                    {"type":"encrypted_content","encrypted_content":"opaque-only-agent-state"}
                ]
            },
            {
                "type":"agent_message",
                "message":"Single object fallback text.",
                "content":{"type":"encrypted_content","encrypted_content":"opaque-single-agent-state"}
            },
            {
                "type":"agent_message",
                "content":[
                    {"type":"encrypted_content","encrypted_content":"opaque-agent-state"},
                    {"type":"output_text","text":"The visual worker saw an error banner."}
                ]
            },
            {
                "type":"message",
                "role":"assistant",
                "content":[
                    {"type":"encrypted_content","encrypted_content":"opaque-assistant-state"},
                    {"type":"refusal","refusal":"I cannot inspect that file."}
                ]
            },
            {
                "role":"user",
                "content":[
                    {"encrypted_content":"opaque-user-state"},
                    {"type":"reasoning","encrypted_content":"opaque-reasoning-part"},
                    {"type":"input_text","text":"try again"}
                ]
            },
            {
                "role":"user",
                "content":{"type":"encrypted_content","encrypted_content":"opaque-single-user-state"}
            }
        ]
    });

    let chat = responses_to_chat_completions_body(&body).unwrap();
    assert_eq!(
        chat["messages"],
        json!([
            {"role":"user","content":"inspect the screenshot"},
            {"role":"assistant","content":"The agent kept visible fallback text."},
            {"role":"assistant","content":"Single object fallback text."},
            {"role":"assistant","content":"The visual worker saw an error banner."},
            {"role":"assistant","content":"I cannot inspect that file."},
            {"role":"user","content":[{"type":"text","text":"try again"}]}
        ])
    );
    assert!(!chat.to_string().contains("opaque"));

    let anthropic = responses_to_anthropic_messages_body(&body).unwrap();
    assert!(!anthropic.to_string().contains("opaque"));
    assert_eq!(anthropic["messages"][1]["content"][0]["type"], "text");
    assert_eq!(
        anthropic["messages"][1]["content"][0]["text"],
        "The agent kept visible fallback text."
    );
    assert_eq!(
        anthropic["messages"][1]["content"][1]["text"],
        "Single object fallback text."
    );
    assert_eq!(
        anthropic["messages"][1]["content"][2]["text"],
        "The visual worker saw an error banner."
    );
    assert_eq!(
        anthropic["messages"][1]["content"][3]["text"],
        "I cannot inspect that file."
    );
}

#[test]
fn nonportable_compaction_history_is_rejected_without_losing_visible_context() {
    let body = json!({
        "model":"provider-model",
        "input":[
            {"role":"user","content":"continue"},
            {"type":"reasoning","id":"rs_1","encrypted_content":"opaque-reasoning"},
            {"type":"compaction","id":"cmp_1","encrypted_content":"opaque-window"},
            {
                "type":"web_search_call",
                "id":"ws_1",
                "status":"completed",
                "action":{"type":"search","query":"provider-side query"}
            }
        ]
    });

    assert!(
        responses_to_chat_completions_body(&body)
            .unwrap_err()
            .to_string()
            .contains("context_not_portable")
    );
    assert!(responses_to_anthropic_messages_body(&body).is_err());
    assert_eq!(body["input"][0]["content"], "continue");

    let missing = responses_to_chat_completions_body(&json!({
        "model":"provider-model",
        "input":[{"type":"compaction","encrypted_content":"opaque-window"}]
    }))
    .unwrap_err();
    assert!(missing.to_string().contains("context_not_portable"));

    let trigger = responses_to_chat_completions_body(&json!({
        "model":"provider-model",
        "input":[
            {"role":"user","content":"full context"},
            {"type":"compaction_trigger"}
        ]
    }))
    .unwrap_err();
    assert!(trigger.to_string().contains("context_not_portable"));

    let active_search = responses_to_chat_completions_body(&json!({
        "model":"provider-model",
        "input":[
            {"role":"user","content":"continue"},
            {"type":"web_search_call","status":"in_progress"}
        ]
    }))
    .unwrap_err();
    assert!(active_search.to_string().contains("web_search_call"));
}

#[test]
fn namespace_tools_expand_to_stable_unique_function_names_and_choices() {
    let body = json!({
        "model":"provider-model",
        "input":"hello",
        "tools":[{
            "type":"namespace",
            "name":"mcp__codey_fastctx",
            "tools":[{
                "type":"function",
                "name":"grep",
                "description":"search content",
                "parameters":{"type":"object","properties":{"pattern":{"type":"string"}}}
            }],
            "children":[{
                "type":"namespace",
                "name":"files",
                "tools":[{
                    "type":"function",
                    "name":"inspect_local_file_with_an_extremely_long_leaf_name",
                    "parameters":{"type":"object","properties":{"path":{"type":"string"}}}
                }]
            }]
        }],
        "tool_choice":{
            "type":"function",
            "namespace":"mcp__codey_fastctx.files",
            "name":"inspect_local_file_with_an_extremely_long_leaf_name"
        },
        "function_call":{"namespace":"mcp__codey_fastctx","name":"grep"}
    });

    let converted = responses_to_chat_completions_request(&body).unwrap();
    let repeated = responses_to_chat_completions_request(&body).unwrap();

    let grep_name = converted.body["tools"][0]["function"]["name"]
        .as_str()
        .unwrap();
    let inspect_name = converted.body["tools"][1]["function"]["name"]
        .as_str()
        .unwrap();
    assert!(grep_name.starts_with(NAMESPACE_UPSTREAM_TOOL_PREFIX));
    assert!(inspect_name.starts_with(NAMESPACE_UPSTREAM_TOOL_PREFIX));
    assert_ne!(grep_name, inspect_name);
    assert_ne!(grep_name, "grep");
    assert!(inspect_name.len() <= UPSTREAM_FUNCTION_NAME_MAX_BYTES);
    assert_eq!(
        repeated.body["tools"][0]["function"]["name"],
        converted.body["tools"][0]["function"]["name"]
    );
    assert_eq!(
        converted.body["tool_choice"]["function"]["name"],
        inspect_name
    );
    assert_eq!(converted.body["function_call"]["name"], grep_name);
}

#[test]
fn namespace_children_alias_accepts_function_tools() {
    let converted = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"hello",
        "tools":[{
            "type":"namespace",
            "name":"mcp_files",
            "children":[{
                "type":"function",
                "name":"read",
                "parameters":{"type":"object","properties":{"path":{"type":"string"}}}
            }]
        }]
    }))
    .unwrap();

    let upstream_name = converted.body["tools"][0]["function"]["name"]
        .as_str()
        .unwrap();
    assert!(upstream_name.starts_with(NAMESPACE_UPSTREAM_TOOL_PREFIX));
    assert_eq!(
        converted
            .tool_bridge
            .restore_upstream_name(upstream_name)
            .unwrap(),
        ResponsesToolName {
            kind: ResponsesToolKind::Function,
            namespace: vec!["mcp_files".to_string()],
            name: "read".to_string(),
        }
    );
}

#[test]
fn namespace_additional_tools_rewrite_historical_function_calls() {
    let converted = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":[
            {"role":"user","content":"hello"},
            {
                "type":"additional_tools",
                "role":"developer",
                "tools":[{
                    "type":"namespace",
                    "name":"fs",
                    "tools":[{
                        "type":"function",
                        "name":"read_file",
                        "parameters":{"type":"object","properties":{"path":{"type":"string"}}}
                    }]
                }]
            },
            {"type":"function_call","call_id":"call-1","namespace":"fs","name":"read_file","arguments":{"path":"a.txt"}},
            {"type":"message","role":"assistant","tool_calls":[{
                "id":"call-2",
                "type":"function",
                "namespace":"fs",
                "function":{"name":"read_file","arguments":"{}"}
            }]},
            {"type":"message","role":"assistant","function_call":{"namespace":["fs"],"name":"read_file"}}
        ]
    }))
    .unwrap();

    let upstream_name = converted.body["tools"][0]["function"]["name"]
        .as_str()
        .unwrap();
    assert_eq!(
        converted.body["messages"][1]["tool_calls"][0]["function"]["name"],
        upstream_name
    );
    assert_eq!(
        converted.body["messages"][2]["tool_calls"][0]["function"]["name"],
        upstream_name
    );
    assert_eq!(
        converted.body["messages"][3]["function_call"]["name"],
        upstream_name
    );
}

#[test]
fn custom_tools_wrap_definition_choice_history_and_result() {
    let patch = "*** Begin Patch\n*** Update File: README.md\n@@\n-old\n+new\n*** End Patch";
    let converted = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":[
            {"role":"user","content":"apply it"},
            {"type":"custom_tool_call","call_id":"call-patch","name":"apply_patch","input":patch},
            {"type":"custom_tool_call_output","call_id":"call-patch","output":"Done!"}
        ],
        "tools":[{
            "type":"custom",
            "name":"apply_patch",
            "description":"Apply a patch",
            "format":{"type":"grammar","syntax":"lark","definition":"start: /[\\s\\S]+/"}
        }],
        "tool_choice":{"type":"custom","name":"apply_patch"}
    }))
    .unwrap();

    let upstream_name = converted.body["tools"][0]["function"]["name"]
        .as_str()
        .unwrap();
    assert!(upstream_name.starts_with(CUSTOM_UPSTREAM_TOOL_PREFIX));
    assert!(upstream_name.len() <= UPSTREAM_FUNCTION_NAME_MAX_BYTES);
    assert_eq!(
        converted.body["tools"][0]["function"]["parameters"]["required"],
        json!(["input"])
    );
    assert_eq!(
        converted.body["tools"][0]["function"]["parameters"]["additionalProperties"],
        false
    );
    assert!(
        converted.body["tools"][0]["function"]["description"]
            .as_str()
            .unwrap()
            .contains("\"syntax\":\"lark\"")
    );
    assert_eq!(
        converted.body["tool_choice"]["function"]["name"],
        upstream_name
    );
    assert_eq!(
        converted.body["messages"][1]["tool_calls"][0]["function"]["name"],
        upstream_name
    );
    let wrapped = serde_json::from_str::<Value>(
        converted.body["messages"][1]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(wrapped["input"], patch);
    assert_eq!(converted.body["messages"][2]["role"], "tool");
    assert_eq!(converted.body["messages"][2]["tool_call_id"], "call-patch");
    assert_eq!(converted.body["messages"][2]["content"], "Done!");

    let anthropic = responses_to_anthropic_messages_body(&json!({
        "model":"provider-model",
        "input":[
            {"role":"user","content":"apply it"},
            {"type":"custom_tool_call","call_id":"call-patch","name":"apply_patch","input":patch},
            {"type":"custom_tool_call_output","call_id":"call-patch","output":"Done!"}
        ],
        "tools":[{"type":"custom","name":"apply_patch"}]
    }))
    .unwrap();
    assert!(
        anthropic["tools"][0]["name"]
            .as_str()
            .unwrap()
            .starts_with(CUSTOM_UPSTREAM_TOOL_PREFIX)
    );
    assert_eq!(
        anthropic["messages"][1]["content"][0]["input"]["input"],
        patch
    );
    assert_eq!(
        anthropic["messages"][2]["content"][0]["type"],
        "tool_result"
    );
    assert_eq!(
        anthropic["messages"][2]["content"][0]["tool_use_id"],
        "call-patch"
    );
}

#[test]
fn custom_response_calls_restore_for_chat_and_anthropic() {
    let converted = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"hello",
        "tools":[{"type":"custom","name":"apply_patch","description":"Apply a patch"}]
    }))
    .unwrap();
    let upstream_name = converted.body["tools"][0]["function"]["name"]
        .as_str()
        .unwrap();
    let raw_input = "*** Begin Patch\n*** End Patch";
    let wrapped = wrap_custom_tool_input(raw_input).unwrap();

    let chat = chat_completion_to_responses_body_with_tool_bridge(
        json!({
            "choices":[{"message":{"role":"assistant","tool_calls":[{
                "id":"call-chat",
                "type":"function",
                "function":{"name":upstream_name,"arguments":wrapped}
            }]},"finish_reason":"tool_calls"}]
        }),
        "provider-model",
        &converted.tool_bridge,
    )
    .unwrap();
    assert_eq!(chat["output"][0]["type"], "custom_tool_call");
    assert_eq!(chat["output"][0]["name"], "apply_patch");
    assert_eq!(chat["output"][0]["input"], raw_input);
    assert!(
        chat["output"][0]["id"]
            .as_str()
            .unwrap()
            .starts_with("ctc_codey_")
    );

    let anthropic = anthropic_message_to_responses_body_with_tool_bridge(
        &json!({
            "type":"message",
            "content":[{
                "type":"tool_use",
                "id":"call-anthropic",
                "name":upstream_name,
                "input":{"input":raw_input}
            }]
        }),
        "provider-model",
        &converted.tool_bridge,
    )
    .unwrap();
    assert_eq!(anthropic["output"][0]["type"], "custom_tool_call");
    assert_eq!(anthropic["output"][0]["name"], "apply_patch");
    assert_eq!(anthropic["output"][0]["input"], raw_input);
}

#[test]
fn namespace_custom_tools_bridge_for_anthropic_requests_and_responses() {
    let raw_input = "*** Begin Patch\n*** End Patch";
    let converted = responses_to_anthropic_messages_request(&json!({
        "model":"provider-model",
        "input":[
            "apply it",
            {
                "type":"custom_tool_call",
                "call_id":"call-patch",
                "namespace":"workspace",
                "name":"apply_patch",
                "input":raw_input
            }
        ],
        "tools":[{
            "type":"namespace",
            "name":"workspace",
            "tools":[{
                "type":"custom",
                "name":"apply_patch",
                "description":"Apply a patch"
            }]
        }],
        "tool_choice":{
            "type":"custom",
            "namespace":"workspace",
            "name":"apply_patch"
        }
    }))
    .unwrap();

    let upstream_name = converted.body["tools"][0]["name"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(upstream_name.starts_with(CUSTOM_UPSTREAM_TOOL_PREFIX));
    assert_eq!(
        converted.body["tool_choice"]["name"],
        Value::String(upstream_name.clone())
    );
    assert_eq!(
        converted.body["messages"][1]["content"][0]["name"],
        Value::String(upstream_name.clone())
    );
    assert_eq!(
        converted.body["messages"][1]["content"][0]["input"]["input"],
        raw_input
    );

    let restored = anthropic_message_to_responses_body_with_tool_bridge(
        &json!({
            "type":"message",
            "content":[{
                "type":"tool_use",
                "id":"call-result",
                "name":upstream_name,
                "input":{"input":raw_input}
            }]
        }),
        "provider-model",
        &converted.tool_bridge,
    )
    .unwrap();
    assert_eq!(restored["output"][0]["type"], "custom_tool_call");
    assert_eq!(restored["output"][0]["namespace"], "workspace");
    assert_eq!(restored["output"][0]["name"], "apply_patch");
    assert_eq!(restored["output"][0]["input"], raw_input);
}

#[test]
fn custom_tools_deduplicate_identical_definitions_and_reject_conflicts() {
    let definition = json!({
        "type":"custom",
        "name":"apply_patch",
        "format":{"type":"grammar","syntax":"lark","definition":"start: /.+/"}
    });
    let converted = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"hello",
        "tools":[definition.clone(), definition]
    }))
    .unwrap();
    assert_eq!(converted.body["tools"].as_array().unwrap().len(), 1);

    let conflict = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"hello",
        "tools":[
            {"type":"custom","name":"apply_patch","description":"first"},
            {"type":"custom","name":"apply_patch","description":"second"}
        ]
    }))
    .unwrap_err();
    assert!(conflict.to_string().contains("定义冲突"));

    let generated = custom_upstream_tool_name(&[], "apply_patch");
    let collision = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"hello",
        "tools":[
            {"type":"function","name":generated,"parameters":{"type":"object"}},
            {"type":"custom","name":"apply_patch"}
        ]
    }))
    .unwrap_err();
    assert!(collision.to_string().contains("冲突"));
}

#[test]
fn custom_streaming_waits_for_the_complete_json_wrapper() {
    let converted = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"hello",
        "tools":[{"type":"custom","name":"apply_patch"}]
    }))
    .unwrap();
    let upstream_name = converted.body["tools"][0]["function"]["name"]
        .as_str()
        .unwrap();
    let mut stream = ResponsesSseState::new("provider-model", &converted.tool_bridge);

    let events = stream
        .tool_delta(
            0,
            Some("call-patch"),
            Some(upstream_name),
            Some("{\"input\":\"*** Begin"),
            None,
        )
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["type"], "response.output_item.added");
    assert_eq!(events[0]["item"]["type"], "custom_tool_call");
    assert_eq!(events[0]["item"]["name"], "apply_patch");
    assert_eq!(events[0]["item"]["input"], "");

    let events = stream
        .tool_delta(0, None, None, Some(" Patch\"}"), None)
        .unwrap();
    assert!(events.is_empty());
    assert_eq!(
        custom_tool_input_from_arguments(
            &stream.tools.get(&0).unwrap().arguments,
            "test custom arguments"
        )
        .unwrap(),
        "*** Begin Patch"
    );
}

#[test]
fn client_tool_search_bridges_to_stable_chat_and_anthropic_functions() {
    let body = json!({
        "model":"provider-model",
        "input":"find a filesystem tool",
        "tools":[client_tool_search_definition()],
        "tool_choice":{"type":"tool_search"}
    });

    let chat = responses_to_chat_completions_request(&body).unwrap();
    let repeated = responses_to_chat_completions_request(&body).unwrap();
    assert_eq!(
        chat.body["tools"][0]["function"]["name"],
        TOOL_SEARCH_UPSTREAM_TOOL_NAME
    );
    assert_eq!(
        repeated.body["tools"][0]["function"]["name"],
        chat.body["tools"][0]["function"]["name"]
    );
    assert_eq!(
        chat.body["tools"][0]["function"]["description"],
        "Search the client tool catalog"
    );
    assert_eq!(
        chat.body["tools"][0]["function"]["parameters"],
        client_tool_search_definition()["parameters"]
    );
    assert_eq!(
        chat.body["tool_choice"]["function"]["name"],
        TOOL_SEARCH_UPSTREAM_TOOL_NAME
    );
    assert_eq!(
        chat.tool_bridge
            .restore_upstream_name(TOOL_SEARCH_UPSTREAM_TOOL_NAME)
            .unwrap(),
        ResponsesToolName::tool_search()
    );

    let anthropic = responses_to_anthropic_messages_request(&body).unwrap();
    assert_eq!(
        anthropic.body["tools"][0]["name"],
        TOOL_SEARCH_UPSTREAM_TOOL_NAME
    );
    assert_eq!(
        anthropic.body["tools"][0]["description"],
        "Search the client tool catalog"
    );
    assert_eq!(
        anthropic.body["tools"][0]["input_schema"],
        client_tool_search_definition()["parameters"]
    );
    assert_eq!(
        anthropic.body["tool_choice"]["name"],
        TOOL_SEARCH_UPSTREAM_TOOL_NAME
    );
}

#[test]
fn client_tool_search_requires_description_and_object_parameters() {
    for tool in [
        json!({
            "type":"tool_search",
            "execution":"client",
            "parameters":{"type":"object"}
        }),
        json!({
            "type":"tool_search",
            "execution":"client",
            "description":"Search the client tool catalog"
        }),
        json!({
            "type":"tool_search",
            "execution":"client",
            "description":"Search the client tool catalog",
            "parameters":[]
        }),
    ] {
        let error = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"find a tool",
            "tools":[tool]
        }))
        .unwrap_err();
        assert!(
            error.to_string().contains("tool_search.description")
                || error.to_string().contains("tool_search.parameters")
        );
    }

    let conflict = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"find a tool",
        "tools":[
            client_tool_search_definition(),
            {
                "type":"tool_search",
                "execution":"client",
                "description":"A conflicting search definition",
                "parameters":{"type":"object","properties":{}}
            }
        ]
    }))
    .unwrap_err();
    assert!(conflict.to_string().contains("tool_search 存在定义冲突"));
}

#[test]
fn client_tool_search_history_requires_client_execution_and_object_arguments() {
    for output in [
        json!({
            "type":"tool_search_output",
            "call_id":"call-search-history",
            "tools":[]
        }),
        json!({
            "type":"tool_search_output",
            "execution":"server",
            "call_id":"call-search-history",
            "tools":[]
        }),
    ] {
        let error = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "tools":[client_tool_search_definition()],
            "input":[output]
        }))
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("tool_search_output 缺少 execution=client")
                || error
                    .to_string()
                    .contains("tool_search_output.execution=server 不受支持")
        );
    }

    for arguments in [json!([]), json!("not an object")] {
        let error = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "tools":[client_tool_search_definition()],
            "input":[{
                "type":"tool_search_call",
                "execution":"client",
                "call_id":"call-search-history",
                "arguments":arguments
            }]
        }))
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("tool_search_call.arguments 必须是 JSON 对象")
        );
    }
}

#[test]
fn client_tool_search_calls_restore_for_chat_and_anthropic() {
    let converted = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"find a tool",
        "tools":[client_tool_search_definition()]
    }))
    .unwrap();
    let arguments = "{\"goal\":\"filesystem access\"}";

    let chat = chat_completion_to_responses_body_with_tool_bridge(
        json!({
            "choices":[{"message":{"role":"assistant","tool_calls":[{
                "id":"call-search-chat",
                "type":"function",
                "function":{"name":TOOL_SEARCH_UPSTREAM_TOOL_NAME,"arguments":arguments}
            }]},"finish_reason":"tool_calls"}]
        }),
        "provider-model",
        &converted.tool_bridge,
    )
    .unwrap();
    assert_eq!(chat["output"][0]["type"], "tool_search_call");
    assert_eq!(chat["output"][0]["execution"], "client");
    assert_eq!(chat["output"][0]["call_id"], "call-search-chat");
    assert_eq!(chat["output"][0]["status"], "completed");
    assert_eq!(
        chat["output"][0]["arguments"],
        json!({"goal":"filesystem access"})
    );
    assert!(chat["output"][0].get("name").is_none());

    let anthropic = anthropic_message_to_responses_body_with_tool_bridge(
        &json!({
            "type":"message",
            "content":[{
                "type":"tool_use",
                "id":"call-search-anthropic",
                "name":TOOL_SEARCH_UPSTREAM_TOOL_NAME,
                "input":{"goal":"calendar tools"}
            }]
        }),
        "provider-model",
        &converted.tool_bridge,
    )
    .unwrap();
    assert_eq!(anthropic["output"][0]["type"], "tool_search_call");
    assert_eq!(anthropic["output"][0]["execution"], "client");
    assert_eq!(anthropic["output"][0]["call_id"], "call-search-anthropic");
    assert_eq!(
        anthropic["output"][0]["arguments"],
        json!({"goal":"calendar tools"})
    );
}

#[test]
fn client_tool_search_history_round_trips_and_promotes_loaded_tools() {
    let converted = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "tools":[client_tool_search_definition()],
        "input":[
            {"type":"message","role":"user","content":"find a tool"},
            {
                "type":"tool_search_call",
                "execution":"client",
                "call_id":"call-search-history",
                "status":"completed",
                "arguments":{"goal":"read files"}
            },
            {
                "type":"tool_search_output",
                "execution":"client",
                "call_id":"call-search-history",
                "status":"completed",
                "tools":[{
                    "type":"function",
                    "name":"read_file",
                    "description":"Read a file",
                    "defer_loading":true,
                    "parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}
                },{
                    "type":"namespace",
                    "name":"filesystem",
                    "tools":[{
                        "type":"function",
                        "name":"list_files",
                        "description":"List files",
                        "defer_loading":true,
                        "parameters":{"type":"object","properties":{}}
                    }]
                }]
            }
        ]
    }))
    .unwrap();

    assert_eq!(converted.body["tools"].as_array().unwrap().len(), 3);
    assert_eq!(
        converted.body["tools"][0]["function"]["name"],
        TOOL_SEARCH_UPSTREAM_TOOL_NAME
    );
    assert_eq!(converted.body["tools"][1]["function"]["name"], "read_file");
    assert!(
        converted.body["tools"][1]["function"]
            .get("defer_loading")
            .is_none()
    );
    assert_eq!(
        converted.body["tools"][2]["function"]["name"],
        namespaced_upstream_tool_name(&["filesystem".to_string()], "list_files")
    );
    assert!(
        converted.body["tools"][2]["function"]
            .get("defer_loading")
            .is_none()
    );
    assert_eq!(
        converted.body["messages"][1]["tool_calls"][0]["function"]["name"],
        TOOL_SEARCH_UPSTREAM_TOOL_NAME
    );
    assert_eq!(
        converted.body["messages"][1]["tool_calls"][0]["id"],
        "call-search-history"
    );
    assert_eq!(
        converted.body["messages"][1]["tool_calls"][0]["function"]["arguments"],
        "{\"goal\":\"read files\"}"
    );
    assert_eq!(converted.body["messages"][2]["role"], "tool");
    assert_eq!(
        converted.body["messages"][2]["tool_call_id"],
        "call-search-history"
    );
    let result =
        serde_json::from_str::<Value>(converted.body["messages"][2]["content"].as_str().unwrap())
            .unwrap();
    assert_eq!(result["tools"][0]["name"], "read_file");

    let anthropic = responses_to_anthropic_messages_request(&json!({
        "model":"provider-model",
        "tools":[client_tool_search_definition()],
        "input":[
            {"role":"user","content":"find a tool"},
            {"type":"tool_search_call","execution":"client","call_id":"call-search-history","arguments":{"goal":"read files"}},
            {"type":"tool_search_output","execution":"client","call_id":"call-search-history","tools":[{"type":"function","name":"read_file","defer_loading":true,"parameters":{"type":"object"}}]}
        ]
    }))
    .unwrap();
    assert_eq!(anthropic.body["tools"].as_array().unwrap().len(), 2);
    assert_eq!(
        anthropic.body["messages"][1]["content"][0]["type"],
        "tool_use"
    );
    assert_eq!(
        anthropic.body["messages"][1]["content"][0]["input"],
        json!({"goal":"read files"})
    );
    assert_eq!(
        anthropic.body["messages"][2]["content"][0]["type"],
        "tool_result"
    );
}

#[test]
fn client_tool_search_streaming_restores_native_items_without_function_events() {
    let converted = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"find a tool",
        "tools":[client_tool_search_definition()]
    }))
    .unwrap();
    let mut stream = ResponsesSseState::new("provider-model", &converted.tool_bridge);

    let first = stream
        .tool_delta(
            0,
            Some("call-search-stream"),
            Some(TOOL_SEARCH_UPSTREAM_TOOL_NAME),
            Some("{\"goal\":"),
            None,
        )
        .unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["type"], "response.output_item.added");
    assert_eq!(first[0]["item"]["type"], "tool_search_call");
    assert_eq!(first[0]["item"]["execution"], "client");
    assert_eq!(first[0]["item"]["arguments"], json!({}));
    assert!(
        !serde_json::to_string(&first)
            .unwrap()
            .contains("function_call_arguments")
    );

    let second = stream
        .tool_delta(0, None, None, Some("\"filesystem\"}"), None)
        .unwrap();
    assert!(second.is_empty());
    let done = stream_tool_item(stream.tools.get(&0).unwrap()).unwrap();
    assert_eq!(done["type"], "tool_search_call");
    assert_eq!(done["execution"], "client");
    assert_eq!(done["call_id"], "call-search-stream");
    assert_eq!(done["arguments"], json!({"goal":"filesystem"}));
}

#[test]
fn responses_web_search_drops_ambient_auto_but_rejects_omitted_selection() {
    let omitted = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"normal conversation",
        "tools":[{"type":"web_search"}]
    }))
    .unwrap_err();
    assert!(omitted.to_string().contains("可选 web_search"));

    let auto = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"normal conversation",
        "tools":[
            {"type":"web_search"},
            {"type":"function","name":"lookup","parameters":{"type":"object","properties":{}}}
        ],
        "tool_choice":"auto"
    }))
    .unwrap();
    assert!(auto.body.get("web_search_options").is_none());
    assert_eq!(auto.body["tool_choice"], "auto");
    assert_eq!(auto.body["tools"].as_array().unwrap().len(), 1);
    assert_eq!(auto.body["tools"][0]["function"]["name"], "lookup");

    let additional = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":[
            {"role":"user","content":"search the web"},
            {"type":"additional_tools","role":"developer","tools":[{"type":"web_search"}]}
        ]
    }))
    .unwrap_err();
    assert!(additional.to_string().contains("可选 web_search"));

    let chat = responses_to_chat_completions_request(&json!({
        "model":"gpt-5-search-api",
        "input":"search the web",
        "tools":[{
            "type":"web_search_preview",
            "search_context_size":"low",
            "user_location":{
                "type":"approximate",
                "country":"US",
                "city":"San Francisco",
                "region":"California",
                "timezone":"America/Los_Angeles"
            }
        }],
        "tool_choice":{"type":"web_search_preview"}
    }))
    .unwrap();

    assert!(chat.body.get("tools").is_none());
    assert!(chat.body.get("tool_choice").is_none());
    assert_eq!(
        chat.body["web_search_options"],
        json!({
            "search_context_size":"low",
            "user_location":{
                "type":"approximate",
                "approximate":{
                    "country":"US",
                    "city":"San Francisco",
                    "region":"California",
                    "timezone":"America/Los_Angeles"
                }
            }
        })
    );
}

#[test]
fn responses_web_search_required_only_maps_and_required_mixed_fails_closed() {
    let required = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"search the web",
        "tools":[{"type":"web_search","search_context_size":"medium"}],
        "tool_choice":"required"
    }))
    .unwrap();
    assert_eq!(
        required.body["web_search_options"],
        json!({"search_context_size":"medium"})
    );
    assert!(required.body.get("tools").is_none());
    assert!(required.body.get("tool_choice").is_none());

    let error = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"use a required tool",
        "tools":[
            {"type":"web_search"},
            {"type":"function","name":"lookup","parameters":{"type":"object","properties":{}}}
        ],
        "tool_choice":"required"
    }))
    .unwrap_err();
    assert!(error.to_string().contains("tool_choice=required"));
    assert!(error.to_string().contains("无法无损表达"));

    let explicit_mixed = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"search the web",
        "tools":[
            {"type":"web_search"},
            {"type":"function","name":"lookup","parameters":{"type":"object","properties":{}}}
        ],
        "tool_choice":{"type":"web_search"}
    }))
    .unwrap_err();
    assert!(explicit_mixed.to_string().contains("仍包含其他工具"));

    let sources = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"search the web",
        "tools":[{"type":"web_search"}],
        "tool_choice":{"type":"web_search"},
        "include":["web_search_call.action.sources"]
    }))
    .unwrap_err();
    assert!(sources.to_string().contains("action.sources"));
}

#[test]
fn responses_explicit_web_search_requires_a_declared_search_tool() {
    let error = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"search the web",
        "tool_choice":{"type":"web_search"}
    }))
    .unwrap_err();
    assert!(error.to_string().contains("未在 tools 中声明的 web_search"));
}

#[test]
fn responses_web_search_respects_none_and_function_tool_choice() {
    let tools = json!([
        {"type":"web_search"},
        {"type":"function","name":"lookup","parameters":{"type":"object","properties":{}}}
    ]);
    let none = responses_to_chat_completions_request(&json!({
        "model":"gpt-5-search-api",
        "input":"search disabled",
        "tools":tools,
        "tool_choice":"none"
    }))
    .unwrap();
    assert!(none.body.get("web_search_options").is_none());
    assert_eq!(none.body["tool_choice"], "none");
    assert_eq!(none.body["tools"].as_array().unwrap().len(), 1);

    let function = responses_to_chat_completions_request(&json!({
        "model":"gpt-5-search-api",
        "input":"call lookup",
        "tools":tools,
        "tool_choice":{"type":"function","name":"lookup"}
    }))
    .unwrap();
    assert!(function.body.get("web_search_options").is_none());
    assert_eq!(function.body["tool_choice"]["function"]["name"], "lookup");
}

#[test]
fn responses_web_search_rejects_unsupported_chat_options_and_anthropic() {
    for tool in [
        json!({"type":"web_search","filters":{"allowed_domains":["example.com"]}}),
        json!({"type":"web_search","return_token_budget":2048}),
        json!({"type":"web_search","external_web_access":false}),
        json!({"type":"web_search","search_context_size":"tiny"}),
    ] {
        let error = responses_to_chat_completions_request(&json!({
            "model":"gpt-5-search-api",
            "input":"search",
            "tools":[tool]
        }))
        .unwrap_err();
        assert!(
            error.to_string().contains("web_search")
                || error.to_string().contains("external_web_access")
        );
    }

    let error = responses_to_anthropic_messages_request(&json!({
        "model":"claude-test",
        "input":"search",
        "tools":[{"type":"web_search"}],
        "tool_choice":{"type":"web_search"}
    }))
    .unwrap_err();
    assert!(
        error.to_string().contains("web_search_options")
            || error.to_string().contains("支持 Responses 的线路")
    );

    let error = responses_to_anthropic_messages_request(&json!({
        "model":"claude-test",
        "input":"search",
        "tools":[{"type":"web_search"}]
    }))
    .unwrap_err();
    assert!(error.to_string().contains("可选 web_search"));

    let ambient = responses_to_anthropic_messages_request(&json!({
        "model":"claude-test",
        "input":"normal conversation",
        "tools":[{"type":"web_search"}],
        "tool_choice":"auto"
    }))
    .unwrap();
    assert!(ambient.body.get("tools").is_none());
    assert!(ambient.body.get("tool_choice").is_none());
}

#[test]
fn chat_search_annotations_are_preserved_in_responses_output_text() {
    let body = chat_completion_to_responses_body(
        json!({
            "id":"chatcmpl-search",
            "created":123,
            "choices":[{
                "message":{
                    "role":"assistant",
                    "content":"OpenAI released a search model.",
                    "annotations":[{
                        "type":"url_citation",
                        "url_citation":{
                            "url":"https://openai.com/",
                            "title":"OpenAI",
                            "start_index":0,
                            "end_index":6
                        }
                    }]
                },
                "finish_reason":"stop"
            }]
        }),
        "gpt-5-search-api",
    )
    .unwrap();

    assert_eq!(
        body["output"][0]["content"][0]["annotations"][0]["url_citation"]["url"],
        "https://openai.com/"
    );
}

#[test]
fn namespace_tools_fail_closed_on_conflicts_and_unsupported_tool_kinds() {
    let duplicate = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"hello",
        "tools":[{
            "type":"namespace",
            "name":"fs",
            "tools":[
                {"type":"function","name":"read","parameters":{"type":"object","properties":{"path":{"type":"string"}}}},
                {"type":"function","name":"read","parameters":{"type":"object","properties":{"path":{"type":"number"}}}}
            ]
        }]
    }))
    .unwrap_err();
    assert!(duplicate.to_string().contains("存在定义冲突"));

    for tool in [
        json!({"type":"tool_search"}),
        json!({"type":"tool_search","execution":"server"}),
    ] {
        let error = responses_to_chat_completions_request(&json!({
            "model":"provider-model",
            "input":"hello",
            "tools":[tool]
        }))
        .unwrap_err();
        assert!(
            error.to_string().contains("支持 Responses 的线路")
                || error.to_string().contains("仅 execution=client 可桥接")
        );
    }
}

#[test]
fn namespace_expansion_rejects_collisions_with_plain_function_names() {
    let generated = namespaced_upstream_tool_name(&["fs".to_string()], "read");

    let error = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"hello",
        "tools":[
            {"type":"function","name":generated,"parameters":{"type":"object","properties":{}}},
            {"type":"namespace","name":"fs","tools":[
                {"type":"function","name":"read","parameters":{"type":"object","properties":{}}}
            ]}
        ]
    }))
    .unwrap_err();

    assert!(error.to_string().contains("冲突"));
}

#[test]
fn namespace_response_names_restore_for_chat_and_anthropic_outputs() {
    let converted = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"hello",
        "tools":[{"type":"namespace","name":"fs","tools":[
            {"type":"function","name":"read","parameters":{"type":"object","properties":{}}}
        ]}]
    }))
    .unwrap();
    let upstream_name = converted.body["tools"][0]["function"]["name"]
        .as_str()
        .unwrap();

    let chat = chat_completion_to_responses_body_with_tool_bridge(
        json!({
            "choices":[{"message":{"role":"assistant","tool_calls":[{
                "id":"call-chat",
                "type":"function",
                "function":{"name":upstream_name,"arguments":"{\"path\":\"a.txt\"}"}
            }]},"finish_reason":"tool_calls"}]
        }),
        "provider-model",
        &converted.tool_bridge,
    )
    .unwrap();
    assert_eq!(chat["output"][0]["name"], "read");
    assert_eq!(chat["output"][0]["namespace"], "fs");

    let anthropic = anthropic_message_to_responses_body_with_tool_bridge(
        &json!({
            "type":"message",
            "content":[{"type":"tool_use","id":"call-anthropic","name":upstream_name,"input":{"path":"a.txt"}}]
        }),
        "provider-model",
        &converted.tool_bridge,
    )
    .unwrap();
    assert_eq!(anthropic["output"][0]["name"], "read");
    assert_eq!(anthropic["output"][0]["namespace"], "fs");
}

#[test]
fn namespace_streaming_restores_added_and_done_items_after_complete_name() {
    let converted = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"hello",
        "tools":[{"type":"namespace","name":"fs","tools":[
            {"type":"function","name":"read","parameters":{"type":"object","properties":{}}}
        ]}]
    }))
    .unwrap();
    let upstream_name = converted.body["tools"][0]["function"]["name"]
        .as_str()
        .unwrap();
    let split = 5;
    let mut stream = ResponsesSseState::new("provider-model", &converted.tool_bridge);

    let first = stream
        .tool_delta(
            0,
            Some("call-stream"),
            Some(&upstream_name[..split]),
            Some("{"),
            None,
        )
        .unwrap();
    assert!(first.is_empty());

    let second = stream
        .tool_delta(
            0,
            None,
            Some(&upstream_name[split..]),
            Some("\"path\":\"a.txt\"}"),
            None,
        )
        .unwrap();
    assert_eq!(second[0]["type"], "response.output_item.added");
    assert_eq!(second[0]["item"]["name"], "read");
    assert_eq!(second[0]["item"]["namespace"], "fs");
    assert_eq!(second[1]["delta"], "{\"path\":\"a.txt\"}");

    let done = stream_tool_item(stream.tools.get(&0).unwrap()).unwrap();
    assert_eq!(done["name"], "read");
    assert_eq!(done["namespace"], "fs");
    assert_eq!(done["arguments"], "{\"path\":\"a.txt\"}");
}

#[test]
fn streaming_waits_for_a_complete_declared_function_name() {
    let converted = responses_to_chat_completions_request(&json!({
        "model":"provider-model",
        "input":"hello",
        "tools":[
            {"type":"function","name":"look","parameters":{"type":"object"}},
            {"type":"function","name":"lookup","parameters":{"type":"object"}}
        ]
    }))
    .unwrap();
    let mut stream = ResponsesSseState::new("provider-model", &converted.tool_bridge);

    let first = stream
        .tool_delta(0, Some("call-stream"), Some("look"), None, None)
        .unwrap();
    assert!(first.is_empty());

    let second = stream
        .tool_delta(0, None, Some("up"), Some("{}"), None)
        .unwrap();
    assert_eq!(second[0]["type"], "response.output_item.added");
    assert_eq!(second[0]["item"]["name"], "lookup");
    assert_eq!(second[1]["delta"], "{}");
}

#[test]
fn request_log_adapters_preserve_cache_write_usage_and_missing_values() {
    for writes in [None, Some(0_u64), Some(7)] {
        for protocol in ["chat", "anthropic"] {
            let response = if protocol == "chat" {
                chat_completion_to_responses_body(json!({
                    "choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":20,"completion_tokens":2,
                        "prompt_tokens_details":{"cached_tokens":3,"cache_write_tokens":writes}}
                }), "test-model").unwrap()
            } else {
                anthropic_message_to_responses_body(
                    &json!({
                        "content":[{"type":"text","text":"ok"}], "stop_reason":"end_turn",
                        "usage":{"input_tokens":10,"output_tokens":2,
                            "cache_read_input_tokens":3,"cache_creation_input_tokens":writes}
                    }),
                    "test-model",
                )
                .unwrap()
            };
            let details = &response["usage"]["input_tokens_details"];
            assert_eq!(
                details.get("cache_write_tokens").and_then(Value::as_u64),
                writes
            );
            assert_eq!(
                details.get("cache_write_tokens").is_some(),
                writes.is_some()
            );
            let probe = RouteRequestLogProbe::detached_test_probe();
            let mut projector = RequestLogMetadataProjector::default();
            let event = format!(
                "data: {}\n\n",
                json!({"type":"response.completed","response":response})
            );
            for chunk in event.as_bytes().chunks(3) {
                projector.observe(chunk, &probe, Instant::now()).unwrap();
            }
            assert_eq!(
                probe.token_usage_for_test().cache_creation_input_tokens,
                writes
            );
        }
    }
    for field in [
        "cache_creation_input_tokens",
        "cache_creation_tokens",
        "cache_write_input_tokens",
    ] {
        let mut usage = json!({"input_tokens":20,"output_tokens":2});
        usage[field] = json!(7);
        let converted = sse_chat::chat_usage_to_responses_usage(&usage);
        assert_eq!(converted["input_tokens_details"]["cache_write_tokens"], 7);
        usage["input_tokens_details"] = json!({"cache_write_tokens":0});
        assert_eq!(
            sse_chat::chat_usage_to_responses_usage(&usage)["input_tokens_details"]["cache_write_tokens"],
            0
        );
    }
}

#[test]
fn chat_response_converts_parallel_tools_and_usage_to_responses() {
    let responses = chat_completion_to_responses_body(
        json!({
            "id": "chatcmpl-1",
            "created": 123,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "checking",
                    "tool_calls": [
                        {"id":"call-a","type":"function","function":{"name":"first","arguments":"{\"a\":1}"}},
                        {"id":"call-b","type":"function","function":{"name":"second","arguments":"{\"b\":2}"}}
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15,
                "prompt_tokens_details": {"cached_tokens": 3},
                "completion_tokens_details": {"reasoning_tokens": 2}
            }
        }),
        "provider-model",
    )
    .unwrap();

    assert_eq!(responses["id"], "chatcmpl-1");
    assert_eq!(responses["model"], "provider-model");
    assert_eq!(responses["output"][0]["content"][0]["text"], "checking");
    assert_eq!(responses["output"][1]["type"], "function_call");
    assert_eq!(responses["output"][1]["call_id"], "call-a");
    assert_eq!(responses["output"][2]["name"], "second");
    assert_eq!(responses["usage"]["input_tokens"], 10);
    assert_eq!(
        responses["usage"]["input_tokens_details"]["cached_tokens"],
        3
    );
    assert_eq!(
        responses["usage"]["output_tokens_details"]["reasoning_tokens"],
        2
    );
}

#[test]
fn responses_request_converts_messages_images_tools_and_results_to_anthropic() {
    let anthropic = responses_to_anthropic_messages_body(&json!({
        "model":"claude-sonnet-test",
        "instructions":"Be concise",
        "input":[
            {
                "type":"message",
                "role":"user",
                "content":[
                    {"type":"input_text","text":"inspect"},
                    {"type":"input_image","image_url":"data:image/png;base64,aGVsbG8="}
                ]
            },
            {"type":"function_call","call_id":"call-1","name":"lookup","arguments":"{\"q\":1}"},
            {"type":"function_call_output","call_id":"call-1","output":"done"}
        ],
        "tools":[{
            "type":"function",
            "name":"lookup",
            "description":"lookup data",
            "parameters":{"type":"object","properties":{"q":{"type":"number"}}}
        }],
        "tool_choice":{"type":"function","name":"lookup"},
        "parallel_tool_calls":false,
        "reasoning":{"effort":"high"},
        "max_output_tokens":2048,
        "stream":true
    }))
    .unwrap();

    assert_eq!(anthropic["system"], "Be concise");
    assert_eq!(anthropic["model"], "claude-sonnet-test");
    assert_eq!(anthropic["max_tokens"], 2048);
    assert_eq!(anthropic["messages"][0]["role"], "user");
    assert_eq!(
        anthropic["messages"][0]["content"][1]["source"]["type"],
        "base64"
    );
    assert_eq!(anthropic["messages"][1]["content"][0]["type"], "tool_use");
    assert_eq!(anthropic["messages"][1]["content"][0]["input"]["q"], 1);
    assert_eq!(
        anthropic["messages"][2]["content"][0]["type"],
        "tool_result"
    );
    assert_eq!(anthropic["tools"][0]["name"], "lookup");
    assert_eq!(anthropic["tools"][0]["input_schema"]["type"], "object");
    assert_eq!(anthropic["tool_choice"]["type"], "tool");
    assert_eq!(anthropic["tool_choice"]["name"], "lookup");
    assert_eq!(anthropic["tool_choice"]["disable_parallel_tool_use"], true);
    assert_eq!(anthropic["output_config"]["effort"], "high");
    assert_eq!(anthropic["stream"], true);
}

#[test]
fn anthropic_response_converts_text_tools_and_cache_usage_to_responses() {
    let responses = anthropic_message_to_responses_body(
        &json!({
            "id":"msg-anthropic",
            "type":"message",
            "role":"assistant",
            "model":"claude-sonnet-test",
            "content":[
                {"type":"thinking","thinking":"must stay private"},
                {"type":"text","text":"checking"},
                {"type":"tool_use","id":"tool-1","name":"lookup","input":{"q":1}}
            ],
            "stop_reason":"tool_use",
            "usage":{
                "input_tokens":10,
                "cache_creation_input_tokens":2,
                "cache_read_input_tokens":3,
                "output_tokens":5
            }
        }),
        "fallback-model",
    )
    .unwrap();

    assert_eq!(responses["model"], "claude-sonnet-test");
    assert_eq!(responses["output_text"], "checking");
    assert_eq!(responses["output"][0]["content"][0]["text"], "checking");
    assert_eq!(responses["output"][1]["type"], "function_call");
    assert_eq!(responses["output"][1]["call_id"], "tool-1");
    assert_eq!(responses["output"][1]["arguments"], "{\"q\":1}");
    assert!(!responses.to_string().contains("must stay private"));
    assert_eq!(responses["usage"]["input_tokens"], 15);
    assert_eq!(
        responses["usage"]["input_tokens_details"]["cached_tokens"],
        3
    );
    assert_eq!(responses["usage"]["total_tokens"], 20);
}

#[test]
fn anthropic_sse_accumulates_text_tool_arguments_and_usage() {
    let sse = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-stream\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-sonnet-test\",\"content\":[],\"usage\":{\"input_tokens\":4}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool-stream\",\"name\":\"lookup\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":1}\"}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":3}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );

    let message = parse_anthropic_message_sse_bytes(sse.as_bytes(), "fallback").unwrap();
    let responses = anthropic_message_to_responses_body(&message, "fallback").unwrap();

    assert_eq!(responses["output_text"], "hello");
    assert_eq!(responses["output"][1]["call_id"], "tool-stream");
    assert_eq!(responses["output"][1]["arguments"], "{\"q\":1}");
    assert_eq!(responses["usage"]["input_tokens"], 4);
    assert_eq!(responses["usage"]["output_tokens"], 3);
}

#[test]
fn upstream_error_summary_extracts_context_and_redacts_route_credentials() {
    let (config, provider_id, _) = router_config("https://relay.example/v1".into());
    let snapshot = RouterSnapshot::from_config(&config);
    let route = snapshot.routes.get(&provider_id).unwrap();
    let original = "  <html>\n  gateway error: Bearer sk-upstream\n</html>\n";
    assert_eq!(
        redact_upstream_error_text(original, route),
        "  <html>\n  gateway error: ***\n</html>\n"
    );
    let summary = upstream_error_summary(
        &json!({
            "error": {
                "message":"image is too large for sk-upstream\nplease resize it",
                "type":"invalid_request_error",
                "code":"image_too_large"
            }
        }),
        route,
    );

    assert_eq!(
        summary.message.as_deref(),
        Some("image is too large for *** please resize it")
    );
    assert_eq!(summary.error_type.as_deref(), Some("invalid_request_error"));
    assert_eq!(summary.code.as_deref(), Some("image_too_large"));
    assert_eq!(
        upstream_error_detail(&summary).as_deref(),
        Some(
            "image is too large for *** please resize it（类型：invalid_request_error；代码：image_too_large）"
        )
    );
}

#[test]
fn websocket_failure_event_keeps_provider_codes_and_adds_route_context() {
    let (config, provider_id, _) = router_config("https://relay.example/v1".into());
    let snapshot = RouterSnapshot::from_config(&config);
    let route = snapshot.routes.get(&provider_id).unwrap();
    let mut event = json!({
        "type":"response.failed",
        "response":{
            "error":{
                "message":"openai_error",
                "type":"bad_response_status_code",
                "code":"bad_response_status_code"
            }
        }
    });

    let error_summary = annotate_upstream_websocket_failure(
        &mut event,
        route,
        "provider-model",
        "wss://relay.example/v1/responses",
    );

    let error = &event["response"]["error"];
    assert_eq!(error["type"], "bad_response_status_code");
    assert_eq!(error["code"], "bad_response_status_code");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("线路「Relay」"));
    assert!(message.contains("provider-model"));
    assert!(message.contains("openai_error"));
    assert!(message.contains("bad_response_status_code"));
    assert_eq!(
        error_summary.as_deref(),
        Some("type=bad_response_status_code; code=bad_response_status_code")
    );
}

#[tokio::test]
async fn native_responses_upstream_http_error_is_preserved_as_safe_text() {
    let logs = tempfile::tempdir().unwrap();
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let original = format!(
        "  {{\n  \"error\":{{\"message\":\"image exceeds the provider limit for sk-upstream\",\"type\":\"invalid_request_error\",\"code\":\"image_too_large\"}},\n  \"detail\":\"{}\"\n}}\n",
        "供应商诊断 ".repeat(100)
    );
    let expected = original.replace("sk-upstream", "***");
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let body = original;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\nx-request-id: upstream-request-123\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        request.path
    });
    let (mut config, provider_id, model) =
        router_config(format!("http://{upstream_address}/v1/responses"));
    config.route_request_log.enabled = true;
    config.route_request_log.backend = crate::config::RouteRequestLogBackend::Sqlite;
    let router = LocalRouter::start_with_logger(
        &config,
        Arc::new(RouteRequestLogController::with_root(
            logs.path().to_path_buf(),
        )),
    )
    .await
    .unwrap();
    let endpoint = router.endpoint();

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({
            "model": model_alias(&provider_id, &model),
            "input":"inspect the image",
            "stream":true
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/plain")
    );
    let body = response.text().await.unwrap();
    assert!(body.contains("线路「Relay」"));
    assert!(body.contains("provider-model"));
    assert!(body.contains("HTTP 429"));
    assert!(body.contains("image exceeds the provider limit for ***"));
    assert!(body.contains("image_too_large"));
    assert!(body.contains("上游请求 ID：upstream-request-123"));
    assert!(!body.contains("sk-upstream"));
    assert_eq!(upstream_task.await.unwrap(), "/v1/responses");
    router.stop().await.unwrap();
    let page = crate::route_request_log::query_route_request_logs(
        logs.path(),
        crate::config::RouteRequestLogBackend::Sqlite,
        crate::route_request_log::RouteRequestLogQuery::default(),
    )
    .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(
        page.items[0].upstream_error_summary.as_deref(),
        Some(expected.as_str())
    );
}

#[tokio::test]
async fn responses_compact_restores_route_identity_and_proxies_the_window_unchanged() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let compacted_window = br#"{ "id":"cmp_response", "object":"response.compaction", "output":[{"type":"compaction","encrypted_content":"opaque-window"}] }"#.to_vec();
    let expected_window = compacted_window.clone();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let authorization = incoming_header(&request, "authorization").map(str::to_string);
        let account_id = incoming_header(&request, CHATGPT_ACCOUNT_ID_HEADER).map(str::to_string);
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    compacted_window.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        stream.write_all(&compacted_window).await.unwrap();
        (request.path, authorization, account_id, body)
    });
    let (mut config, provider_id, model) =
        router_config(format!("http://{upstream_address}/v1/responses"));
    config.profiles[0].supports_remote_compaction = true;
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    assert!(endpoint.supports_remote_compaction);

    let response = reqwest::Client::new()
        .post(format!("{}/responses/compact", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .header(CHATGPT_ACCOUNT_ID_HEADER, "acct-must-not-leak")
        .json(&json!({
            "model": model_alias(&provider_id, &model),
            "input": [{"role":"user","content":"full context"}],
            "client_metadata": {
                ROUTE_METADATA_KEY: provider_id,
                "keep": "metadata"
            }
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        serde_json::from_slice::<Value>(&expected_window).unwrap()
    );
    let (path, authorization, account_id, body) = upstream_task.await.unwrap();
    assert_eq!(path, "/v1/responses/compact");
    assert_eq!(authorization.as_deref(), Some("Bearer sk-upstream"));
    assert!(account_id.is_none());
    assert_eq!(body["model"], model);
    assert_eq!(body["client_metadata"]["keep"], "metadata");
    assert!(body["client_metadata"].get(ROUTE_METADATA_KEY).is_none());
    router.stop().await.unwrap();
}

#[tokio::test]
async fn responses_v2_compaction_trigger_passes_through_the_native_route() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let sse = concat!(
        "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"compaction\",\"id\":\"cmp_1\",\"encrypted_content\":\"\"}}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"compaction\",\"id\":\"cmp_1\",\"encrypted_content\":\"opaque-window\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_compact\",\"object\":\"response\",\"output\":[]}}\n\n"
    );

    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}",
                    sse.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        (request.path, body)
    });
    let (mut config, provider_id, model) =
        router_config(format!("http://{upstream_address}/v1/responses"));
    config.profiles[0].supports_remote_compaction = true;
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({
            "model": model_alias(&provider_id, &model),
            "input": [
                {"role":"user","content":"full context"},
                {"type":"compaction_trigger"}
            ],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let events = response.text().await.unwrap();
    assert!(events.contains("response.completed"));
    assert!(events.contains("opaque-window"));
    assert!(!events.contains("response.failed"));
    let (path, body) = upstream_task.await.unwrap();
    assert_eq!(path, "/v1/responses");
    assert_eq!(body["model"], model);
    assert_eq!(body["input"][1]["type"], "compaction_trigger");
    router.stop().await.unwrap();
}

#[tokio::test]
async fn responses_compact_rejects_adapted_routes_before_sending() {
    for protocol in [
        crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
        crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
    ] {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let (mut config, provider_id, model) =
            router_config(format!("http://{}/v1", upstream.local_addr().unwrap()));
        config.profiles[0].upstream_protocol = protocol.into();
        config.profiles[0].normalize();
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        for (path, input, code) in [
            (
                "responses/compact",
                json!("full context"),
                "compaction_unsupported",
            ),
            (
                "responses",
                json!([{"type":"compaction_trigger"}]),
                "compaction_unsupported",
            ),
            (
                "responses",
                json!([{"role":"user","content":"continue"},{"type":"compaction","encrypted_content":"opaque"}]),
                "context_not_portable",
            ),
        ] {
            let response = reqwest::Client::new()
                .post(format!("{}/{path}", endpoint.base_url))
                .bearer_auth(&endpoint.token)
                .json(&json!({"model":model_alias(&provider_id, &model),"input":input}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), 400);
            assert_eq!(
                response.json::<Value>().await.unwrap()["error"]["code"],
                code
            );
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(20), upstream.accept())
                .await
                .is_err()
        );
        router.stop().await.unwrap();
    }
}

#[tokio::test]
async fn context_limit_http_errors_are_structured_on_every_protocol() {
    for protocol in [
        crate::config::UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
        crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
        crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
    ] {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let (mut config, provider_id, model) =
            router_config(format!("http://{}/v1", upstream.local_addr().unwrap()));
        config.profiles[0].upstream_protocol = protocol.into();
        config.profiles[0].normalize();
        let upstream_task = tokio::spawn(async move {
            let (mut socket, _) = upstream.accept().await.unwrap();
            read_http_request(&mut socket).await.unwrap();
            write_json_response(
                &mut socket,
                400,
                &json!({"error":{"code":"context_length_exceeded","message":"context full"}}),
            )
            .await
            .unwrap();
        });
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({"model":model_alias(&provider_id, &model),"input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 400);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            CONTEXT_LENGTH_EXCEEDED
        );
        upstream_task.await.unwrap();
        router.stop().await.unwrap();
    }
}

#[tokio::test]
async fn compaction_timeout_releases_session_and_model_switch_keeps_request_snapshot() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let (mut config, provider_id, model) = router_config(format!(
        "http://{}/v1/responses",
        upstream.local_addr().unwrap()
    ));
    config.profiles[0].supports_remote_compaction = true;
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let (received, request_received) = oneshot::channel();
    let upstream_task = tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut socket).await.unwrap();
        received.send(request).unwrap();
        std::future::pending::<()>().await;
    });
    let url = format!("{}/responses/compact", endpoint.base_url);
    let token = endpoint.token.clone();
    let request_body = json!({"model":model_alias(&provider_id, &model),"input":"full context"});
    let request = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .header("thread-id", "compaction-test")
        .json(&request_body);
    let pending = tokio::spawn(async move { request.send().await.unwrap() });
    let sent = request_received.await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&sent.body).unwrap()["model"],
        model
    );
    // A concurrent request never reaches either upstream, including while the
    // configured route changes. The first request keeps its captured route.
    config.profiles[0].base_url = "http://127.0.0.1:9/v1/responses".into();
    router.update_config(&config);
    let duplicate = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .header("thread-id", "compaction-test")
        .json(&request_body)
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status().as_u16(), 409);
    assert_eq!(
        duplicate.json::<Value>().await.unwrap()["error"]["code"],
        "compaction_in_progress"
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(121)).await;
    tokio::time::resume();
    let timeout = tokio::time::timeout(Duration::from_secs(2), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(timeout.status().as_u16(), 504);
    assert_eq!(
        timeout.json::<Value>().await.unwrap()["error"]["code"],
        "compaction_timeout"
    );
    let retry = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .header("thread-id", "compaction-test")
        .json(&request_body)
        .send()
        .await
        .unwrap();
    assert_ne!(retry.status().as_u16(), 409);
    upstream_task.abort();
    router.stop().await.unwrap();
}

#[tokio::test]
async fn compaction_stream_can_wait_for_headers_within_its_total_budget() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let (mut config, provider_id, model) = router_config(format!(
        "http://{}/v1/responses",
        upstream.local_addr().unwrap()
    ));
    config.profiles[0].supports_remote_compaction = true;
    let (received, wait_received) = oneshot::channel();
    let (release, wait_release) = oneshot::channel();
    let upstream_task = tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        read_http_request(&mut socket).await.unwrap();
        received.send(()).unwrap();
        wait_release.await.unwrap();
        write_json_response(&mut socket, 200, &json!({"status":"completed","output":[{"type":"compaction","encrypted_content":"opaque"}]})).await.unwrap();
    });
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let request = reqwest::Client::new().post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({"model":model_alias(&provider_id, &model),"stream":true,"input":[{"type":"compaction_trigger"}]}));
    let mut pending = tokio::spawn(async move { request.send().await.unwrap() });
    wait_received.await.unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::time::resume();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut pending)
            .await
            .is_err()
    );
    release.send(()).unwrap();
    let response = pending.await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert!(response.text().await.unwrap().contains("opaque"));
    upstream_task.await.unwrap();
    router.stop().await.unwrap();
}

#[tokio::test]
async fn compaction_large_sse_and_context_errors_keep_their_meaning() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let (mut config, provider_id, model) = router_config(format!(
        "http://{}/v1/responses",
        upstream.local_addr().unwrap()
    ));
    config.profiles[0].supports_remote_compaction = true;
    let encrypted = "x".repeat(MAX_UPSTREAM_SSE_BUFFER_BYTES + 1);
    let large = format!(
        "data: {}\n\ndata: {}\n\n",
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"compaction","encrypted_content":encrypted}}),
        json!({"type":"response.completed","response":{"status":"completed","output":[]}})
    );
    let error = json!({"error":{"code":"context_length_exceeded","message":"context full"}});
    let sse_error = format!(
        "data: {}\n\n",
        json!({"type":"response.failed","response":error})
    );
    let upstream_task = tokio::spawn(async move {
        for (content_type, body) in [
            ("text/event-stream", large),
            ("application/json", error.to_string()),
            ("text/event-stream", sse_error),
        ] {
            let (mut socket, _) = upstream.accept().await.unwrap();
            read_http_request(&mut socket).await.unwrap();
            socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    for expected in [200, 400, 400] {
        let response = reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({"model":model_alias(&provider_id, &model),"input":[{"type":"compaction_trigger"}],"stream":true}))
            .send().await.unwrap();
        assert_eq!(response.status().as_u16(), expected);
        if expected == 200 {
            let body = response.text().await.unwrap();
            assert!(body.contains(&encrypted));
            assert!(body.contains("response.completed"));
        } else {
            assert_eq!(
                response.json::<Value>().await.unwrap()["error"]["code"],
                CONTEXT_LENGTH_EXCEEDED
            );
        }
    }
    upstream_task.await.unwrap();
    router.stop().await.unwrap();
}

#[tokio::test]
async fn invalid_compaction_result_does_not_prevent_a_later_valid_request() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let (mut config, provider_id, model) = router_config(format!(
        "http://{}/v1/responses",
        upstream.local_addr().unwrap()
    ));
    config.profiles[0].supports_remote_compaction = true;
    let upstream_task = tokio::spawn(async move {
        for value in [
            json!({"output":[{"type":"message","content":"not a compaction"}]}),
            json!({"output":[{"type":"compaction","encrypted_content":"valid"}]}),
        ] {
            let (mut socket, _) = upstream.accept().await.unwrap();
            read_http_request(&mut socket).await.unwrap();
            write_json_response(&mut socket, 200, &value).await.unwrap();
        }
    });
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let body = json!({"model":model_alias(&provider_id, &model),"input":"full context"});
    for status in [502, 200] {
        let response = reqwest::Client::new()
            .post(format!("{}/responses/compact", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .header("thread-id", "same-history")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
        let value = response.json::<Value>().await.unwrap();
        if status == 502 {
            assert_eq!(value["error"]["code"], "invalid_compaction_response");
        } else {
            assert_eq!(value["output"][0]["encrypted_content"], "valid");
        }
    }
    assert_eq!(body["input"], "full context");
    upstream_task.await.unwrap();
    router.stop().await.unwrap();
}

#[tokio::test]
async fn responses_route_normalizes_tool_schemas_and_passes_web_search_natively() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        let response = json!({
            "id":"resp-search",
            "object":"response",
            "output":[{
                "type":"message",
                "role":"assistant",
                "content":[{
                    "type":"output_text",
                    "text":"search result",
                    "annotations":[]
                }]
            }]
        })
        .to_string();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                    response.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        (request.path, body)
    });
    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].upstream_protocol = crate::config::UPSTREAM_PROTOCOL_OPENAI_RESPONSES.into();
    config.profiles[0].normalize();
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({
            "model":model_alias(&provider_id, &model),
            "input":"search the web",
            "tools":[
                {
                    "type":"web_search",
                    "search_context_size":"high",
                    "filters":{"allowed_domains":["example.com"]},
                    "return_token_budget":2048,
                    "external_web_access":false
                },
                {
                    "type":"function",
                    "name":"automation_update",
                    "parameters":{
                        "anyOf":[
                            {"type":"object","properties":{"mode":{"const":"view"}},"required":["mode"]},
                            {"oneOf":[{}, {"type":"null"}]},
                            {"type":"null"}
                        ]
                    }
                },
                {
                    "type":"function",
                    "name":"read_file",
                    "parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}
                }
            ]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let (path, body) = upstream_task.await.unwrap();
    assert_eq!(path, "/v1/responses");
    assert_eq!(body["model"], model);
    assert_eq!(body["tools"][0]["type"], "web_search");
    let union = &body["tools"][1]["parameters"];
    assert_eq!(union["type"], "object");
    let branches = union["anyOf"].as_array().unwrap();
    assert_eq!(branches.len(), 2);
    assert!(branches.iter().all(|branch| branch["type"] == "object"));
    assert_eq!(branches[1]["oneOf"].as_array().unwrap().len(), 1);
    assert_eq!(branches[1]["oneOf"][0]["type"], "object");
    assert_eq!(
        body["tools"][2]["parameters"],
        json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]})
    );
    assert_eq!(
        body["tools"][0]["filters"]["allowed_domains"][0],
        "example.com"
    );
    assert_eq!(body["tools"][0]["return_token_budget"], 2048);
    assert_eq!(body["tools"][0]["external_web_access"], false);
    router.stop().await.unwrap();
}

#[tokio::test]
async fn chat_completions_route_uses_its_path_key_and_returns_responses_sse() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let authorization = incoming_header(&request, "authorization").map(str::to_string);
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        let sse = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"inspect \"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"the file\"}},{\"index\":1,\"delta\":{\"reasoning_content\":\"ignored\"}}]}\n\n",
            "data: {\"id\":\"chatcmpl-stream\",\"created\":123,\"model\":\"provider-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hello \",\"tool_calls\":null},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-stream\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-stream\",\"type\":\"function\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{\\\"q\\\":\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-stream\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"1}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"id\":\"chatcmpl-stream\",\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":3,\"total_tokens\":7}}\n\n",
            "data: [DONE]\n\n"
        );
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}",
                    sse.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        (request.path, authorization, body)
    });
    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].upstream_protocol =
        crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
    config.profiles[0].normalize();
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({
            "model": model_alias(&provider_id, &model),
            "input": "hello",
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("text/event-stream")
    );
    let events = response.text().await.unwrap();
    assert!(events.contains("response.output_text.delta"));
    assert!(events.contains("response.function_call_arguments.delta"));
    assert!(events.contains("\"call_id\":\"call-stream\""));
    assert!(events.contains("\"input_tokens\":4"));
    assert!(events.contains("response.completed"));

    let parsed = events
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let output = parsed
        .iter()
        .filter(|event| event["type"] == "response.output_item.done")
        .map(|event| event["item"].clone())
        .collect::<Vec<_>>();
    let completed = parsed
        .iter()
        .find(|event| event["type"] == "response.completed")
        .unwrap();
    assert_eq!(completed["response"]["output"], json!(output));
    assert_eq!(output[0]["content"][0]["text"], "inspect the file");
    assert_eq!(completed["response"]["output_text"], "hello ");
    let mut next_input = output;
    next_input.push(json!({"type":"function_call_output","call_id":"call-stream","output":"done"}));
    let next = responses_to_chat_completions_body(&json!({
        "model":"provider-model", "input":next_input,
    }))
    .unwrap();
    assert_eq!(next["messages"][0]["reasoning_content"], "inspect the file");
    assert_eq!(next["messages"][0]["tool_calls"][0]["id"], "call-stream");
    assert_eq!(next["messages"][1]["tool_call_id"], "call-stream");

    let (path, authorization, body) = upstream_task.await.unwrap();
    assert_eq!(path, "/v1/chat/completions");
    assert_eq!(authorization.as_deref(), Some("Bearer sk-upstream"));
    assert_eq!(body["model"], "provider-model");
    assert_eq!(body["messages"][0]["content"], "hello");
    assert_eq!(body["stream_options"]["include_usage"], true);
    router.stop().await.unwrap();
}

#[tokio::test]
async fn chat_route_maps_web_search_for_provider_scoped_model_without_model_name_gate() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        write_json_response(
            &mut stream,
            200,
            &json!({
                "id":"chatcmpl-search",
                "created":123,
                "model":body["model"],
                "choices":[{
                    "message":{"role":"assistant","content":"search result"},
                    "finish_reason":"stop"
                }],
                "usage":{"prompt_tokens":4,"completion_tokens":2,"total_tokens":6}
            }),
        )
        .await
        .unwrap();
        (request.path, body)
    });
    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].upstream_protocol =
        crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
    config.profiles[0].normalize();
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({
            "model":model_alias(&provider_id, &model),
            "input":"search the web",
            "tools":[{
                "type":"web_search",
                "search_context_size":"high"
            }],
            "tool_choice":{"type":"web_search"}
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap()["output_text"],
        "search result"
    );
    let (path, body) = upstream_task.await.unwrap();
    assert_eq!(path, "/v1/chat/completions");
    assert_eq!(body["model"], "provider-model");
    assert_eq!(
        body["web_search_options"],
        json!({"search_context_size":"high"})
    );
    assert!(body.get("tools").is_none());
    router.stop().await.unwrap();
}

#[tokio::test]
async fn chat_route_drops_ambient_auto_web_search_before_contacting_upstream() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        write_json_response(
            &mut stream,
            200,
            &json!({
                "id":"chatcmpl-ambient-search",
                "created":123,
                "model":body["model"],
                "choices":[{
                    "message":{"role":"assistant","content":"normal answer"},
                    "finish_reason":"stop"
                }],
                "usage":{"prompt_tokens":2,"completion_tokens":2,"total_tokens":4}
            }),
        )
        .await
        .unwrap();
        (request.path, body)
    });
    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].upstream_protocol =
        crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
    config.profiles[0].normalize();
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({
            "model":model_alias(&provider_id, &model),
            "input":"1",
            "tools":[{"type":"web_search"}],
            "tool_choice":"auto"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap()["output_text"],
        "normal answer"
    );
    let (path, body) = upstream_task.await.unwrap();
    assert_eq!(path, "/v1/chat/completions");
    assert_eq!(body["model"], "provider-model");
    assert!(body.get("web_search_options").is_none());
    assert!(body.get("tools").is_none());
    assert!(body.get("tool_choice").is_none());
    router.stop().await.unwrap();
}

#[tokio::test]
async fn responses_route_passes_web_search_through_without_chat_conversion() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        write_json_response(
            &mut stream,
            200,
            &json!({
                "id":"resp-search",
                "object":"response",
                "model":body["model"],
                "output_text":"native search"
            }),
        )
        .await
        .unwrap();
        (request.path, body)
    });
    let (config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({
            "model":model_alias(&provider_id, &model),
            "input":"search the web",
            "tools":[{
                "type":"web_search",
                "search_context_size":"high"
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap()["output_text"],
        "native search"
    );
    let (path, body) = upstream_task.await.unwrap();
    assert_eq!(path, "/v1/responses");
    assert_eq!(body["model"], "provider-model");
    assert_eq!(body["tools"][0]["type"], "web_search");
    assert!(body.get("web_search_options").is_none());
    router.stop().await.unwrap();
}

#[tokio::test]
async fn native_responses_route_rejects_unrecoverable_synthetic_history_and_preserves_real_ids() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let mut forwarded = Vec::new();
        for _ in 0..1 {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            write_json_response(
                &mut stream,
                200,
                &json!({
                    "id":"resp-upstream",
                    "object":"response",
                    "model":body["model"],
                    "output_text":"ok"
                }),
            )
            .await
            .unwrap();
            forwarded.push(body);
        }
        forwarded
    });
    let (config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let client = reqwest::Client::new();
    let alias = model_alias(&provider_id, &model);

    for previous_response_id in ["resp_codey_wrapped", "resp_upstream"] {
        let response = client
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({
                "model": alias,
                "input": "continue",
                "previous_response_id": previous_response_id,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status().as_u16(),
            if is_codey_synthetic_response_id(previous_response_id) {
                400
            } else {
                200
            }
        );
    }

    let forwarded = upstream_task.await.unwrap();
    assert_eq!(forwarded.len(), 1);
    assert_eq!(forwarded[0]["model"], "provider-model");
    assert_eq!(forwarded[0]["previous_response_id"], "resp_upstream");
    router.stop().await.unwrap();
}

#[tokio::test]
async fn model_switch_from_chat_to_native_expands_synthetic_history_in_order() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let (mut config, provider_id, model) =
        router_config(format!("http://{}/v1", upstream.local_addr().unwrap()));
    let mut native = config.profiles[0].clone();
    native.id = "route-native".into();
    native.source_provider_id = Some("route-native".into());
    native.normalize();
    config
        .selected_models_by_provider
        .insert("route-native".into(), vec![model.clone()]);
    config.profiles[0].upstream_protocol =
        crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
    config.profiles[0].normalize();
    config.profiles.push(native);
    let upstream_task = tokio::spawn(async move {
        let (mut first, _) = upstream.accept().await.unwrap();
        read_http_request(&mut first).await.unwrap();
        let sse = concat!(
            "data: {\"id\":\"chat-first\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"private bridge reasoning\"}}]}\n\n",
            "data: {\"id\":\"chat-first\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"remembered answer\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        first.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}", sse.len()).as_bytes()).await.unwrap();
        let (mut second, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut second).await.unwrap();
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        write_json_response(
            &mut second,
            200,
            &json!({"id":"resp-native","object":"response","status":"completed","output":[]}),
        )
        .await
        .unwrap();
        body
    });
    let router = LocalRouter::start(&config).await.unwrap();
    let mut socket = connect_router_websocket(&router.endpoint()).await;
    let mut previous: Option<String> = None;
    for (provider, input) in [
        (provider_id.as_str(), "original task"),
        ("route-native", "continue"),
    ] {
        socket.send(WebSocketMessage::Text(json!({"type":"response.create","model":model_alias(provider, &model),"input":input,"previous_response_id":previous}).to_string().into())).await.unwrap();
        previous = Some(
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let message = socket.next().await.unwrap().unwrap();
                    if let WebSocketMessage::Text(text) = message {
                        let event: Value = serde_json::from_str(&text).unwrap();
                        assert_ne!(event["type"], "response.failed", "{event}");
                        if event["type"] == "response.completed" {
                            break event["response"]["id"].as_str().unwrap().to_string();
                        }
                    }
                }
            })
            .await
            .unwrap(),
        );
    }
    let sent = upstream_task.await.unwrap();
    assert!(sent.get("previous_response_id").is_none());
    assert_eq!(sent["input"][0], "original task");
    assert_eq!(sent["input"][1]["role"], "assistant");
    assert_eq!(sent["input"][1]["content"][0]["text"], "remembered answer");
    assert_eq!(sent["input"][2], "continue");
    assert!(
        sent["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| { item.get("type").and_then(Value::as_str) != Some("reasoning") })
    );
    socket.close(None).await.unwrap();
    router.stop().await.unwrap();
}

#[tokio::test]
async fn native_route_switch_drops_previous_provider_reasoning_state() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let (mut config, provider_a, model) = router_config(format!("http://{upstream_address}/v1"));
    let mut route_b = config.profiles[0].clone();
    route_b.id = "route-native-b".into();
    route_b.name = "Native B".into();
    route_b.normalize();
    let provider_b = route_b.provider_id().to_string();
    config.profiles.push(route_b);
    config
        .selected_models_by_provider
        .insert(provider_b.clone(), vec![model.clone()]);
    let upstream_task = tokio::spawn(async move {
        let mut bodies = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            write_json_response(
                &mut stream,
                200,
                &json!({"id":"resp-native","object":"response","model":body["model"],"output":[]}),
            )
            .await
            .unwrap();
            bodies.push(body);
        }
        bodies
    });
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let client = reqwest::Client::new();

    let first = client
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .header("thread-id", "native-provider-switch")
        .json(&json!({"model":model_alias(&provider_a, &model),"input":"first"}))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::OK);

    let second = client
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .header("thread-id", "native-provider-switch")
        .json(&json!({
            "model":model_alias(&provider_b, &model),
            "input":[
                {"role":"user","content":"original task"},
                {"type":"reasoning","id":"rs_provider_a","encrypted_content":"opaque-provider-a"},
                {"role":"assistant","content":[{"type":"output_text","text":"visible answer"}]},
                {"role":"user","content":"continue"}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::OK);

    let bodies = upstream_task.await.unwrap();
    assert_eq!(bodies[1]["input"].as_array().unwrap().len(), 3);
    assert!(
        bodies[1]["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| { item.get("type").and_then(Value::as_str) != Some("reasoning") })
    );
    assert_eq!(bodies[1]["input"][1]["role"], "assistant");
    assert_eq!(
        bodies[1]["input"][1]["content"][0]["text"],
        "visible answer"
    );
    router.stop().await.unwrap();
}

#[tokio::test]
async fn native_route_switch_rejects_provider_bound_history_references_locally() {
    for opaque_item in [
        json!({"type":"compaction","id":"cmp_provider_a","encrypted_content":"opaque-provider-a"}),
        json!({"type":"item_reference","id":"item_provider_a"}),
    ] {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let (mut config, provider_a, model) =
            router_config(format!("http://{upstream_address}/v1"));
        let mut route_b = config.profiles[0].clone();
        route_b.id = "route-native-b".into();
        route_b.name = "Native B".into();
        route_b.normalize();
        let provider_b = route_b.provider_id().to_string();
        config.profiles.push(route_b);
        config
            .selected_models_by_provider
            .insert(provider_b.clone(), vec![model.clone()]);
        let upstream_task = tokio::spawn(async move {
            let (mut first, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut first).await.unwrap();
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            write_json_response(
                &mut first,
                200,
                &json!({"id":"resp-native","object":"response","model":body["model"],"output":[]}),
            )
            .await
            .unwrap();
            if let Ok(Ok((mut second, _))) =
                tokio::time::timeout(Duration::from_millis(250), upstream.accept()).await
            {
                let _request = read_http_request(&mut second).await.unwrap();
                write_json_response(
                    &mut second,
                    200,
                    &json!({"id":"unexpected-forward","object":"response","output":[]}),
                )
                .await
                .unwrap();
                return true;
            }
            false
        });
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let client = reqwest::Client::new();

        let first = client
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .header("thread-id", "native-provider-reference-switch")
            .json(&json!({"model":model_alias(&provider_a, &model),"input":"first"}))
            .send()
            .await
            .unwrap();
        assert_eq!(first.status(), reqwest::StatusCode::OK);

        let second = client
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .header("thread-id", "native-provider-reference-switch")
            .json(&json!({
                "model":model_alias(&provider_b, &model),
                "input":[{"role":"user","content":"continue"},opaque_item.clone()]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(second.status().as_u16(), 400);
        assert_eq!(
            second.json::<Value>().await.unwrap()["error"]["code"],
            "context_not_portable"
        );
        let retry = client
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .header("thread-id", "native-provider-reference-switch")
            .json(&json!({
                "model":model_alias(&provider_b, &model),
                "input":[{"role":"user","content":"continue"},opaque_item]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(retry.status().as_u16(), 400);
        assert_eq!(
            retry.json::<Value>().await.unwrap()["error"]["code"],
            "context_not_portable"
        );
        assert!(!upstream_task.await.unwrap());
        router.stop().await.unwrap();
    }
}

#[tokio::test]
async fn non_stream_chat_adapter_requests_upstream_stream_and_returns_json() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let (request_body_sent, request_body_received) = oneshot::channel();
    let (first_event_sent, first_event_observed) = oneshot::channel();
    let (release_upstream, wait_for_release) = oneshot::channel();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        request_body_sent.send(body.clone()).unwrap();
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        write_test_http_chunk(
            &mut stream,
            "data: {\"id\":\"chatcmpl-internal-stream\",\"model\":\"provider-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"first-token\"},\"finish_reason\":null}]}\n\n",
        )
        .await;
        first_event_sent.send(()).unwrap();
        wait_for_release.await.unwrap();
        write_test_http_chunk(
            &mut stream,
            "data: {\"id\":\"chatcmpl-internal-stream\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" done\"},\"finish_reason\":\"stop\"}]}\n\ndata: {\"id\":\"chatcmpl-internal-stream\",\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":3,\"total_tokens\":7}}\n\ndata: [DONE]\n\n",
        )
        .await;
        stream.write_all(b"0\r\n\r\n").await.unwrap();
        body
    });
    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].upstream_protocol =
        crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
    config.profiles[0].normalize();
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();

    let mut client_task = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({
                "model": model_alias(&provider_id, &model),
                "input": "hello"
            }))
            .send()
            .await
            .unwrap()
    });
    let upstream_body = request_body_received.await.unwrap();
    assert_eq!(upstream_body["stream"], true);
    assert_eq!(upstream_body["stream_options"]["include_usage"], true);
    first_event_observed.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut client_task)
            .await
            .is_err(),
        "non-stream downstream response must wait for the complete upstream stream"
    );

    release_upstream.send(()).unwrap();
    let response = tokio::time::timeout(Duration::from_secs(2), client_task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let response = response.json::<Value>().await.unwrap();
    assert_eq!(response["output_text"], "first-token done");
    assert_eq!(response["usage"]["total_tokens"], 7);
    assert_eq!(upstream_task.await.unwrap()["stream"], true);
    router.stop().await.unwrap();
}

#[tokio::test]
async fn chat_adapter_streams_mislabeled_sse_before_upstream_completes() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let (first_event_sent, first_event_observed) = oneshot::channel();
    let (release_upstream, wait_for_release) = oneshot::channel();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let _request = read_http_request(&mut stream).await.unwrap();
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        write_test_http_chunk(
            &mut stream,
            "data: {\"id\":\"chatcmpl-progressive\",\"model\":\"provider-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"first-token\"},\"finish_reason\":null}]}\n\n",
        )
        .await;
        first_event_sent.send(()).unwrap();
        wait_for_release.await.unwrap();
        write_test_http_chunk(
            &mut stream,
            "data: {\"id\":\"chatcmpl-progressive\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        )
        .await;
        stream.write_all(b"0\r\n\r\n").await.unwrap();
    });
    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].upstream_protocol =
        crate::config::UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
    config.profiles[0].normalize();
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();

    let mut response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({
            "model":model_alias(&provider_id, &model),
            "input":"hello",
            "stream":true,
        }))
        .send()
        .await
        .unwrap();
    first_event_observed.await.unwrap();
    let first_payload = tokio::time::timeout(Duration::from_secs(2), async {
        let mut payload = Vec::new();
        loop {
            let chunk = response.chunk().await.unwrap().unwrap();
            payload.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&payload).contains("first-token") {
                break payload;
            }
        }
    })
    .await
    .unwrap();
    let first_payload = String::from_utf8_lossy(&first_payload);
    assert!(first_payload.contains("response.output_text.delta"));
    assert!(!first_payload.contains("response.completed"));

    release_upstream.send(()).unwrap();
    let mut remaining = Vec::new();
    while let Some(chunk) = response.chunk().await.unwrap() {
        remaining.extend_from_slice(&chunk);
    }
    assert!(String::from_utf8_lossy(&remaining).contains("response.completed"));

    upstream_task.await.unwrap();
    router.stop().await.unwrap();
}

#[tokio::test]
async fn anthropic_route_uses_messages_key_headers_and_returns_responses_sse() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let authorization = incoming_header(&request, "authorization").map(str::to_string);
        let api_key = incoming_header(&request, "x-api-key").map(str::to_string);
        let version = incoming_header(&request, "anthropic-version").map(str::to_string);
        let account_id = incoming_header(&request, "chatgpt-account-id").map(str::to_string);
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        let upstream_tool_name = body["tools"][0]["name"].as_str().unwrap();
        assert!(upstream_tool_name.starts_with(NAMESPACE_UPSTREAM_TOOL_PREFIX));
        let sse = [
            (
                "message_start",
                json!({
                    "type":"message_start",
                    "message":{
                        "id":"msg-stream",
                        "type":"message",
                        "role":"assistant",
                        "model":"claude-sonnet-test",
                        "content":[],
                        "usage":{"input_tokens":4}
                    }
                }),
            ),
            (
                "content_block_start",
                json!({
                    "type":"content_block_start",
                    "index":0,
                    "content_block":{"type":"text","text":""}
                }),
            ),
            (
                "content_block_delta",
                json!({
                    "type":"content_block_delta",
                    "index":0,
                    "delta":{"type":"text_delta","text":"hello"}
                }),
            ),
            (
                "content_block_start",
                json!({
                    "type":"content_block_start",
                    "index":1,
                    "content_block":{
                        "type":"tool_use",
                        "id":"call-read",
                        "name":upstream_tool_name,
                        "input":{"path":"a.txt"}
                    }
                }),
            ),
            (
                "message_delta",
                json!({
                    "type":"message_delta",
                    "delta":{"stop_reason":"tool_use"},
                    "usage":{"output_tokens":2}
                }),
            ),
            ("message_stop", json!({"type":"message_stop"})),
        ]
        .into_iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect::<String>();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}",
                    sse.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        (
            request.path,
            authorization,
            api_key,
            version,
            account_id,
            body,
        )
    });
    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].upstream_protocol =
        crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES.into();
    config.profiles[0].normalize();
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .header("chatgpt-account-id", "must-not-leak")
        .json(&json!({
            "model":model_alias(&provider_id, &model),
            "input":"hello",
            "stream":true,
            "tools":[{
                "type":"namespace",
                "name":"fs",
                "tools":[{
                    "type":"function",
                    "name":"read",
                    "description":"read a file",
                    "parameters":{"type":"object","properties":{"path":{"type":"string"}}}
                }]
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let events = response.text().await.unwrap();
    assert!(events.contains("response.output_text.delta"));
    assert!(events.contains("\"delta\":\"hello\""));
    assert!(events.contains("\"call_id\":\"call-read\""));
    assert!(events.contains("\"namespace\":\"fs\""));
    assert!(events.contains("\"name\":\"read\""));
    assert!(events.contains("\"input_tokens\":4"));
    assert!(events.contains("response.completed"));

    let (path, authorization, api_key, version, account_id, body) = upstream_task.await.unwrap();
    assert_eq!(path, "/v1/messages");
    assert_eq!(authorization, None);
    assert_eq!(api_key.as_deref(), Some("sk-upstream"));
    assert_eq!(version.as_deref(), Some("2023-06-01"));
    assert_eq!(account_id, None);
    assert_eq!(body["model"], "provider-model");
    assert_eq!(body["messages"][0]["content"][0]["text"], "hello");
    assert!(
        body["tools"][0]["name"]
            .as_str()
            .unwrap()
            .starts_with(NAMESPACE_UPSTREAM_TOOL_PREFIX)
    );
    router.stop().await.unwrap();
}

#[tokio::test]
async fn non_stream_anthropic_adapter_requests_upstream_stream_and_returns_json() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let (request_body_sent, request_body_received) = oneshot::channel();
    let (first_event_sent, first_event_observed) = oneshot::channel();
    let (release_upstream, wait_for_release) = oneshot::channel();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        request_body_sent.send(body.clone()).unwrap();
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        write_test_http_chunk(
            &mut stream,
            concat!(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-internal-stream\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"provider-model\",\"content\":[],\"usage\":{\"input_tokens\":4}}}\n\n",
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"first-token\"}}\n\n"
            ),
        )
        .await;
        first_event_sent.send(()).unwrap();
        wait_for_release.await.unwrap();
        write_test_http_chunk(
            &mut stream,
            concat!(
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" done\"}}\n\n",
                "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
            ),
        )
        .await;
        stream.write_all(b"0\r\n\r\n").await.unwrap();
        body
    });
    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].upstream_protocol =
        crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES.into();
    config.profiles[0].normalize();
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();

    let mut client_task = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({
                "model": model_alias(&provider_id, &model),
                "input": "hello"
            }))
            .send()
            .await
            .unwrap()
    });
    let upstream_body = request_body_received.await.unwrap();
    assert_eq!(upstream_body["stream"], true);
    first_event_observed.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut client_task)
            .await
            .is_err(),
        "non-stream downstream response must wait for the complete upstream stream"
    );

    release_upstream.send(()).unwrap();
    let response = tokio::time::timeout(Duration::from_secs(2), client_task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let response = response.json::<Value>().await.unwrap();
    assert_eq!(response["output_text"], "first-token done");
    assert_eq!(response["usage"]["total_tokens"], 7);
    assert_eq!(upstream_task.await.unwrap()["stream"], true);
    router.stop().await.unwrap();
}

#[tokio::test]
async fn anthropic_route_bridges_apply_patch_custom_tool_and_stream_events() {
    const RAW_PATCH: &str =
        "*** Begin Patch\n*** Update File: README.md\n@@\n-old\n+new\n*** End Patch";

    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        let upstream_tool_name = body["tools"][0]["name"].as_str().unwrap().to_string();
        assert!(upstream_tool_name.starts_with(CUSTOM_UPSTREAM_TOOL_PREFIX));
        let wrapped_input = json!({"input":RAW_PATCH}).to_string();
        let sse = [
            (
                "message_start",
                json!({
                    "type":"message_start",
                    "message":{
                        "id":"msg-custom-stream",
                        "type":"message",
                        "role":"assistant",
                        "model":"claude-sonnet-test",
                        "content":[],
                        "usage":{"input_tokens":5}
                    }
                }),
            ),
            (
                "content_block_start",
                json!({
                    "type":"content_block_start",
                    "index":0,
                    "content_block":{
                        "type":"tool_use",
                        "id":"call-apply-patch",
                        "name":upstream_tool_name,
                        "input":{}
                    }
                }),
            ),
            (
                "content_block_delta",
                json!({
                    "type":"content_block_delta",
                    "index":0,
                    "delta":{
                        "type":"input_json_delta",
                        "partial_json":wrapped_input
                    }
                }),
            ),
            (
                "message_delta",
                json!({
                    "type":"message_delta",
                    "delta":{"stop_reason":"tool_use"},
                    "usage":{"output_tokens":3}
                }),
            ),
            ("message_stop", json!({"type":"message_stop"})),
        ]
        .into_iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect::<String>();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{sse}",
                    sse.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        body
    });
    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].upstream_protocol =
        crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES.into();
    config.profiles[0].normalize();
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({
            "model":model_alias(&provider_id, &model),
            "input":"apply the patch",
            "stream":true,
            "tools":[{
                "type":"custom",
                "name":"apply_patch",
                "description":"Apply a patch to the workspace",
                "format":{
                    "type":"grammar",
                    "syntax":"lark",
                    "definition":"start: /[\\s\\S]+/"
                }
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let events = response.text().await.unwrap();
    assert!(events.contains("response.custom_tool_call_input.delta"));
    assert!(events.contains("response.custom_tool_call_input.done"));
    assert!(events.contains("\"type\":\"custom_tool_call\""));
    assert!(events.contains("\"name\":\"apply_patch\""));
    assert!(events.contains("*** Begin Patch"));
    assert!(!events.contains("response.function_call_arguments"));
    assert!(!events.contains(CUSTOM_UPSTREAM_TOOL_PREFIX));
    assert!(events.contains("response.completed"));

    let body = upstream_task.await.unwrap();
    assert_eq!(
        body["tools"][0]["input_schema"]["required"],
        json!(["input"])
    );
    assert_eq!(
        body["tools"][0]["input_schema"]["additionalProperties"],
        false
    );
    assert!(
        body["tools"][0]["description"]
            .as_str()
            .unwrap()
            .contains("\"syntax\":\"lark\"")
    );
    router.stop().await.unwrap();
}

#[tokio::test]
async fn anthropic_adapter_streams_mislabeled_sse_before_upstream_completes() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let (first_event_sent, first_event_observed) = oneshot::channel();
    let (release_upstream, wait_for_release) = oneshot::channel();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let _request = read_http_request(&mut stream).await.unwrap();
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        write_test_http_chunk(
            &mut stream,
            concat!(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-progressive\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"provider-model\",\"content\":[],\"usage\":{\"input_tokens\":1}}}\n\n",
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"first-token\"}}\n\n"
            ),
        )
        .await;
        first_event_sent.send(()).unwrap();
        wait_for_release.await.unwrap();
        write_test_http_chunk(
            &mut stream,
            concat!(
                "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
            ),
        )
        .await;
        stream.write_all(b"0\r\n\r\n").await.unwrap();
    });
    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].upstream_protocol =
        crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES.into();
    config.profiles[0].normalize();
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();

    let mut response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({
            "model":model_alias(&provider_id, &model),
            "input":"hello",
            "stream":true,
        }))
        .send()
        .await
        .unwrap();
    first_event_observed.await.unwrap();
    let first_payload = tokio::time::timeout(Duration::from_secs(2), async {
        let mut payload = Vec::new();
        loop {
            let chunk = response.chunk().await.unwrap().unwrap();
            payload.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&payload).contains("first-token") {
                break payload;
            }
        }
    })
    .await
    .unwrap();
    let first_payload = String::from_utf8_lossy(&first_payload);
    assert!(first_payload.contains("response.output_text.delta"));
    assert!(!first_payload.contains("response.completed"));

    release_upstream.send(()).unwrap();
    let mut remaining = Vec::new();
    while let Some(chunk) = response.chunk().await.unwrap() {
        remaining.extend_from_slice(&chunk);
    }
    assert!(String::from_utf8_lossy(&remaining).contains("response.completed"));

    upstream_task.await.unwrap();
    router.stop().await.unwrap();
}

#[test]
fn model_aliases_do_not_collapse_distinct_provider_ids() {
    assert_ne!(
        model_alias("team/relay", "shared-model"),
        model_alias("team_relay", "shared-model")
    );
    assert_eq!(
        model_alias("team/relay", "shared-model"),
        "team%2Frelay/shared-model"
    );
}

#[tokio::test]
async fn historical_alias_http_requests_survive_hot_route_removal() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let mut models = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            models.push(body["model"].clone());
            write_json_response(&mut stream, 200, &json!({"model":body["model"]}))
                .await
                .unwrap();
        }
        models
    });
    let (mut config, _, model) = router_config(format!("http://{address}/v1"));
    let mut old = config.profiles[0].clone();
    old.id = "retired/route".into();
    config.profiles.push(old);
    config
        .selected_models_by_provider
        .insert("retired/route".into(), vec![model.clone()]);
    config = config.normalize();
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let client = reqwest::Client::new();
    for remove in [false, true] {
        if remove {
            config
                .profiles
                .retain(|profile| profile.id != "retired/route");
            config.selected_models_by_provider.remove("retired/route");
            router.update_config(&config);
        }
        let response = client
            .post(format!("{}/responses", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .header("thread-id", "historical-thread")
            .json(&json!({"model":model_alias("retired/route", &model), "input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.json::<Value>().await.unwrap()["model"], model);
    }
    assert_eq!(
        upstream_task.await.unwrap(),
        vec![json!(model), json!(model)]
    );
    router.stop().await.unwrap();
}

#[tokio::test]
async fn router_rewrites_alias_and_keeps_upstream_credentials_private() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let authorization = request
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.clone());
        let user_agent = incoming_header(&request, "user-agent").map(str::to_string);
        let originator = incoming_header(&request, "originator").map(str::to_string);
        let codex_window_id = incoming_header(&request, "x-codex-window-id").map(str::to_string);
        let account_id = incoming_header(&request, "chatgpt-account-id").map(str::to_string);
        let router_token = incoming_header(&request, ROUTER_AUTH_HEADER).map(str::to_string);
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        write_json_response(
            &mut stream,
            200,
            &json!({"object":"response","model":body["model"]}),
        )
        .await
        .unwrap();
        (
            request.path,
            authorization,
            user_agent,
            originator,
            codex_window_id,
            account_id,
            router_token,
            body,
        )
    });
    let (config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let alias = model_alias(&provider_id, &model);

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .header("user-agent", "codex_cli_rs/0.114.0")
        .header("originator", "codex_cli_rs")
        .header("x-codex-window-id", "window-123")
        .header("chatgpt-account-id", "must-not-leak")
        .json(&json!({"model":alias,"input":"hello","stream":true}))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.json::<Value>().await.unwrap()["model"], model);
    let (
        path,
        authorization,
        user_agent,
        originator,
        codex_window_id,
        account_id,
        router_token,
        body,
    ) = upstream_task.await.unwrap();
    assert_eq!(path, "/v1/responses");
    assert_eq!(authorization.as_deref(), Some("Bearer sk-upstream"));
    assert_eq!(user_agent.as_deref(), Some("codex_cli_rs/0.114.0"));
    assert_eq!(originator.as_deref(), Some("codex_cli_rs"));
    assert_eq!(codex_window_id.as_deref(), Some("window-123"));
    assert_eq!(account_id, None);
    assert_eq!(router_token, None);
    assert_eq!(body["model"], "provider-model");
    assert!(!body.to_string().contains(&endpoint.token));
    router.stop().await.unwrap();
}

#[tokio::test]
async fn route_header_overrides_replace_and_remove_forwarded_headers() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let user_agent = incoming_header(&request, "user-agent").map(str::to_string);
        let originator = incoming_header(&request, "originator").map(str::to_string);
        let request_id = incoming_header(&request, "x-codey-request-id").map(str::to_string);
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        write_json_response(
            &mut stream,
            200,
            &json!({"object":"response","model":body["model"]}),
        )
        .await
        .unwrap();
        (user_agent, originator, request_id)
    });
    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    let headers = &mut config.profiles[0].model_request_headers;
    headers.insert("user-agent".into(), "Codex Desktop/0.153.4".into());
    // 空值（前端的 null）表示从上游请求中移除该请求头，而不是发送空值头。
    // x-codey-request-id 不配置覆盖：Codey 内部请求 ID 默认就不得发往上游。
    headers.insert("originator".into(), String::new());
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let alias = model_alias(&provider_id, &model);

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .header("user-agent", "codex_cli_rs/0.114.0")
        .header("originator", "codex_cli_rs")
        .json(&json!({"model":alias,"input":"hello"}))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let (user_agent, originator, request_id) = upstream_task.await.unwrap();
    assert_eq!(user_agent.as_deref(), Some("Codex Desktop/0.153.4"));
    assert_eq!(originator, None);
    assert_eq!(request_id, None);
    router.stop().await.unwrap();
}

#[tokio::test]
async fn routing_hint_reaches_upstream_with_the_restored_model_name() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let routing_hint = incoming_header(&request, "x-codex-routing-hint").map(str::to_string);
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        write_json_response(
            &mut stream,
            200,
            &json!({"object":"response","model":body["model"]}),
        )
        .await
        .unwrap();
        (routing_hint, body["model"].as_str().unwrap().to_string())
    });
    let (config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let alias = model_alias(&provider_id, &model);

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .header(
            "x-codex-routing-hint",
            format!("model={alias};tier=default"),
        )
        .json(&json!({"model":alias,"input":"hello"}))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let (routing_hint, upstream_model) = upstream_task.await.unwrap();
    assert_eq!(upstream_model, model);
    // 路由提示必须和请求体里已还原的上游模型名一致，不能把线路别名泄给上游。
    assert_eq!(
        routing_hint.as_deref(),
        Some("model=provider-model;tier=default")
    );
    router.stop().await.unwrap();
}

#[tokio::test]
async fn route_upstream_proxy_carries_requests_through_the_proxy() {
    let proxy = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_address = proxy.local_addr().unwrap();
    let proxy_task = tokio::spawn(async move {
        let (mut stream, _) = proxy.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        write_json_response(
            &mut stream,
            200,
            &json!({"object":"response","model":body["model"]}),
        )
        .await
        .unwrap();
        request.path
    });
    // 上游域名不可解析：请求只有经过代理（绝对形式请求行）才能成功。
    let (mut config, provider_id, model) =
        router_config("http://codey-proxy-test.invalid/v1".into());
    config.profiles[0].upstream_proxy = format!("http://{proxy_address}");
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({"model":model_alias(&provider_id, &model),"input":"hello"}))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.json::<Value>().await.unwrap()["model"], model);
    let proxied_path = proxy_task.await.unwrap();
    assert_eq!(proxied_path, "http://codey-proxy-test.invalid/v1/responses");
    // 配置了上游代理的线路不使用上游 WebSocket，即使线路声明支持。
    config.profiles[0].supports_websockets = true;
    let snapshot = RouterSnapshot::from_config(&config);
    assert!(
        snapshot
            .routes
            .values()
            .all(|route| !route.supports_websockets)
    );
    config.profiles[0].upstream_proxy = String::new();
    let snapshot = RouterSnapshot::from_config(&config);
    assert!(
        snapshot
            .routes
            .values()
            .any(|route| route.supports_websockets)
    );
    router.stop().await.unwrap();
}

#[tokio::test]
async fn upstream_response_headers_reach_the_downstream_client() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        let response = json!({"object":"response","model":body["model"]}).to_string();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nx-codex-turn-state: sticky-token\r\nx-models-etag: etag-1\r\nx-codey-request-id: forged-by-upstream\r\nconnection: close\r\n\r\n{response}",
                    response.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let (config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({"model":model_alias(&provider_id, &model),"input":"hello"}))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    // Codex 依赖粘性路由令牌等端到端响应头；传输层头由本地路由自行管理，
    // x-codey-request-id 以本地路由生成的值为准，不接受上游伪造。
    assert_eq!(
        response
            .headers()
            .get("x-codex-turn-state")
            .and_then(|value| value.to_str().ok()),
        Some("sticky-token")
    );
    assert_eq!(
        response
            .headers()
            .get("x-models-etag")
            .and_then(|value| value.to_str().ok()),
        Some("etag-1")
    );
    let request_id = response
        .headers()
        .get("x-codey-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap();
    assert_ne!(request_id, "forged-by-upstream");
    assert_eq!(
        response
            .headers()
            .get_all(reqwest::header::CONTENT_TYPE)
            .iter()
            .count(),
        1
    );
    assert_eq!(response.json::<Value>().await.unwrap()["model"], model);
    upstream_task.await.unwrap();
    router.stop().await.unwrap();
}

#[tokio::test]
async fn raw_model_metadata_selects_an_ambiguous_route_and_binds_the_thread() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let mut bodies = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let request = read_http_request(&mut stream).await.unwrap();
            let body = serde_json::from_slice::<Value>(&request.body).unwrap();
            assert_eq!(body["model"], "provider-model");
            write_json_response(
                &mut stream,
                200,
                &json!({"object":"response","model":body["model"]}),
            )
            .await
            .unwrap();
            bodies.push(body);
        }
        bodies
    });
    let (mut config, _, model) = router_config("http://127.0.0.1:9/v1".into());
    let mut route_b = config.profiles[0].clone();
    route_b.id = "route-b".into();
    route_b.name = "Relay B".into();
    route_b.base_url = format!("http://{upstream_address}/v1");
    route_b.api_key = "sk-route-b".into();
    route_b.normalize();
    let provider_b = route_b.provider_id().to_string();
    config.profiles.push(route_b);
    config
        .selected_models_by_provider
        .insert(provider_b.clone(), vec![model.clone()]);
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let client = reqwest::Client::new();
    let turn_metadata = json!({
        ROUTE_METADATA_KEY: provider_b,
        "preserved": "yes"
    })
    .to_string();

    let first = client
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .header("thread-id", "thread-route-b")
        .header(TURN_METADATA_HEADER, &turn_metadata)
        .json(&json!({
            "model": model,
            "input": "first",
            "client_metadata": {
                "x-codex-turn-metadata": turn_metadata,
                "preserved": "body"
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::OK);

    let second = client
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .header("thread-id", "thread-route-b")
        .json(&json!({"model":"provider-model","input":"second"}))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::OK);

    let ambiguous = client
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .header("thread-id", "unbound-thread")
        .json(&json!({"model":"provider-model","input":"ambiguous"}))
        .send()
        .await
        .unwrap();
    assert_eq!(ambiguous.status(), reqwest::StatusCode::NOT_FOUND);
    assert!(
        ambiguous.json::<Value>().await.unwrap()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("缺少明确")
    );

    let bodies = upstream_task.await.unwrap();
    let nested = bodies[0]["client_metadata"][TURN_METADATA_HEADER]
        .as_str()
        .and_then(|metadata| serde_json::from_str::<Value>(metadata).ok())
        .unwrap();
    assert!(nested.get(ROUTE_METADATA_KEY).is_none());
    assert_eq!(nested["preserved"], "yes");
    assert_eq!(bodies[0]["client_metadata"]["preserved"], "body");
    router.stop().await.unwrap();
}

#[tokio::test]
async fn route_updates_affect_only_later_requests_and_keep_inflight_streams_pinned() {
    let upstream_a = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_a_address = upstream_a.local_addr().unwrap();
    let upstream_b = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_b_address = upstream_b.local_addr().unwrap();
    let (release_a, wait_for_release_a) = tokio::sync::oneshot::channel();
    let upstream_a_task = tokio::spawn(async move {
        let (mut stream, _) = upstream_a.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        assert_eq!(body["model"], "provider-model");
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let first = b"data: {\"type\":\"response.created\",\"source\":\"a\",\"phase\":1}\n\n";
        stream
            .write_all(format!("{:x}\r\n", first.len()).as_bytes())
            .await
            .unwrap();
        stream.write_all(first).await.unwrap();
        stream.write_all(b"\r\n").await.unwrap();
        stream.flush().await.unwrap();
        wait_for_release_a.await.unwrap();
        let second = b"data: {\"type\":\"response.completed\",\"source\":\"a\",\"phase\":2}\n\n";
        stream
            .write_all(format!("{:x}\r\n", second.len()).as_bytes())
            .await
            .unwrap();
        stream.write_all(second).await.unwrap();
        stream.write_all(b"\r\n0\r\n\r\n").await.unwrap();
    });
    let upstream_b_task = tokio::spawn(async move {
        let (mut stream, _) = upstream_b.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        write_json_response(
            &mut stream,
            200,
            &json!({"object":"response","source":"b","model":body["model"]}),
        )
        .await
        .unwrap();
    });
    let (config, provider_id, model) = router_config(format!("http://{upstream_a_address}/v1"));
    let alias = model_alias(&provider_id, &model);
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let client = reqwest::Client::new();

    let mut first_response = client
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({"model":alias,"input":"first","stream":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(first_response.status(), reqwest::StatusCode::OK);
    let first_payload = tokio::time::timeout(Duration::from_secs(5), async {
        let mut payload = Vec::new();
        loop {
            let chunk = first_response.chunk().await.unwrap().unwrap();
            payload.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&payload).contains("\"phase\":1") {
                break payload;
            }
        }
    })
    .await
    .unwrap();
    assert!(String::from_utf8_lossy(&first_payload).contains("\"source\":\"a\""));

    let mut updated = config.clone();
    updated.profiles[0].base_url = format!("http://{upstream_b_address}/v1");
    router.update_config(&updated);
    let second_response = client
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({"model":alias,"input":"second","stream":false}))
        .send()
        .await
        .unwrap();
    assert_eq!(second_response.status(), reqwest::StatusCode::OK);
    let second_body = second_response.json::<Value>().await.unwrap();
    assert_eq!(second_body["source"], "b");
    assert_eq!(second_body["model"], "provider-model");

    release_a.send(()).unwrap();
    let mut remaining_first_payload = Vec::new();
    while let Some(chunk) = first_response.chunk().await.unwrap() {
        remaining_first_payload.extend_from_slice(&chunk);
    }
    let remaining_first_payload = String::from_utf8_lossy(&remaining_first_payload);
    assert!(remaining_first_payload.contains("\"source\":\"a\""));
    assert!(remaining_first_payload.contains("\"phase\":2"));
    assert!(!remaining_first_payload.contains("\"source\":\"b\""));

    upstream_a_task.await.unwrap();
    upstream_b_task.await.unwrap();
    router.stop().await.unwrap();
}

#[test]
fn upstream_authority_omits_credentials_paths_and_queries() {
    assert_eq!(
        upstream_authority("https://user:secret@relay.example:8443/private/v1?token=hidden"),
        "relay.example:8443"
    );
}

#[tokio::test]
async fn transport_error_response_is_plain_text_and_non_retryable() {
    let (mut reader, mut writer) = tokio::io::duplex(4096);

    write_text_error_response(
        &mut writer,
        424,
        "upstream_unreachable",
        "Codey 线路「Relay」无法连接上游 relay.example:8443",
    )
    .await
    .unwrap();
    drop(writer);

    let mut response = String::new();
    reader.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 424 Failed Dependency\r\n"));
    assert!(response.contains("content-type: text/plain; charset=utf-8\r\n"));
    assert!(response.contains("Codey 线路「Relay」无法连接上游 relay.example:8443"));
    assert!(response.contains("错误码：upstream_unreachable"));
}

#[tokio::test]
async fn router_proxies_image_generation_to_the_default_openai_route() {
    let logs = tempfile::tempdir().unwrap();
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let authorization = incoming_header(&request, "authorization").map(str::to_string);
        let body = serde_json::from_slice::<Value>(&request.body).unwrap();
        let response = r#"{"created":1,"data":[{"b64_json":"aGVsbG8="}]}"#;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                    response.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        (request.path, authorization, body)
    });
    let (mut config, provider_id, model) =
        router_config(format!("http://{upstream_address}/v1/responses"));
    config.default_model = model_alias(&provider_id, &model);
    config.route_request_log.enabled = true;
    config.route_request_log.backend = RouteRequestLogBackend::Sqlite;
    config.route_request_log.batch_size = 1;
    let router = LocalRouter::start_with_logger(
        &config,
        Arc::new(RouteRequestLogController::with_root(
            logs.path().to_path_buf(),
        )),
    )
    .await
    .unwrap();
    let endpoint = router.endpoint();

    let response = reqwest::Client::new()
        .post(format!("{}/images/generations", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({"model":"gpt-image-2","prompt":"draw an otter"}))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.json::<Value>().await.unwrap()["created"], 1);
    let (path, authorization, body) = upstream_task.await.unwrap();
    assert_eq!(path, "/v1/images/generations");
    assert_eq!(authorization.as_deref(), Some("Bearer sk-upstream"));
    assert_eq!(body["model"], "gpt-image-2");
    assert_eq!(body["prompt"], "draw an otter");
    let invalid = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .body("not-json")
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);
    let deadline = Instant::now() + Duration::from_secs(2);
    while router.request_log_health().await.stats.entries_written < 2 && Instant::now() < deadline {
        tokio::task::yield_now().await;
    }
    let stats = reqwest::Client::new()
        .post(format!(
            "{}/codey/api/query_route_request_log_stats",
            endpoint.base_url.trim_end_matches("/v1")
        ))
        .header(ROUTER_AUTH_HEADER, &endpoint.token)
        .json(&json!({"groupBy":"request_kind"}))
        .send()
        .await
        .unwrap();
    assert_eq!(stats.status(), reqwest::StatusCode::OK);
    let stats = stats.json::<Value>().await.unwrap();
    assert_eq!(stats["total"], 2);
    assert_eq!(stats["succeededCount"], 1);
    assert_eq!(stats["failedCount"], 1);
    assert_eq!(stats["recordingHealth"]["entriesWritten"], 2);
    router.stop().await.unwrap();
    let page = crate::route_request_log::query_route_request_logs(
        logs.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    assert_eq!(page.total, 2);
    let image = page
        .items
        .iter()
        .find(|item| item.request_kind == "images_generations")
        .unwrap();
    assert_eq!(image.status, "succeeded");
    assert_eq!(image.model.as_deref(), Some("gpt-image-2"));
    assert_eq!(image.total_tokens, None);
    let rejected = page
        .items
        .iter()
        .find(|item| item.request_kind == "responses")
        .unwrap();
    assert_eq!(rejected.status, "failed");
    assert_eq!(rejected.error_code.as_deref(), Some("invalid_request_body"));
}

#[tokio::test]
async fn router_rejects_unknown_raw_models_instead_of_guessing_a_route() {
    let (config, _, _) = router_config("http://127.0.0.1:9/v1".to_string());
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({"model":"unknown-model","input":"hello"}))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(body["error"]["code"], "model_not_enabled");
    router.stop().await.unwrap();
}

#[tokio::test]
async fn router_rejects_requests_without_the_launch_token() {
    let (config, provider_id, model) = router_config("http://127.0.0.1:9/v1".to_string());
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .json(&json!({"model":model_alias(&provider_id, &model),"input":"hello"}))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_router_token");

    let gateway_root = endpoint.base_url.trim_end_matches("/v1");
    let legacy_capability_response = reqwest::Client::new()
        .post(format!("{gateway_root}/{}/v1/responses", endpoint.token))
        .json(&json!({"model":model_alias(&provider_id, &model),"input":"hello"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        legacy_capability_response.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    router.stop().await.unwrap();
}

#[tokio::test]
async fn router_rejects_unauthorized_request_before_reading_its_body() {
    let (config, _, _) = router_config("http://127.0.0.1:9/v1".to_string());
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let url = reqwest::Url::parse(&endpoint.base_url).unwrap();
    let mut stream = TcpStream::connect((url.host_str().unwrap(), url.port().unwrap()))
        .await
        .unwrap();
    stream
        .write_all(
            b"POST /v1/responses HTTP/1.1\r\nhost: localhost\r\ncontent-length: 1048576\r\nconnection: close\r\n\r\n",
        )
        .await
        .unwrap();

    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(1), stream.read_to_string(&mut response))
        .await
        .expect("unauthorized request must not wait for its declared body")
        .unwrap();
    assert!(response.starts_with("HTTP/1.1 401 Unauthorized\r\n"));
    assert!(response.contains("invalid_router_token"));
    router.stop().await.unwrap();
}

#[tokio::test]
async fn request_log_excludes_non_model_paths_but_keeps_rejected_model_requests() {
    let logs = tempfile::tempdir().unwrap();
    let (mut config, _, _) = router_config("http://127.0.0.1:9/v1".to_string());
    config.route_request_log.enabled = true;
    config.route_request_log.backend = RouteRequestLogBackend::Sqlite;
    let router = LocalRouter::start_with_logger(
        &config,
        Arc::new(RouteRequestLogController::with_root(
            logs.path().to_path_buf(),
        )),
    )
    .await
    .unwrap();
    let endpoint = router.endpoint();
    let root = endpoint.base_url.trim_end_matches("/v1");
    let client = reqwest::Client::new();

    for path in [
        "/favicon.ico",
        "/apple-touch-icon.png",
        "/robots.txt",
        "/.well-known/appspecific/com.chrome.devtools.json",
        "/",
        "/codey/missing.js",
    ] {
        for authenticated in [false, true] {
            let mut request = client.get(format!("{root}{path}"));
            if authenticated {
                request = request.bearer_auth(&endpoint.token);
            }
            let response = request.send().await.unwrap();
            assert_eq!(
                response.status().as_u16(),
                if authenticated { 404 } else { 401 }
            );
        }
    }
    let model_paths = [
        "/v1/models",
        "/models",
        "/v1/responses",
        "/responses",
        "/v1/images/generations",
        "/images/generations",
        "/v1/responses/compact",
        "/responses/compact",
        "/v1/v1/responses/compact",
        "/codex/v1/responses/compact",
    ];
    for path in model_paths {
        let response = client.post(format!("{root}{path}")).send().await.unwrap();
        assert_eq!(response.status().as_u16(), 401);
    }
    let response = client
        .get(format!("{root}/v1/responses"))
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 404);
    router.stop().await.unwrap();
    let page = crate::route_request_log::query_route_request_logs(
        logs.path(),
        RouteRequestLogBackend::Sqlite,
        RouteRequestLogQuery::default(),
    )
    .unwrap();
    assert_eq!(page.total, (model_paths.len() + 1) as u64);
    assert_eq!(
        page.items
            .iter()
            .filter(|entry| entry.error_code.as_deref() == Some("invalid_router_token"))
            .count(),
        model_paths.len()
    );
    assert_eq!(
        page.items
            .iter()
            .filter(|entry| entry.error_code.as_deref() == Some("not_found"))
            .count(),
        1
    );
}

#[tokio::test]
async fn request_log_page_is_public_but_its_api_requires_the_launch_token() {
    let (config, provider_id, model) = router_config("http://127.0.0.1:9/v1".to_string());
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let gateway_root = endpoint.base_url.trim_end_matches("/v1");
    let client = reqwest::Client::new();

    let page = client
        .get(format!("{gateway_root}{REQUEST_LOG_PAGE_PATH}"))
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), reqwest::StatusCode::OK);
    assert!(page.text().await.unwrap().contains(REQUEST_LOG_SCRIPT_PATH));
    let script = client
        .get(format!("{gateway_root}{REQUEST_LOG_SCRIPT_PATH}"))
        .send()
        .await
        .unwrap();
    assert_eq!(script.status(), reqwest::StatusCode::OK);
    assert!(script.text().await.unwrap().contains(REQUEST_LOG_PAGE_PATH));
    assert_eq!(
        endpoint.request_log_url(),
        format!("{gateway_root}{REQUEST_LOG_PAGE_PATH}#{}", endpoint.token)
    );

    for command in [
        "load_codey_config",
        "query_route_request_logs",
        "query_route_request_log_stats",
        "query_official_account_usage",
    ] {
        let unauthorized = client
            .post(format!("{gateway_root}/codey/api/{command}"))
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    let catalog = client
        .post(format!("{gateway_root}/codey/api/load_codey_config"))
        .header(ROUTER_AUTH_HEADER, &endpoint.token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(catalog.status(), reqwest::StatusCode::OK);
    let catalog = catalog.json::<Value>().await.unwrap();
    assert_eq!(catalog["config"]["profiles"][0]["id"], "route-a");
    assert_eq!(
        catalog["config"]["selectedModelsByProvider"][&provider_id][0],
        model
    );
    assert!(catalog["config"]["profiles"][0].get("apiKey").is_none());
    assert!(catalog["config"]["profiles"][0].get("baseUrl").is_none());

    let usage = client
        .post(format!(
            "{gateway_root}/codey/api/query_official_account_usage"
        ))
        .header(ROUTER_AUTH_HEADER, &endpoint.token)
        .json(&json!({"forceRefresh": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(usage.status(), reqwest::StatusCode::OK);
    assert_eq!(
        usage.json::<Value>().await.unwrap()["status"],
        "unavailable"
    );

    router.stop().await.unwrap();
}

#[test]
fn reason_phrase_uses_canonical_text_for_every_status() {
    assert_eq!(reason_phrase(200), "OK");
    assert_eq!(reason_phrase(401), "Unauthorized");
    assert_eq!(reason_phrase(424), "Failed Dependency");
    // Statuses that the old fixed table lacked no longer fall back to "OK".
    assert_eq!(reason_phrase(403), "Forbidden");
    assert_eq!(reason_phrase(413), "Payload Too Large");
    assert_eq!(reason_phrase(429), "Too Many Requests");
    assert_eq!(reason_phrase(529), "Unknown");
}

#[tokio::test]
async fn text_error_response_status_line_carries_matching_reason() {
    let (mut client, mut server) = tokio::io::duplex(4096);
    write_text_error_response(&mut server, 429, "upstream_http_error", "限流")
        .await
        .unwrap();
    drop(server);
    let mut raw = Vec::new();
    client.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8(raw).unwrap();
    assert!(
        text.starts_with("HTTP/1.1 429 Too Many Requests\r\n"),
        "unexpected status line: {text}"
    );
    assert!(text.contains("限流（错误码：upstream_http_error）"));
}

#[tokio::test]
async fn request_body_read_reserves_declared_length_once() {
    let (mut client, mut server) = tokio::io::duplex(64 * 1024);
    let body = vec![b'x'; 40_000];
    let head = format!(
        "POST /v1/responses HTTP/1.1\r\ncontent-length: {}\r\n\r\n",
        body.len()
    );
    let writer = async move {
        client.write_all(head.as_bytes()).await.unwrap();
        for chunk in body.chunks(1_000) {
            client.write_all(chunk).await.unwrap();
        }
        drop(client);
    };
    let reader = async {
        let pending = read_http_request_head(&mut server).await.unwrap();
        read_http_request_body_with_budget(&mut server, pending, None)
            .await
            .unwrap()
    };
    let (_, request) = tokio::join!(writer, reader);
    assert_eq!(request.body.len(), 40_000);
    assert!(
        request.body.capacity() < 40_000 + 8192,
        "capacity {} suggests repeated doubling",
        request.body.capacity()
    );
}

#[tokio::test]
async fn anthropic_upstream_http_error_keeps_status_and_safe_text() {
    let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_address = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream.accept().await.unwrap();
        let request = read_http_request(&mut stream).await.unwrap();
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key sk-upstream"}}"#;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\nrequest-id: req_anthropic_1\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        request.path
    });
    let (mut config, provider_id, model) = router_config(format!("http://{upstream_address}/v1"));
    config.profiles[0].upstream_protocol =
        crate::config::UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES.into();
    config.profiles[0].normalize();
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();

    let response = reqwest::Client::new()
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({
            "model": model_alias(&provider_id, &model),
            "input":"hello",
            "stream":true
        }))
        .send()
        .await
        .unwrap();

    // The real upstream status reaches Codex instead of a retryable 502.
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/plain")
    );
    let body = response.text().await.unwrap();
    assert!(body.contains("线路「Relay」"), "{body}");
    assert!(body.contains("HTTP 401"), "{body}");
    assert!(body.contains("authentication_error"), "{body}");
    assert!(body.contains("invalid x-api-key ***"), "{body}");
    assert!(body.contains("上游请求 ID：req_anthropic_1"), "{body}");
    assert!(!body.contains("sk-upstream"), "{body}");
    assert_eq!(upstream_task.await.unwrap(), "/v1/messages");
    router.stop().await.unwrap();
}
