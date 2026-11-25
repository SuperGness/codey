use super::tests::{connect_router_websocket, router_config};
use super::*;

pub(super) async fn error_upstream(
    status: u16,
    body: String,
    content_type: &'static str,
) -> (String, tokio::task::JoinHandle<()>, Arc<Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let body = body.clone();
            let recorded = recorded.clone();
            connections.spawn(async move {
                let request = read_http_request(&mut socket).await.unwrap();
                recorded.lock().unwrap().push(serde_json::from_slice(&request.body).unwrap());
                let headers = format!("HTTP/1.1 {status} {}\r\ncontent-type: {content_type}\r\nretry-after: 1\r\nx-request-id: retry-contract\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", reason_phrase(status), body.len());
                socket.write_all(headers.as_bytes()).await.unwrap();
                socket.write_all(body.as_bytes()).await.unwrap();
            });
            while connections.try_join_next().is_some() {}
        }
    });
    (format!("http://{address}/v1"), task, requests)
}

async fn websocket_terminal(socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(message) = socket.next().await {
            if let WebSocketMessage::Text(text) = message.unwrap() {
                let event: Value = serde_json::from_str(text.as_ref()).unwrap();
                if responses_event_is_terminal(&event) {
                    return event;
                }
            }
        }
        panic!("missing terminal event")
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn streaming_http_and_websocket_share_retry_classification_across_turns() {
    for (status, source, expected) in [
        (429, "rate_limit_exceeded", "rate_limit_exceeded"),
        (429, "insufficient_quota", "insufficient_quota"),
        (401, "invalid_api_key", "invalid_prompt"),
        (503, "server_error", "rate_limit_exceeded"),
        (400, "context_length_exceeded", CONTEXT_LENGTH_EXCEEDED),
    ] {
        let body =
            json!({"error":{"code":source,"message":"fixture failure sk-upstream"}}).to_string();
        let (url, task, requests) = error_upstream(status, body, "application/json").await;
        let (config, provider, model) = router_config(url);
        let router = LocalRouter::start(&config).await.unwrap();
        let endpoint = router.endpoint();
        let model = model_alias(&provider, &model);
        let client = test_http_client_builder().no_proxy().build().unwrap();
        let mut websocket = connect_router_websocket(&endpoint).await;
        for _ in 0..2 {
            let response = client
                .post(format!("{}/responses", endpoint.base_url))
                .bearer_auth(&endpoint.token)
                .json(&json!({"model":model,"input":"continue","stream":true}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), 200);
            let body = response.text().await.unwrap();
            assert!(!body.contains("sk-upstream"));
            let events = parse_responses_websocket_sse_events(&body).unwrap();
            let http_error = &events.last().unwrap()["response"]["error"];
            websocket
                .send(WebSocketMessage::Text(
                    json!({"type":"response.create","model":model,"input":"continue"})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            let event = websocket_terminal(&mut websocket).await;
            let ws_error = &event["response"]["error"];
            for error in [http_error, ws_error] {
                assert_eq!(error["code"], expected, "{status} {source}");
                assert_eq!(error["codey"]["httpStatus"], status);
                assert_eq!(error["codey"]["upstreamCode"], source);
                assert_eq!(error["codey"]["upstreamRequestId"], "retry-contract");
                if expected == "rate_limit_exceeded" {
                    assert!(
                        error["message"]
                            .as_str()
                            .unwrap()
                            .starts_with("Please try again in ")
                    );
                    assert!(error["codey"]["retryAfterMs"].as_u64().unwrap() <= 1000);
                }
            }
        }
        assert_eq!(requests.lock().unwrap().len(), 4);
        websocket.close(None).await.unwrap();
        router.stop().await.unwrap();
        task.abort();
    }
}

#[tokio::test]
async fn non_stream_responses_and_compaction_keep_http_errors_and_retry_after() {
    let (url, task, _) = error_upstream(
        429,
        json!({"error":{"code":"rate_limit_exceeded","message":"limited"}}).to_string(),
        "application/json",
    )
    .await;
    let (mut config, provider, model) = router_config(url);
    config.profiles[0].supports_remote_compaction = true;
    let router = LocalRouter::start(&config).await.unwrap();
    let endpoint = router.endpoint();
    let client = test_http_client_builder().no_proxy().build().unwrap();
    for (path, stream, input) in [
        ("responses", false, json!("hello")),
        ("responses/compact", false, json!("hello")),
        ("responses", true, json!([{"type":"compaction_trigger"}])),
    ] {
        let response = client
            .post(format!("{}/{path}", endpoint.base_url))
            .bearer_auth(&endpoint.token)
            .json(&json!({"model":model_alias(&provider,&model),"input":input,"stream":stream}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 429, "{path}");
        assert_eq!(response.headers()["retry-after"], "1");
        assert!(
            response.headers()[CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/plain")
        );
    }
    router.stop().await.unwrap();
    task.abort();
}

#[tokio::test]
async fn native_chat_and_anthropic_stream_failures_keep_provider_classification() {
    for protocol in [
        UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
        UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
        UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
    ] {
        for (source, expected) in [
            ("insufficient_quota", "insufficient_quota"),
            ("rate_limit_error", "rate_limit_exceeded"),
            ("authentication_error", "invalid_prompt"),
        ] {
            let body = format!(
                "event: error\ndata: {}\n\n",
                json!({"type":"error","error":{"type":source,"message":"fixture stream failure sk-upstream"}})
            );
            let (url, task, _) = error_upstream(200, body, "text/event-stream").await;
            let (mut config, provider, model) = router_config(url);
            config.profiles[0].upstream_protocol = protocol.into();
            config.profiles[0].normalize();
            let router = LocalRouter::start(&config).await.unwrap();
            let endpoint = router.endpoint();
            let response = test_http_client_builder()
                .no_proxy()
                .build()
                .unwrap()
                .post(format!("{}/responses", endpoint.base_url))
                .bearer_auth(&endpoint.token)
                .json(&json!({"model":model_alias(&provider,&model),"input":"hello","stream":true}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), 200);
            let body = response.text().await.unwrap();
            let events = parse_responses_websocket_sse_events(&body).unwrap();
            let error = &events.last().unwrap()["response"]["error"];
            assert_eq!(error["code"], expected, "{protocol}: {body}");
            assert!(
                error["message"]
                    .as_str()
                    .unwrap()
                    .contains("fixture stream failure")
            );
            if protocol != UPSTREAM_PROTOCOL_OPENAI_RESPONSES {
                assert!(!body.contains("sk-upstream"));
            }
            router.stop().await.unwrap();
            task.abort();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires CODEY_NATIVE_CODEX_BIN; isolated native client retry contract"]
async fn native_codex_restarts_retry_budget_after_exhaustion_and_transport_fallback() {
    use tokio::io::{AsyncBufReadExt, BufReader};

    async fn rpc(
        input: &mut tokio::process::ChildStdin,
        output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
        id: u64,
        method: &str,
        params: Value,
    ) -> Value {
        input
            .write_all(format!("{}\n", json!({"id":id,"method":method,"params":params})).as_bytes())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let line = output
                    .next_line()
                    .await
                    .unwrap()
                    .expect("native client exited");
                let value: Value = serde_json::from_str(&line).unwrap();
                if value["id"] == id {
                    assert!(value.get("error").is_none(), "{value}");
                    return value["result"].clone();
                }
            }
        })
        .await
        .expect("native RPC timed out")
    }

    let binary = std::env::var_os("CODEY_NATIVE_CODEX_BIN")
        .expect("set CODEY_NATIVE_CODEX_BIN to a native Codex CLI");
    for websocket in [false, true] {
        for (status, code, retryable) in [
            (429, "rate_limit_exceeded", true),
            (503, "server_error", true),
            (429, "insufficient_quota", false),
            (401, "invalid_api_key", false),
        ] {
            let (url, task, requests) = error_upstream(
                status,
                json!({"error":{"code":code,"message":"isolated retry fixture"}}).to_string(),
                "application/json",
            )
            .await;
            let (config, provider, model) = router_config(url);
            let router = LocalRouter::start(&config).await.unwrap();
            let endpoint = router.endpoint();
            let model = model_alias(&provider, &model);
            let home = tempfile::tempdir().unwrap();
            let cwd = tempfile::tempdir().unwrap();
            let config = format!(
                "model = {model:?}\nmodel_provider = \"fixture\"\n[model_providers.fixture]\nname = \"fixture\"\nbase_url = {:?}\nwire_api = \"responses\"\nrequires_openai_auth = false\nsupports_websockets = {websocket}\nstream_max_retries = 1\nrequest_max_retries = 1\nstream_idle_timeout_ms = 5000\nhttp_headers = {{ Authorization = {:?} }}\n[analytics]\nenabled = false\n",
                endpoint.base_url,
                format!("Bearer {}", endpoint.token)
            );
            std::fs::write(home.path().join("config.toml"), config).unwrap();
            let mut command = tokio::process::Command::new(&binary);
            command
                .args(["app-server", "--stdio"])
                .current_dir(cwd.path())
                .env_clear()
                .env("CODEX_HOME", home.path())
                .env("HOME", home.path())
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            for name in ["PATH", "SYSTEMROOT", "TMPDIR", "TEMP", "TMP"] {
                if let Some(value) = std::env::var_os(name) {
                    command.env(name, value);
                }
            }
            let mut child = command.spawn().unwrap();
            let mut input = child.stdin.take().unwrap();
            let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
            rpc(&mut input,&mut output,1,"initialize",json!({"clientInfo":{"name":"codey_retry_contract","version":"1.0"},"capabilities":{"experimentalApi":true}})).await;
            input
                .write_all(b"{\"method\":\"initialized\"}\n")
                .await
                .unwrap();
            let thread = rpc(&mut input,&mut output,2,"thread/start",json!({"model":model,"modelProvider":"fixture","approvalPolicy":"never","sandbox":"read-only","cwd":cwd.path()})).await;
            let thread_id = thread["thread"]["id"].as_str().unwrap();
            for turn in 0..2 {
                let before = requests.lock().unwrap().len();
                let started = Instant::now();
                rpc(&mut input,&mut output,3+turn,"turn/start",json!({"threadId":thread_id,"input":[{"type":"text","text":"Return OK without tools","text_elements":[]}]})).await;
                let retries = tokio::time::timeout(Duration::from_secs(20), async {
                    let mut retries = Vec::new();
                    loop {
                        let line = output
                            .next_line()
                            .await
                            .unwrap()
                            .expect("native client exited during turn");
                        let event: Value = serde_json::from_str(&line).unwrap();
                        if event["method"] == "error" && event["params"]["willRetry"] == true {
                            retries.push(
                                event["params"]["error"]["message"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_owned(),
                            );
                        }
                        if event["method"] == "turn/completed" {
                            return retries;
                        }
                    }
                })
                .await
                .expect("native turn did not terminate");
                let requests = requests.lock().unwrap();
                let generated = requests[before..]
                    .iter()
                    .filter(|request| request["generate"] != false)
                    .count();
                let expected = if !retryable {
                    1
                } else if websocket && turn == 0 {
                    4
                } else {
                    2
                };
                assert_eq!(
                    generated, expected,
                    "ws={websocket} status={status} code={code} turn={turn} retries={retries:?}"
                );
                if retryable {
                    assert!(
                        !retries.is_empty(),
                        "new turn must receive a fresh retry budget"
                    );
                    assert!(
                        started.elapsed() >= Duration::from_millis(900),
                        "Retry-After was ignored"
                    );
                } else {
                    assert!(retries.is_empty(), "permanent failure retried: {retries:?}");
                }
                eprintln!(
                    "native retry contract: ws={websocket} status={status} code={code} turn={turn} requests={generated} retries={retries:?}"
                );
            }
            child.kill().await.unwrap();
            child.wait().await.unwrap();
            router.stop().await.unwrap();
            task.abort();
        }
    }
}
