//! Opt-in loopback measurements; no provider account or persisted configuration.
use super::*;

fn sse(event: Value) -> String {
    format!("data: {event}\n\n")
}

fn fixture(protocol: &str) -> (String, String) {
    match protocol {
        UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS => (
            sse(json!({"choices":[{"index":0,"delta":{"content":"hello"}}]})),
            sse(
                json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":4,"completion_tokens":1,"total_tokens":5}}),
            ) + "data: [DONE]\n\n",
        ),
        UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES => (
            sse(
                json!({"type":"message_start","message":{"id":"msg-bench","model":"bench-model",
                "role":"assistant","content":[],"usage":{"input_tokens":4}}}),
            ) + &sse(
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            ) + &sse(
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}),
            ),
            sse(json!({"type":"content_block_stop","index":0}))
                + &sse(
                    json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}),
                )
                + &sse(json!({"type":"message_stop"})),
        ),
        _ => (
            sse(json!({"type":"response.output_text.delta","delta":"hello"})),
            sse(
                json!({"type":"response.completed","response":{"id":"resp-bench","status":"completed",
                "output":[],"usage":{"input_tokens":4,"output_tokens":1,"total_tokens":5}}}),
            ),
        ),
    }
}

#[cfg(unix)]
fn resources() -> (f64, f64) {
    // getrusage reports this process, including the mock upstream and client.
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    let usage = unsafe {
        assert_eq!(libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()), 0);
        usage.assume_init()
    };
    let cpu_ms = (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64 * 1000.0
        + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1000.0;
    let rss_bytes = usage.ru_maxrss as f64
        * if cfg!(target_os = "macos") {
            1.0
        } else {
            1024.0
        };
    (cpu_ms, rss_bytes / (1024.0 * 1024.0))
}

#[cfg(not(unix))]
fn resources() -> (f64, f64) {
    (f64::NAN, f64::NAN)
}

fn percentile(values: &mut [f64], percent: usize) -> f64 {
    values.sort_by(f64::total_cmp);
    values[(values.len() * percent).div_ceil(100).saturating_sub(1)]
}

async fn measure_request(
    client: &reqwest::Client,
    endpoint: &RuntimeRouterEndpoint,
    streaming: bool,
) -> Result<(f64, f64)> {
    let start = Instant::now();
    let mut response = client
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(&endpoint.token)
        .json(&json!({"model":"bench-model","input":[
            {"role":"user","content":"earlier"},
            {"role":"assistant","content":"previous answer"},
            {"role":"user","content":"hello"}],"stream":streaming}))
        .send()
        .await?
        .error_for_status()?;
    let mut buffer = Vec::new();
    let mut first_content = None;
    let mut terminal = false;
    let mut cursor = SseCursor::default();
    while let Some(chunk) = response.chunk().await? {
        buffer.extend_from_slice(&chunk);
        if streaming {
            while let Some(frame) = take_next_sse_frame(&buffer, &mut cursor) {
                let Some(data) = sse_frame_data(frame)? else {
                    continue;
                };
                let event: Value = serde_json::from_str(&data)?;
                if event["type"] == "response.output_text.delta" && event["delta"] == "hello" {
                    first_content.get_or_insert(start.elapsed().as_secs_f64() * 1000.0);
                }
                terminal |= event["type"] == "response.completed";
            }
        }
    }
    let total = start.elapsed().as_secs_f64() * 1000.0;
    if !streaming {
        let response: Value = serde_json::from_slice(&buffer)?;
        anyhow::ensure!(response["output_text"] == "hello");
        terminal = response["status"] == "completed";
        first_content = Some(total);
    }
    anyhow::ensure!(terminal, "missing completed event");
    Ok((first_content.context("missing text delta")?, total))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opt-in local-router latency benchmark"]
async fn loopback_latency() {
    let only = std::env::var("CODEY_BENCH_PROTOCOL").ok();
    let concurrency = std::env::var("CODEY_BENCH_CONCURRENCY")
        .ok()
        .map(|v| v.parse::<usize>().unwrap());
    for (protocol, streaming) in [
        (UPSTREAM_PROTOCOL_OPENAI_RESPONSES, true),
        (UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS, true),
        (UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES, true),
        (UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS, false),
    ] {
        if only.as_deref().is_some_and(|value| value != protocol) {
            continue;
        }
        for workers in [1, 8, 32, 64] {
            if concurrency.is_some_and(|value| value != workers) {
                continue;
            }
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = listener.local_addr().unwrap();
            let mock = tokio::spawn(async move {
                let mut requests = JoinSet::new();
                loop {
                    tokio::select! {
                        result = listener.accept() => {
                            let (mut socket, _) = result.unwrap();
                            socket.set_nodelay(true).unwrap();
                            requests.spawn(async move {
                                read_http_request(&mut socket).await.unwrap();
                                let (first, last) = fixture(protocol);
                                socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n").await.unwrap();
                                tokio::time::sleep(Duration::from_millis(10)).await;
                                write_chunked_frame(&mut socket, first.as_bytes(), "benchmark first chunk").await.unwrap();
                                tokio::time::sleep(Duration::from_millis(10)).await;
                                write_chunked_frame(&mut socket, last.as_bytes(), "benchmark last chunk").await.unwrap();
                                finish_chunked_response(&mut socket).await.unwrap();
                            });
                        }
                        result = requests.join_next(), if !requests.is_empty() => { result.unwrap().unwrap(); }
                    }
                }
            });
            let mut route = ProviderProfile::new("Benchmark");
            route.id = "benchmark".into();
            route.api_key = "benchmark-placeholder".into();
            route.base_url = format!("http://{address}/v1");
            route.upstream_protocol = protocol.into();
            route.normalize();
            let provider = route.provider_id().to_string();
            let mut config = CodeyConfig {
                active_profile_id: route.id.clone(),
                profiles: vec![route],
                ..CodeyConfig::default()
            }
            .normalize();
            config
                .selected_models_by_provider
                .insert(provider, vec!["bench-model".into()]);
            let router = LocalRouter::start(&config).await.unwrap();
            let client = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap();
            for _ in 0..4 {
                measure_request(&client, &router.endpoint(), streaming)
                    .await
                    .unwrap();
            }
            let before = resources();
            let start = Instant::now();
            let mut jobs = JoinSet::new();
            for _ in 0..workers {
                let client = client.clone();
                let endpoint = router.endpoint();
                jobs.spawn(async move {
                    let mut samples = Vec::new();
                    for _ in 0..8 {
                        samples.push(measure_request(&client, &endpoint, streaming).await);
                    }
                    samples
                });
            }
            let mut first = Vec::new();
            let mut totals = Vec::new();
            let mut errors = 0;
            while let Some(result) = jobs.join_next().await {
                for sample in result.unwrap() {
                    match sample {
                        Ok((ttft, total)) => {
                            first.push(ttft);
                            totals.push(total);
                        }
                        Err(error) => {
                            errors += 1;
                            eprintln!("benchmark request failed: {error:#}");
                        }
                    }
                }
            }
            let elapsed = start.elapsed().as_secs_f64();
            let after = resources();
            assert!(!first.is_empty());
            println!(
                "BENCH {}",
                json!({"protocol":protocol,"stream":streaming,"concurrency":workers,
                "requests":workers*8,"errors":errors,"first_content_p50_ms":percentile(&mut first,50),
                "first_content_p95_ms":percentile(&mut first,95),"total_p50_ms":percentile(&mut totals,50),
                "total_p95_ms":percentile(&mut totals,95),"requests_per_second":first.len() as f64/elapsed,
                "cpu_ms":after.0-before.0,"process_peak_rss_mib":after.1})
            );
            router.stop().await.unwrap();
            mock.abort();
            let _ = mock.await;
            assert_eq!(errors, 0);
        }
    }
}

#[test]
#[ignore = "opt-in fragmented SSE parser benchmark"]
fn fragmented_sse_latency() {
    for size in [16 * 1024, 128 * 1024, 512 * 1024] {
        let bytes = sse(json!({"delta":"x".repeat(size)})).into_bytes();
        let mut samples = Vec::new();
        for _ in 0..8 {
            let mut buffer = Vec::new();
            let mut cursor = SseCursor::default();
            let start = Instant::now();
            let mut frames = 0;
            for chunk in bytes.chunks(256) {
                compact_sse_buffer(&mut buffer, &mut cursor);
                buffer.extend_from_slice(chunk);
                while let Some(frame) = take_next_sse_frame(&buffer, &mut cursor) {
                    std::hint::black_box(frame);
                    frames += 1;
                }
            }
            assert_eq!(frames, 1);
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        println!(
            "BENCH {}",
            json!({"case":"fragmented_sse","event_bytes":size,"chunk_bytes":256,
            "p50_ms":percentile(&mut samples,50),"p95_ms":percentile(&mut samples,95)})
        );
    }
}
