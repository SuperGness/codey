//! Long-response regression fixtures and an opt-in loopback benchmark.
use super::*;

struct Fixture {
    request: Value,
    frames: Arc<Vec<String>>,
    text_bytes: usize,
    tools: usize,
    success: bool,
}

fn fixture(protocol: &str, case: &str, text_bytes: usize) -> Fixture {
    let mixed = matches!(case, "mixed" | "invalid_custom" | "invalid_search");
    let definitions = if mixed {
        json!([
            {"type":"namespace","name":"fs","tools":[{"type":"function","name":"lookup","parameters":{"type":"object"}}]},
            {"type":"custom","name":"apply_patch","description":"Apply a patch"},
            {"type":"tool_search","execution":"client","description":"Find tools","parameters":{"type":"object","properties":{"goal":{"type":"string"}},"required":["goal"]}}
        ])
    } else if matches!(case, "parallel_tools" | "invalid_function") {
        Value::Array((0..8).map(|i| json!({"type":"function","name":format!("lookup_{i}"),"parameters":{"type":"object"}})).collect())
    } else {
        json!([])
    };
    let request = json!({"model":"provider-model","input":[{"role":"user","content":"earlier"},{"role":"assistant","content":"previous answer"},{"role":"user","content":"continue"}],"stream":true,"tools":definitions});
    let chat = protocol == UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS;
    let converted = if chat {
        responses_to_chat_completions_request(&request)
    } else {
        responses_to_anthropic_messages_request(&request)
    }
    .unwrap();
    let names = converted.body["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .map(|tool| {
                    if chat {
                        tool["function"]["name"].as_str()
                    } else {
                        tool["name"].as_str()
                    }
                    .unwrap()
                    .to_string()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut events = Vec::new();
    if !chat {
        events.push(json!({"type":"message_start","message":{"id":"msg-upstream","role":"assistant","content":[],"usage":{"input_tokens":4,"cache_read_input_tokens":3,"cache_creation_input_tokens":2}}}));
        events.push(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}));
    }
    for length in (0..text_bytes)
        .step_by(8192)
        .map(|i| 8192.min(text_bytes - i))
    {
        let text = "x".repeat(length);
        events.push(if chat { json!({"choices":[{"index":0,"delta":{"content":text}}]}) } else { json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}) });
    }
    for (index, name) in names.iter().enumerate() {
        let payload = "z".repeat(if case == "parallel_tools" {
            64 * 1024
        } else {
            16 * 1024
        });
        let arguments = if case == "invalid_function" && index == 7 {
            "{broken".into()
        } else if mixed && index == 1 {
            if case == "invalid_custom" {
                "{\"input\":7}".into()
            } else {
                json!({"input":payload}).to_string()
            }
        } else if mixed && index == 2 {
            if case == "invalid_search" {
                "[]".into()
            } else {
                json!({"goal":payload}).to_string()
            }
        } else {
            json!({"value":payload}).to_string()
        };
        events.push(if chat { json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":index,"id":format!("call-{index}"),"type":"function","function":{"name":name,"arguments":""}}]}}]}) }
            else { json!({"type":"content_block_start","index":index+1,"content_block":{"type":"tool_use","id":format!("call-{index}"),"name":name,"input":{}}}) });
        for part in arguments.as_bytes().chunks(4096) {
            let text = std::str::from_utf8(part).unwrap();
            events.push(if chat { json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":index,"function":{"arguments":text}}]}}]}) }
                else { json!({"type":"content_block_delta","index":index+1,"delta":{"type":"input_json_delta","partial_json":text}}) });
        }
    }
    if case == "refusal" {
        events.push(if chat { json!({"choices":[{"index":0,"delta":{"refusal":"cannot answer"}}]}) }
            else { json!({"type":"content_block_start","index":1,"content_block":{"type":"refusal","refusal":"cannot answer"}}) });
    }
    if case == "thinking" && !chat {
        events.push(json!({"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":"private"}}));
        events.push(json!({"type":"content_block_delta","index":1,"delta":{"type":"thinking_delta","thinking":" reasoning"}}));
    }
    events.push(if chat { json!({"choices":[{"index":0,"finish_reason":if case == "refusal" { "content_filter" } else if case == "length" { "length" } else { "stop" }}],"usage":{"prompt_tokens":9,"completion_tokens":2,"total_tokens":11,"prompt_tokens_details":{"cached_tokens":3},"completion_tokens_details":{"reasoning_tokens":1}}}) }
        else { json!({"type":"message_delta","delta":{"stop_reason":if case == "refusal" { "refusal" } else if case == "length" { "max_tokens" } else { "end_turn" }},"usage":{"output_tokens":2}}) });
    let frames = events
        .into_iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect();
    Fixture {
        request,
        frames: Arc::new(frames),
        text_bytes,
        tools: names.len(),
        success: !matches!(case, "invalid_custom" | "invalid_search")
            && (case != "invalid_function" || chat),
    }
}

async fn start_fixture(
    protocol: &str,
    frames: Arc<Vec<String>>,
) -> (LocalRouter, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut requests = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut socket, _) = accepted.unwrap();
                    socket.set_nodelay(true).unwrap();
                    let frames = frames.clone();
                    requests.spawn(async move {
                        read_http_request(&mut socket).await.unwrap();
                        socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n").await.unwrap();
                        for frame in frames.iter() { write_chunked_frame(&mut socket, frame.as_bytes(), "tail benchmark frame").await.unwrap(); }
                        finish_chunked_response(&mut socket).await.unwrap();
                    });
                }
                finished = requests.join_next(), if !requests.is_empty() => { finished.unwrap().unwrap(); }
            }
        }
    });
    let (mut config, _, _) = super::tests::router_config(format!("http://{address}/v1"));
    config.profiles[0].upstream_protocol = protocol.into();
    config.profiles[0].normalize();
    (LocalRouter::start(&config).await.unwrap(), task)
}

async fn measure(
    client: &reqwest::Client,
    endpoint: &RuntimeRouterEndpoint,
    fixture: &Fixture,
    capture: bool,
) -> Result<(f64, f64, f64, Vec<Value>)> {
    let start = Instant::now();
    let mut response = client
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&fixture.request)
        .send()
        .await?
        .error_for_status()?;
    let mut first = None;
    let mut last_delta = None;
    let mut terminal = None;
    let mut events = Vec::new();
    let mut buffer = Vec::new();
    let mut cursor = SseCursor::default();
    while let Some(chunk) = response.chunk().await? {
        compact_sse_buffer(&mut buffer, &mut cursor);
        buffer.extend_from_slice(&chunk);
        while let Some(frame) = take_next_sse_frame(&buffer, &mut cursor) {
            let Some(data) = sse_frame_data(frame)? else {
                continue;
            };
            let event: Value = serde_json::from_str(&data)?;
            let kind = event["type"].as_str().unwrap_or_default();
            if kind.ends_with(".delta") && event["delta"].as_str().is_some_and(|s| !s.is_empty()) {
                first.get_or_insert_with(Instant::now);
                last_delta = Some(Instant::now());
            }
            if responses_event_is_terminal(&event) {
                anyhow::ensure!(terminal.is_none(), "duplicate terminal");
                terminal = Some(Instant::now());
                anyhow::ensure!(
                    responses_event_is_failure(&event) != fixture.success,
                    "unexpected terminal {kind}"
                );
                if fixture.success {
                    anyhow::ensure!(
                        event["response"]["output_text"]
                            .as_str()
                            .unwrap_or_default()
                            .len()
                            == fixture.text_bytes
                    );
                    let tools = event["response"]["output"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|item| item["type"] != "message")
                        .count();
                    anyhow::ensure!(tools == fixture.tools);
                    anyhow::ensure!(event["response"]["usage"]["total_tokens"] == 11);
                }
            }
            if capture {
                events.push(event);
            }
        }
    }
    let terminal = terminal.context("missing terminal")?;
    Ok((
        first
            .unwrap_or(terminal)
            .duration_since(start)
            .as_secs_f64()
            * 1000.0,
        terminal
            .duration_since(last_delta.unwrap_or(terminal))
            .as_secs_f64()
            * 1000.0,
        start.elapsed().as_secs_f64() * 1000.0,
        events,
    ))
}

fn normalize(value: &mut Value, ids: &mut HashMap<String, String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if key == "created_at" {
                    *value = json!(0);
                } else {
                    normalize(value, ids);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                normalize(value, ids);
            }
        }
        Value::String(text)
            if [
                "resp_codey_",
                "msg_codey_",
                "fc_codey_",
                "ctc_codey_",
                "tsc_codey_",
            ]
            .iter()
            .any(|prefix| text.starts_with(prefix)) =>
        {
            let next = format!("generated-{}", ids.len());
            *text = ids.entry(text.clone()).or_insert(next).clone();
        }
        _ => {}
    }
}

#[tokio::test]
async fn long_response_tail_preserves_events_tools_and_errors() {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let mut snapshots = Vec::new();
    for protocol in [
        UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
        UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
    ] {
        for case in [
            "text",
            "parallel_tools",
            "mixed",
            "invalid_function",
            "invalid_custom",
            "invalid_search",
            "refusal",
            "length",
            "thinking",
        ] {
            let fixture = fixture(protocol, case, 32 * 1024);
            let (router, mock) = start_fixture(protocol, fixture.frames.clone()).await;
            let (_, _, _, events) = measure(&client, &router.endpoint(), &fixture, true)
                .await
                .unwrap();
            let response = &events.last().unwrap()["response"];
            if fixture.success {
                assert_eq!(response["output_text"], "x".repeat(fixture.text_bytes));
            }
            if case == "mixed" {
                let output = response["output"].as_array().unwrap();
                assert_eq!(output[1]["type"], "function_call");
                assert_eq!(output[1]["name"], "lookup");
                assert_eq!(output[1]["namespace"], "fs");
                assert_eq!(output[2]["type"], "custom_tool_call");
                assert_eq!(output[2]["input"], "z".repeat(16 * 1024));
                assert_eq!(output[3]["type"], "tool_search_call");
                assert_eq!(output[3]["execution"], "client");
                assert_eq!(output[3]["arguments"]["goal"], "z".repeat(16 * 1024));
            }
            if matches!(case, "refusal" | "length") {
                assert_eq!(response["status"], "incomplete");
                assert_eq!(
                    response["incomplete_details"]["reason"],
                    if case == "refusal" {
                        "content_filter"
                    } else {
                        "max_output_tokens"
                    }
                );
            }
            let mut snapshot = json!({"protocol":protocol,"case":case,"events":events});
            normalize(&mut snapshot, &mut HashMap::new());
            snapshots.push(snapshot);
            router.stop().await.unwrap();
            mock.abort();
        }
    }
    let encoded = serde_json::to_vec(&snapshots).unwrap();
    if let Ok(path) = std::env::var("CODEY_TAIL_SNAPSHOT") {
        std::fs::write(path, &encoded).unwrap();
    }
    // Full normalized transcripts captured before the optimization at f396fabc.
    // CODEY_TAIL_SNAPSHOT exports them for inspection when this assertion fails.
    use sha2::Digest;
    assert_eq!(
        format!("{:x}", sha2::Sha256::digest(&encoded)),
        "8413d292f4b1d9136d376050c21545c5eb087c5a92f4fd5ba7f5440f55aaea9b"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opt-in long-response loopback benchmark"]
async fn long_response_tail_latency() {
    let protocol = std::env::var("CODEY_BENCH_PROTOCOL")
        .unwrap_or_else(|_| UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into());
    let case = std::env::var("CODEY_BENCH_CASE").unwrap_or_else(|_| "text".into());
    let concurrency = std::env::var("CODEY_BENCH_CONCURRENCY")
        .map(|v| v.parse::<usize>().unwrap())
        .unwrap_or(1);
    let fixture = Arc::new(fixture(
        &protocol,
        &case,
        if case == "parallel_tools" {
            0
        } else {
            1024 * 1024
        },
    ));
    let (router, mock) = start_fixture(&protocol, fixture.frames.clone()).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    measure(&client, &router.endpoint(), &fixture, false)
        .await
        .unwrap();
    let before = super::latency_bench::resources();
    let start = Instant::now();
    let mut jobs = JoinSet::new();
    for _ in 0..concurrency {
        let fixture = fixture.clone();
        let client = client.clone();
        let endpoint = router.endpoint();
        jobs.spawn(async move {
            let mut samples = Vec::new();
            for _ in 0..8 {
                samples.push(measure(&client, &endpoint, &fixture, false).await);
            }
            samples
        });
    }
    let mut first = Vec::new();
    let mut tail = Vec::new();
    let mut total = Vec::new();
    let mut errors = 0;
    while let Some(result) = jobs.join_next().await {
        for sample in result.unwrap() {
            match sample {
                Ok((a, b, c, _)) => {
                    first.push(a);
                    tail.push(b);
                    total.push(c);
                }
                Err(error) => {
                    eprintln!("{error:#}");
                    errors += 1;
                }
            }
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    let after = super::latency_bench::resources();
    let percentile = super::latency_bench::percentile;
    println!(
        "TAIL_BENCH {}",
        json!({"protocol":protocol,"case":case,"concurrency":concurrency,"requests":concurrency*8,"errors":errors,"first_content_p50_ms":percentile(&mut first,50),"tail_p50_ms":percentile(&mut tail,50),"tail_p95_ms":percentile(&mut tail,95),"total_p50_ms":percentile(&mut total,50),"total_p95_ms":percentile(&mut total,95),"requests_per_second":total.len() as f64/elapsed,"cpu_ms":after.0-before.0,"process_peak_rss_mib":after.1})
    );
    router.stop().await.unwrap();
    mock.abort();
    assert_eq!(errors, 0);
}
