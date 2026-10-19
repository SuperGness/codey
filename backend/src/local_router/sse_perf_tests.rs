use super::*;

/// 固定输入的本地微基准，不访问上游；仅比较 SSE 重写路径的 CPU 开销。
#[test]
#[ignore = "opt-in SSE rewrite microbenchmark"]
fn sse_rewrite_microbenchmark() {
    for (payload_bytes, frames_per_chunk, iterations) in
        [(64, 1, 40_000), (64, 16, 2_500), (65_536, 1, 160)]
    {
        let frame = format!(
            "data: {{\"type\":\"response.output_text.delta\",\"delta\":\"{}\"}}\n\n",
            "x".repeat(payload_bytes)
        );
        let input = frame.repeat(frames_per_chunk).into_bytes();
        let mut elapsed_us = Vec::new();
        for _ in 0..7 {
            let mut rewriter = ResponsesSseRewriter::new(None, true, None);
            assert_eq!(rewriter.push(&input).unwrap(), input);
            let start = Instant::now();
            let mut bytes = 0;
            for _ in 0..iterations {
                let output = rewriter.push(std::hint::black_box(&input)).unwrap();
                bytes += std::hint::black_box(output).len();
            }
            elapsed_us.push(start.elapsed().as_secs_f64() * 1_000_000.0);
            assert_eq!(bytes, input.len() * iterations);
            assert!(rewriter.finish().is_empty());
        }
        elapsed_us.sort_by(f64::total_cmp);
        println!(
            "SSE_REWRITE_BENCH {}",
            json!({"payload_bytes":payload_bytes,"frames_per_chunk":frames_per_chunk,
                "iterations":iterations,"samples":elapsed_us.len(),
                "median_us":elapsed_us[elapsed_us.len()/2],
                "min_us":elapsed_us[0],"max_us":elapsed_us[elapsed_us.len()-1]})
        );
    }
}

#[test]
#[ignore = "opt-in native SSE terminal microbenchmark"]
fn native_sse_terminal_microbenchmark() {
    for (payload_bytes, frames_per_chunk, iterations) in
        [(64, 1, 40_000), (64, 16, 2_500), (65_536, 1, 160)]
    {
        let frame = format!(
            "data: {{\"type\":\"response.output_text.delta\",\"delta\":\"{}\"}}\n\n",
            "x".repeat(payload_bytes)
        );
        let input = frame.repeat(frames_per_chunk).into_bytes();
        let mut elapsed_us = Vec::new();
        for _ in 0..7 {
            let mut terminal = NativeSseTerminal::default();
            assert!(!terminal.observe(&input).unwrap());
            let start = Instant::now();
            for _ in 0..iterations {
                assert!(!std::hint::black_box(
                    terminal.observe(std::hint::black_box(&input)).unwrap()
                ));
            }
            elapsed_us.push(start.elapsed().as_secs_f64() * 1_000_000.0);
            assert!(
                terminal
                    .observe(b"data: {\"type\":\"response.completed\"}\n\n")
                    .unwrap()
            );
            terminal.finish().unwrap();
        }
        elapsed_us.sort_by(f64::total_cmp);
        println!(
            "NATIVE_SSE_TERMINAL_BENCH {}",
            json!({"payload_bytes":payload_bytes,"frames_per_chunk":frames_per_chunk,
                "iterations":iterations,"samples":elapsed_us.len(),
                "median_us":elapsed_us[elapsed_us.len()/2],
                "min_us":elapsed_us[0],"max_us":elapsed_us[elapsed_us.len()-1]})
        );
    }
}

#[tokio::test]
async fn native_sse_forwards_first_delta_before_completion_and_keeps_log_usage() {
    const FIRST: &[u8] = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n";
    const LAST: &[u8] = b"data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":2}}}\n\n";
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = upstream.local_addr().unwrap();
    let (allow_completion, wait_for_client) = oneshot::channel();
    let mock = tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        read_http_request(&mut socket).await.unwrap();
        socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n").await.unwrap();
        write_chunked_frame(&mut socket, FIRST, "test first delta")
            .await
            .unwrap();
        wait_for_client.await.unwrap();
        write_chunked_frame(&mut socket, LAST, "test completion")
            .await
            .unwrap();
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
    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = downstream.local_addr().unwrap();
    let probe = RouteRequestLogProbe::detached_test_probe();
    let proxy_probe = probe.clone();
    let proxy = tokio::spawn(async move {
        let (mut socket, _) = downstream.accept().await.unwrap();
        write_proxy_response(&mut socket, response, Some(&proxy_probe), true).await
    });
    let mut client = TcpStream::connect(address).await.unwrap();
    let mut wire = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !wire.windows(FIRST.len()).any(|bytes| bytes == FIRST) {
            assert_ne!(client.read_buf(&mut wire).await.unwrap(), 0);
        }
    })
    .await
    .expect("首个 delta 必须在上游生成完成之前转发");
    allow_completion.send(()).unwrap();
    client.read_to_end(&mut wire).await.unwrap();
    proxy.await.unwrap().unwrap();
    mock.await.unwrap();
    assert!(wire.windows(LAST.len()).any(|bytes| bytes == LAST));
    tokio::time::timeout(Duration::from_secs(2), async {
        while probe.token_usage_for_test().output_tokens != Some(2) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("共享输出缓冲区应完整交给异步日志消费");
    assert_eq!(probe.token_usage_for_test().input_tokens, Some(7));
}
