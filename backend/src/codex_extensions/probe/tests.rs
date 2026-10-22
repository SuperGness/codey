use super::*;
use tokio::{io::AsyncReadExt, net::TcpListener, task::JoinHandle};

fn handshake_message(tools: bool) -> Value {
    json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":if tools {json!({"tools":{}})} else {json!({})},"serverInfo":{"name":"fixture","version":"1"}}})
}

fn response(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}",
        body.len()
    )
}

fn sse_response(body: &[u8], chunk_size: usize) -> Response {
    // An in-memory body preserves chunk boundaries without TCP coalescing.
    let chunks = body
        .chunks(chunk_size)
        .map(|chunk| Ok::<_, std::io::Error>(chunk.to_vec()))
        .collect::<Vec<_>>();
    tokio_tungstenite::tungstenite::http::Response::builder()
        .header("content-type", "text/event-stream")
        .body(reqwest::Body::wrap_stream(futures_util::stream::iter(
            chunks,
        )))
        .unwrap()
        .into()
}

async fn server(responses: Vec<String>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for reply in responses {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut data = Vec::new();
            let mut buffer = [0u8; 1024];
            loop {
                let count = stream.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                data.extend_from_slice(&buffer[..count]);
                assert!(data.len() < MAX_BODY);
                if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .map_or(0, |n| n.trim().parse::<usize>().unwrap());
                    if data.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(data).unwrap());
            stream.write_all(reply.as_bytes()).await.unwrap();
        }
        requests
    });
    (url, task)
}

#[tokio::test]
async fn streamable_http_handshake_negotiates_and_closes_session() {
    let tools = json!({"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"fixture","inputSchema":{"type":"object"}}]}});
    let (url, server) = server(vec![
        response(
            "200 OK",
            "Content-Type: application/json\r\nMcp-Session-Id: fixture-session\r\n",
            &handshake_message(true).to_string(),
        ),
        response("202 Accepted", "", ""),
        response(
            "200 OK",
            "Content-Type: application/json\r\n",
            &tools.to_string(),
        ),
        response("204 No Content", "", ""),
    ])
    .await;
    let outcome = test_mcp(json!({"url":url}), None).await;
    assert_eq!(outcome["ok"], true, "{outcome}");
    assert_eq!(outcome["protocolVersion"], "2025-03-26");
    assert_eq!(outcome["serverInfo"]["version"], "1");
    let requests = server.await.unwrap();
    assert!(requests[0].contains("initialize"));
    assert!(requests[1].contains("notifications/initialized"));
    assert!(
        requests[1]
            .to_ascii_lowercase()
            .contains("mcp-protocol-version: 2025-03-26")
    );
    assert!(
        requests[2]
            .to_ascii_lowercase()
            .contains("mcp-session-id: fixture-session")
    );
    assert!(requests[2].contains("tools/list"));
    assert!(requests[3].starts_with("DELETE "));
}

#[tokio::test]
async fn empty_200_notification_and_existing_bearer_are_supported() {
    let (url, server) = server(vec![
        response(
            "200 OK",
            "Content-Type: application/json\r\n",
            &handshake_message(false).to_string(),
        ),
        response("200 OK", "", ""),
    ])
    .await;
    let result = test_mcp(json!({"url":url,"bearer_token":"private-test-token"}), None).await;
    assert_eq!(result["ok"], true, "{result}");
    assert!(!result.to_string().contains("private-test-token"));
    assert!(server.await.unwrap()[0].contains("Bearer private-test-token"));
}

#[tokio::test]
async fn notification_response_body_is_rejected_without_reflecting_contents() {
    let (url, server) = server(vec![
        response(
            "200 OK",
            "Content-Type: application/json\r\n",
            &handshake_message(false).to_string(),
        ),
        response("200 OK", "", "private-response"),
    ])
    .await;
    let result = test_mcp(json!({"url":url}), None).await;
    assert_eq!(result["ok"], false);
    assert!(!result.to_string().contains("private-response"));
    server.await.unwrap();
}

#[tokio::test]
async fn sse_supports_comments_crlf_notifications_and_multiline_data() {
    let handshake = handshake_message(false)
        .to_string()
        .replace(",\"result\"", ",\r\ndata: \"result\"");
    let body = format!(
        ": heartbeat\r\n\r\ndata: {{\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}}\r\n\r\nevent: message\r\ndata: {handshake}\r\n\r\n"
    );
    let (url, server) = server(vec![
        response("200 OK", "Content-Type: text/event-stream\r\n", &body),
        response("202 Accepted", "", ""),
    ])
    .await;
    let outcome = test_mcp(json!({"url":url}), None).await;
    assert_eq!(outcome["ok"], true, "{outcome}");
    assert_eq!(server.await.unwrap().len(), 2);
}

#[tokio::test]
async fn sse_accepts_a_long_utf8_line_in_small_chunks() {
    let expected = json!({"text": "汉字🚀".repeat(4096)});
    let message = json!({"jsonrpc":"2.0", "id":1, "result":expected});
    let body = format!("data: {message}\n\n");
    assert_eq!(
        http_response(sse_response(body.as_bytes(), 7), 1)
            .await
            .unwrap(),
        expected
    );
}

#[tokio::test]
async fn sse_handles_messages_crlf_and_multiline_data_across_chunks() {
    let body = concat!(
        ": heartbeat\r\n\r\n",
        "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}\r\n\r\n",
        "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}\r\n\r\n",
        "event: message\r\n",
        "data: {\"jsonrpc\":\"2.0\",\r\n",
        "data: \"id\":1,\"result\":{\"text\":\"分块🚀\"}}\r\n\r\n",
    );
    for chunk_size in [1, 2, 3, 7, body.len()] {
        assert_eq!(
            http_response(sse_response(body.as_bytes(), chunk_size), 1)
                .await
                .unwrap(),
            json!({"text":"分块🚀"}),
            "chunk size {chunk_size}"
        );
    }
}

#[tokio::test]
async fn sse_does_not_dispatch_an_unterminated_event_at_eof() {
    for ending in ["", "\n", "\r\n", "\r\n\r"] {
        let body = format!("data: {{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{}}}}{ending}");
        assert_eq!(
            http_response(sse_response(body.as_bytes(), 7), 1)
                .await
                .unwrap_err()
                .to_string(),
            "SSE 流在返回匹配的响应前结束"
        );
    }
}

#[tokio::test]
async fn sse_message_limit_includes_the_matching_response_across_chunks() {
    let note = "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}\n\n";
    for notifications in [MAX_MESSAGES - 1, MAX_MESSAGES] {
        let body = format!(
            "{}data: {{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{}}}}\n\n",
            note.repeat(notifications)
        );
        let outcome = http_response(sse_response(body.as_bytes(), 31), 1).await;
        if notifications < MAX_MESSAGES {
            assert_eq!(outcome.unwrap(), json!({}));
        } else {
            assert_eq!(outcome.unwrap_err().to_string(), "SSE 消息数量超过检查上限");
        }
    }
}

#[tokio::test]
async fn sse_body_limit_includes_fragmented_comments() {
    let event = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n";
    for total in [MAX_BODY, MAX_BODY + 1] {
        let body = format!(":{}\n{event}", "x".repeat(total - event.len() - 2));
        let outcome = http_response(sse_response(body.as_bytes(), 257), 1).await;
        if total == MAX_BODY {
            assert_eq!(outcome.unwrap(), json!({}));
        } else {
            assert_eq!(
                outcome.unwrap_err().to_string(),
                "HTTP 响应超过 1 MiB 检查上限"
            );
        }
    }
}

#[tokio::test]
async fn malformed_and_rpc_errors_do_not_leak_response_contents() {
    for body in [
        "secret-value-invalid-json".to_owned(),
        json!({"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"secret-value"}}).to_string(),
        json!({"jsonrpc":"2.0","id":4,"result":{}}).to_string(),
    ] {
        let (url, server) = server(vec![response(
            "200 OK",
            "Content-Type: application/json\r\n",
            &body,
        )])
        .await;
        let outcome = test_mcp(json!({"url":url}), None).await;
        assert_eq!(outcome["ok"], false);
        assert!(!outcome.to_string().contains("secret-value"));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn unknown_protocol_and_invalid_result_are_rejected() {
    for result in [
        json!({"protocolVersion":"9999-01-01","capabilities":{},"serverInfo":{"name":"fixture","version":"1"}}),
        json!(true),
        json!({"protocolVersion":"2025-03-26","capabilities":{"tools":true},"serverInfo":{"name":"fixture","version":"1"}}),
    ] {
        let body = json!({"jsonrpc":"2.0","id":1,"result":result});
        let (url, server) = server(vec![response(
            "200 OK",
            "Content-Type: application/json\r\n",
            &body.to_string(),
        )])
        .await;
        assert_eq!(test_mcp(json!({"url":url}), None).await["ok"], false);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn redirects_and_authentication_are_reported_without_forwarding() {
    for code in ["302 Found", "401 Unauthorized", "403 Forbidden"] {
        let (url, server) = server(vec![response(
            code,
            "Location: http://127.0.0.1:1/forbidden\r\n",
            "secret-value",
        )])
        .await;
        let outcome = test_mcp(
            json!({"url":url,"http_headers":{"Authorization":"fixture-only"}}),
            None,
        )
        .await;
        assert_eq!(outcome["ok"], false);
        assert!(!outcome.to_string().contains("secret-value"));
        assert_eq!(server.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn unsuccessful_cleanup_is_not_reported_as_success() {
    let (url, server) = server(vec![
        response(
            "200 OK",
            "Content-Type: application/json\r\nMcp-Session-Id: fixture-session\r\n",
            &handshake_message(false).to_string(),
        ),
        response("202 Accepted", "", ""),
        response("405 Method Not Allowed", "", ""),
    ])
    .await;
    let outcome = test_mcp(json!({"url":url}), None).await;
    assert_eq!(outcome["ok"], false);
    assert!(outcome.to_string().contains("会话"));
    server.await.unwrap();
}

#[tokio::test]
async fn notification_flood_and_oversized_lines_are_bounded() {
    let note = "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}\n";
    let notes = note.repeat(MAX_MESSAGES + 1);
    let mut reader = BufReader::new(notes.as_bytes());
    assert!(
        stdio_response(&mut reader, 1)
            .await
            .unwrap_err()
            .to_string()
            .contains("过多")
    );
    let huge = vec![b'x'; MAX_BODY + 1];
    assert!(
        bounded_line(&mut BufReader::new(huge.as_slice()), &mut Vec::new())
            .await
            .is_err()
    );
    let data = format!("data: {}\n", note.trim_end())
        .repeat(MAX_MESSAGES + 1)
        .replace("}\n", "}\n\n");
    let (url, server) = server(vec![response(
        "200 OK",
        "Content-Type: text/event-stream\r\n",
        &data,
    )])
    .await;
    assert_eq!(test_mcp(json!({"url":url}), None).await["ok"], false);
    server.await.unwrap();
}

#[test]
fn unsafe_http_configuration_is_rejected() {
    for config in [
        json!({"url":"http://example.com/mcp"}),
        json!({"url":"https://user:secret@example.com/mcp"}),
        json!({"url":"https://example.com/mcp","http_headers":{"Host":"elsewhere"}}),
        json!({"url":"https://example.com/mcp","env_http_headers":{"Authorization":"not a variable"}}),
    ] {
        assert!(http_settings(&config).is_err());
    }
    assert!(http_settings(&json!({"url":"http://[::1]/mcp"})).is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_handshake_passes_arguments_literally() {
    let fixture = tempfile::tempdir().unwrap();
    let marker = fixture.path().join("must-not-exist");
    let argument = format!("$(touch {}) ; touch {}", marker.display(), marker.display());
    let script = format!(
        "read -r line\nprintf '%s\\n' '{}'\nread -r line\nread -r line\nprintf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"tools\":[]}}}}'\n",
        handshake_message(true)
    );
    let outcome = test_mcp(
        json!({"command":"/bin/sh","args":["-c",script,"fixture",argument]}),
        Some(fixture.path().into()),
    )
    .await;
    assert_eq!(outcome["ok"], true, "{outcome}");
    assert!(!marker.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_kills_owned_descendant_process_group() {
    let fixture = tempfile::tempdir().unwrap();
    let pid_file = fixture.path().join("pid");
    let config = json!({"command":"/bin/sh","args":["-c","sleep 60 & echo $$ $! > \"$1\"; wait","fixture",pid_file]});
    let task = tokio::spawn(async move { probe_stdio(&config, None).await });
    tokio::time::timeout(Duration::from_secs(3), async {
        while std::fs::read_to_string(&pid_file)
            .unwrap_or_default()
            .split_whitespace()
            .count()
            != 2
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pids: Vec<i32> = std::fs::read_to_string(pid_file)
        .unwrap()
        .split_whitespace()
        .map(|pid| pid.parse().unwrap())
        .collect();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(3), async {
        while pids.iter().any(|pid| unsafe { libc::kill(*pid, 0) } == 0) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("owned descendant should terminate and the main process should be reaped");
}

#[tokio::test]
async fn stalled_transport_is_cancellable() {
    let (client, _server) = tokio::io::duplex(64);
    let mut reader = BufReader::new(client);
    assert!(
        tokio::time::timeout(Duration::from_millis(25), stdio_response(&mut reader, 1))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancelled_http_session_attempts_delete() {
    let (url, server) = server(vec![response("204 No Content", "", "")]).await;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _session = HttpSession {
            cleanup: Some((Client::new(), Url::parse(&url).unwrap(), HeaderMap::new())),
        };
        ready_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    ready_rx.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let requests = server.await.unwrap();
    assert!(requests[0].starts_with("DELETE "));
}
