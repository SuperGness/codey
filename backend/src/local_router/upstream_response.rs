use super::*;

#[derive(Debug)]
pub(crate) struct UpstreamReadIdleTimeout {
    pub(crate) operation: &'static str,
}

impl std::fmt::Display for UpstreamReadIdleTimeout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}超过读取空闲期限", self.operation)
    }
}

impl std::error::Error for UpstreamReadIdleTimeout {}

#[derive(Debug)]
pub(crate) struct UpstreamResponseDeadline;

impl std::fmt::Display for UpstreamResponseDeadline {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("上游响应超过总时限")
    }
}

impl std::error::Error for UpstreamResponseDeadline {}

/// 上游响应读取超时的统一判定：请求发送失败的超时、响应体总期限和每次读取
/// 的空闲期限都算同一种超时，调用方据此返回结构化 504。
pub(crate) fn is_upstream_timeout_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<reqwest::Error>()
        .is_some_and(reqwest::Error::is_timeout)
        || error.is::<UpstreamResponseDeadline>()
        || error.is::<UpstreamReadIdleTimeout>()
}

/// Deadline for reading one upstream response body. Success bodies are bounded
/// by this budget already; error bodies share it so an upstream that keeps
/// dribbling a few bytes cannot outlive the successful path.
pub(crate) fn upstream_response_body_deadline() -> tokio::time::Instant {
    tokio::time::Instant::now() + UPSTREAM_RESPONSE_TIMEOUT
}

pub(crate) struct PreparedUpstreamResponse {
    pub(crate) retry_advice: Option<ResponseRetryAdvice>,
    pub(crate) response: reqwest::Response,
    pub(crate) prefix: VecDeque<Bytes>,
    pub(crate) is_sse: bool,
    pub(crate) bytes_read: usize,
    pub(crate) retained: Option<RetainedMemoryBudget>,
    pub(crate) deadline: tokio::time::Instant,
}

pub(crate) async fn prepare_upstream_response(
    mut response: reqwest::Response,
    operation: &'static str,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<PreparedUpstreamResponse> {
    let retry_advice = ResponseRetryAdvice::from_headers(response.headers());
    let deadline = upstream_response_body_deadline();
    if let Some(probe) = probe {
        probe.set_upstream_response_headers(&super::responses::format_upstream_response_headers(
            response.headers(),
        ));
    }
    if response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(is_sse_content_type)
    {
        return Ok(PreparedUpstreamResponse {
            retry_advice,
            response,
            prefix: VecDeque::new(),
            is_sse: true,
            bytes_read: 0,
            retained: None,
            deadline,
        });
    }

    let mut prefix = VecDeque::new();
    let mut sniff = Vec::with_capacity(UPSTREAM_SSE_SNIFF_BYTES);
    loop {
        let Some(chunk) = tokio::time::timeout_at(
            deadline,
            read_upstream_chunk(&mut response, operation, probe),
        )
        .await
        .map_err(|_| anyhow::Error::new(UpstreamResponseDeadline))??
        else {
            return Ok(PreparedUpstreamResponse {
                retry_advice,
                response,
                prefix,
                is_sse: false,
                bytes_read: 0,
                retained: None,
                deadline,
            });
        };
        if chunk.is_empty() {
            continue;
        }
        let remaining = UPSTREAM_SSE_SNIFF_BYTES.saturating_sub(sniff.len());
        sniff.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        prefix.push_back(chunk);
        if let Some(is_sse) = classify_upstream_sse_prefix(&sniff) {
            return Ok(PreparedUpstreamResponse {
                retry_advice,
                response,
                prefix,
                is_sse,
                bytes_read: 0,
                retained: None,
                deadline,
            });
        }
        if sniff.len() == UPSTREAM_SSE_SNIFF_BYTES {
            return Ok(PreparedUpstreamResponse {
                retry_advice,
                response,
                prefix,
                is_sse: false,
                bytes_read: 0,
                retained: None,
                deadline,
            });
        }
    }
}

pub(crate) fn classify_upstream_sse_prefix(prefix: &[u8]) -> Option<bool> {
    const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";
    const SSE_PREFIXES: [&[u8]; 5] = [b"data:", b"event:", b"id:", b"retry:", b":"];
    let prefix = if prefix.starts_with(UTF8_BOM) {
        &prefix[UTF8_BOM.len()..]
    } else if UTF8_BOM.starts_with(prefix) {
        return None;
    } else {
        prefix
    };
    let prefix = &prefix[prefix
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(prefix.len())..];
    if prefix.is_empty() {
        return None;
    }
    if SSE_PREFIXES.iter().any(|marker| prefix.starts_with(marker)) {
        return Some(true);
    }
    if SSE_PREFIXES.iter().any(|marker| marker.starts_with(prefix)) {
        return None;
    }
    Some(false)
}

pub(crate) async fn read_prepared_upstream_chunk(
    prepared: &mut PreparedUpstreamResponse,
    operation: &'static str,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<Option<Bytes>> {
    if tokio::time::Instant::now() >= prepared.deadline {
        return Err(anyhow::Error::new(UpstreamResponseDeadline));
    }
    let chunk = if let Some(chunk) = prepared.prefix.pop_front() {
        Some(chunk)
    } else {
        tokio::time::timeout_at(
            prepared.deadline,
            read_upstream_chunk(&mut prepared.response, operation, probe),
        )
        .await
        .map_err(|_| anyhow::Error::new(UpstreamResponseDeadline))??
    };
    if let Some(chunk) = &chunk {
        let total = prepared.bytes_read.saturating_add(chunk.len());
        if total > MAX_UPSTREAM_RESPONSE_BYTES {
            anyhow::bail!("上游响应累计大小超过 Codey 安全上限");
        }
        if let Some(budget) = &mut prepared.retained {
            budget.resize(total)?;
        }
        prepared.bytes_read = total;
    }
    Ok(chunk)
}

pub(crate) async fn read_bounded_prepared_upstream_body(
    prepared: &mut PreparedUpstreamResponse,
    limit: usize,
    operation: &'static str,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<Vec<u8>> {
    prepared.retained.get_or_insert_with(Default::default);
    let mut body = Vec::new();
    while let Some(chunk) = read_prepared_upstream_chunk(prepared, operation, probe).await? {
        if body.len().saturating_add(chunk.len()) > limit {
            anyhow::bail!("{operation}超过 Codey 安全上限");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub(crate) async fn read_upstream_chunk(
    response: &mut reqwest::Response,
    operation: &'static str,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<Option<Bytes>> {
    let chunk = tokio::time::timeout(UPSTREAM_READ_IDLE_TIMEOUT, response.chunk())
        .await
        .map_err(|_| anyhow::Error::new(UpstreamReadIdleTimeout { operation }))?
        .with_context(|| operation)?;
    if chunk.as_ref().is_some_and(|chunk| !chunk.is_empty())
        && let Some(probe) = probe
    {
        probe.mark_first_upstream_data(FirstByteSource::UpstreamHttpBody);
    }
    Ok(chunk)
}

pub(crate) async fn write_all_with_timeout<W>(
    stream: &mut W,
    bytes: &[u8],
    operation: &'static str,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(DOWNSTREAM_WRITE_TIMEOUT, stream.write_all(bytes))
        .await
        .with_context(|| format!("{operation}超过写入期限"))?
        .with_context(|| operation)
        .context(DownstreamClosed)
}

/// SSE comments keep intermediaries alive and expose a closed reader without
/// treating a legal TCP write-half shutdown as cancellation.
pub(crate) async fn await_http_stream_upstream<T, F>(stream: &mut TcpStream, future: F) -> Result<T>
where
    F: std::future::Future<Output = T>,
{
    tokio::pin!(future);
    loop {
        tokio::select! {
            biased;
            result = &mut future => return Ok(result),
            _ = tokio::time::sleep(Duration::from_secs(15)) => {
                write_chunked_frame(stream, b": keep-alive\n\n", "写入 SSE 心跳失败").await?;
            }
        }
    }
}

/// Writes one chunked frame. The size line, payload and trailing CRLF go out
/// through a single `writev`, so a streamed event stays one syscall and the
/// payload is not copied into a second buffer. Separate `write` calls
/// previously produced three TCP segments per event.
pub(crate) async fn write_chunked_frame<W>(
    stream: &mut W,
    payload: &[u8],
    operation: &'static str,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut prefix = [0_u8; 18];
    let prefix_len = encode_chunk_size(&mut prefix, payload.len());
    let mut bufs = [
        std::io::IoSlice::new(&prefix[..prefix_len]),
        std::io::IoSlice::new(payload),
        std::io::IoSlice::new(b"\r\n"),
    ];
    tokio::time::timeout(
        DOWNSTREAM_WRITE_TIMEOUT,
        write_vectored_all(stream, &mut bufs),
    )
    .await
    .with_context(|| format!("{operation}超过写入期限"))?
    .with_context(|| operation)
    .context(DownstreamClosed)
}

async fn write_vectored_all<W>(
    stream: &mut W,
    bufs: &mut [std::io::IoSlice<'_>],
) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut bufs = bufs;
    while bufs.iter().any(|buf| !buf.is_empty()) {
        let written = stream.write_vectored(bufs).await?;
        if written == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "写入 chunked 帧时连接已关闭",
            ));
        }
        std::io::IoSlice::advance_slices(&mut bufs, written);
    }
    Ok(())
}

fn encode_chunk_size(prefix: &mut [u8; 18], len: usize) -> usize {
    let mut hex = [0_u8; 16];
    let digits = if len == 0 {
        hex[15] = b'0';
        1
    } else {
        let mut value = len;
        let mut digits = 0_usize;
        while value > 0 {
            let nibble = (value & 0xf) as u8;
            hex[15 - digits] = match nibble {
                0..=9 => b'0' + nibble,
                _ => b'a' + (nibble - 10),
            };
            value >>= 4;
            digits += 1;
        }
        digits
    };
    prefix[..digits].copy_from_slice(&hex[16 - digits..]);
    prefix[digits] = b'\r';
    prefix[digits + 1] = b'\n';
    digits + 2
}

pub(crate) async fn write_proxy_response(
    stream: &mut TcpStream,
    response: reqwest::Response,
    probe: Option<&RouteRequestLogProbe>,
    validate_responses: bool,
) -> Result<()> {
    let status = response.status().as_u16();
    let reason = reason_phrase(status);
    let retry_advice = ResponseRetryAdvice::from_headers(response.headers());
    let original_content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let mut forwarded_headers = String::new();
    for (name, value) in response.headers() {
        if !should_forward_upstream_response_header(name.as_str()) {
            continue;
        }
        let Ok(value) = value.to_str() else {
            continue;
        };
        forwarded_headers.push_str(name.as_str());
        forwarded_headers.push_str(": ");
        forwarded_headers.push_str(value);
        forwarded_headers.push_str("\r\n");
    }
    let mut prepared = prepare_upstream_response(response, "读取上游响应失败", probe).await?;
    let upstream_is_sse = prepared.is_sse;
    let mut terminal = (upstream_is_sse && validate_responses).then(NativeSseTerminal::default);
    let content_type = if upstream_is_sse {
        "text/event-stream"
    } else {
        original_content_type.as_str()
    };
    let mut log_tap = probe.map(|probe| RequestLogResponseTap::new(probe.clone()));
    write_all_with_timeout(
        stream,
        format!(
            "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ntransfer-encoding: chunked\r\n{forwarded_headers}{}connection: close\r\n\r\n",
            router_request_id_header()
        )
        .as_bytes(),
        "写入上游响应头失败",
    )
    .await?;
    if let Some(probe) = probe {
        probe.mark_response_started(status);
    }
    let response_fix = current_native_response_fix();
    let mut sse_rewriter = (upstream_is_sse && (validate_responses || response_fix.is_some()))
        .then(|| {
            ResponsesSseRewriter::new(
                response_fix.as_ref(),
                validate_responses,
                retry_advice.as_ref(),
            )
        });
    let mut buffered_json = response_fix
        .as_ref()
        .filter(|_| !upstream_is_sse)
        .map(|_| Vec::new());
    let mut finished = false;
    loop {
        let next = read_prepared_upstream_chunk(&mut prepared, "读取上游响应失败", probe);
        let chunk = if upstream_is_sse {
            await_http_stream_upstream(stream, next).await??
        } else {
            next.await?
        };
        let Some(chunk) = chunk else {
            break;
        };
        if chunk.is_empty() {
            continue;
        }
        if let Some(buffered) = buffered_json.as_mut() {
            if buffered.len().saturating_add(chunk.len()) > MAX_UPSTREAM_RESPONSE_BYTES {
                anyhow::bail!("上游响应超过上限");
            }
            buffered.extend_from_slice(&chunk);
            continue;
        }
        let rewritten;
        let output: &[u8] = if let Some(rewriter) = sse_rewriter.as_mut() {
            rewritten = rewriter.push(&chunk)?;
            if rewritten.is_empty() {
                continue;
            }
            &rewritten
        } else {
            &chunk
        };
        finished = match terminal.as_mut() {
            Some(terminal) => terminal.observe(output)?,
            None => false,
        };
        write_chunked_frame(stream, output, "写入上游响应块失败").await?;
        if let Some(tap) = log_tap.as_mut() {
            if sse_rewriter.is_some() {
                tap.observe(&Bytes::copy_from_slice(output));
            } else {
                tap.observe(&chunk);
            }
        }
        if !upstream_is_sse && let Some(probe) = probe {
            probe.mark_first_downstream_content();
        }
        if finished {
            break;
        }
    }
    if !finished && let Some(rewriter) = sse_rewriter.as_mut() {
        let tail = rewriter.finish();
        if !tail.is_empty() {
            if let Some(terminal) = terminal.as_mut() {
                terminal.observe(&tail)?;
            }
            write_chunked_frame(stream, &tail, "写入上游响应块失败").await?;
            if let Some(tap) = log_tap.as_mut() {
                tap.observe(&Bytes::copy_from_slice(&tail));
            }
        }
    }
    if let Some(buffered) = buffered_json {
        let output = response_fix
            .as_ref()
            .expect("JSON rewrite is only armed with a response fix")
            .rewrite_json_bytes(&buffered);
        if !output.is_empty() {
            write_chunked_frame(stream, &output, "写入上游响应块失败").await?;
            if let Some(tap) = log_tap.as_mut() {
                tap.observe(&Bytes::copy_from_slice(&output));
            }
            if let Some(probe) = probe {
                probe.mark_first_downstream_content();
            }
        }
    }
    if let Some(terminal) = terminal.as_mut() {
        terminal.finish()?;
    }
    if let Some(tap) = log_tap.as_mut() {
        tap.finish();
    }
    write_all_with_timeout(stream, b"0\r\n\r\n", "结束上游响应流失败").await?;
    Ok(())
}

pub(crate) const REQUEST_LOG_TAP_CHUNK_BYTES: usize = 8 * 1024;
pub(crate) const REQUEST_LOG_TAP_QUEUE_CHUNKS: usize = 64;
pub(crate) const REQUEST_LOG_USAGE_KEY_BYTES: usize = 64;
/// 完整投影的字符串上限。键都很短，模型名可能较长，因此比键上限宽松。
pub(crate) const REQUEST_LOG_METADATA_STRING_BYTES: usize = 256;
pub(crate) const REQUEST_LOG_USAGE_SCALAR_BYTES: usize = 64;
pub(crate) const REQUEST_LOG_USAGE_NESTING_DEPTH: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;

    async fn proxy_sse_for_test(
        body: &[u8],
        validate_responses: bool,
        retry_after: Option<&str>,
    ) -> (Vec<u8>, Result<()>) {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_address = upstream_listener.local_addr().unwrap();
        let body = body.to_vec();
        let retry_header = retry_after
            .map(|value| format!("retry-after: {value}\r\n"))
            .unwrap_or_default();
        let upstream_server = tokio::spawn(async move {
            let (mut stream, _) = upstream_listener.accept().await.unwrap();
            read_http_request(&mut stream).await.unwrap();
            let headers = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n{retry_header}connection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(headers.as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
        });
        let response = reqwest::Client::new()
            .get(format!("http://{upstream_address}/responses"))
            .send()
            .await
            .unwrap();
        let downstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let downstream_address = downstream_listener.local_addr().unwrap();
        let proxy = tokio::spawn(async move {
            let (mut stream, _) = downstream_listener.accept().await.unwrap();
            write_proxy_response(&mut stream, response, None, validate_responses).await
        });
        let mut stream = TcpStream::connect(downstream_address).await.unwrap();
        let mut wire = Vec::new();
        stream.read_to_end(&mut wire).await.unwrap();
        upstream_server.await.unwrap();
        let result = proxy.await.unwrap();
        let header_end = wire
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap()
            + 4;
        assert!(wire.starts_with(b"HTTP/1.1 200 OK\r\n"));
        let mut cursor = header_end;
        let mut output = Vec::new();
        while cursor < wire.len() {
            let prefix_len = wire[cursor..]
                .windows(2)
                .position(|bytes| bytes == b"\r\n")
                .unwrap();
            let length = usize::from_str_radix(
                std::str::from_utf8(&wire[cursor..cursor + prefix_len]).unwrap(),
                16,
            )
            .unwrap();
            cursor += prefix_len + 2;
            if length == 0 {
                break;
            }
            output.extend_from_slice(&wire[cursor..cursor + length]);
            cursor += length + 2;
        }
        (output, result)
    }

    #[tokio::test]
    async fn native_sse_proxy_normalizes_failures_without_changing_image_streams() {
        let body = b"id: failure\r\nevent: error\r\ndata: {\"type\":\"error\",\"error\":{\"code\":\"invalid_request_error\",\"message\":\"invalid model\"}}\r\n\r\n";
        let (output, result) = proxy_sse_for_test(body, true, None).await;
        result.unwrap();
        assert!(output.starts_with(b"id: failure\r\nevent: response.failed\r\n"));
        let mut cursor = SseCursor::default();
        let frame = take_next_sse_frame(&output, &mut cursor).unwrap();
        let event: Value = serde_json::from_str(&sse_frame_data(frame).unwrap().unwrap()).unwrap();
        assert_eq!(event["response"]["error"]["code"], "invalid_prompt");
        assert_eq!(
            event["response"]["error"]["codey"]["originalCode"],
            "invalid_request_error"
        );
        let (output, result) = proxy_sse_for_test(body, false, None).await;
        result.unwrap();
        assert_eq!(output, body);
    }

    #[tokio::test]
    async fn native_sse_proxy_preserves_normal_tail_and_rejects_missing_terminal() {
        let complete =
            b"\xef\xbb\xbfdata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp\"}}";
        let (output, result) = proxy_sse_for_test(complete, true, None).await;
        result.unwrap();
        assert_eq!(output, complete);
        let incomplete = b"data: {\"type\":\"response.created\"}\n\n";
        let (output, result) = proxy_sse_for_test(incomplete, true, None).await;
        assert_eq!(output, incomplete);
        assert!(result.unwrap_err().to_string().contains("终态事件前断开"));
    }

    #[tokio::test]
    async fn native_sse_proxy_preserves_retry_after_advice_for_stream_errors() {
        let body = b"event: error\ndata: {\"type\":\"error\",\"error\":{\"code\":\"server_error\",\"message\":\"busy\"}}\n\n";
        let (output, result) = proxy_sse_for_test(body, true, Some("2")).await;
        result.unwrap();
        let mut cursor = SseCursor::default();
        let frame = take_next_sse_frame(&output, &mut cursor).unwrap();
        let event: Value = serde_json::from_str(&sse_frame_data(frame).unwrap().unwrap()).unwrap();
        let error = &event["response"]["error"];
        assert_eq!(error["code"], "rate_limit_exceeded");
        assert!(error["codey"]["retryAfterMs"].as_u64().unwrap() <= 2000);
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .starts_with("Please try again in ")
        );
    }

    #[test]
    fn chunk_size_uses_lowercase_hex() {
        for len in [0_usize, 1, 15, 16, 255, 4096, 65535, 1 << 20] {
            let mut prefix = [0_u8; 18];
            let size = encode_chunk_size(&mut prefix, len);
            assert_eq!(&prefix[..size], format!("{len:x}\r\n").as_bytes());
        }
    }

    #[tokio::test]
    async fn chunked_frame_keeps_the_single_buffer_bytes() {
        let payload = b"data: {\"type\":\"response.created\"}\n\n";
        let mut output = Vec::new();
        write_chunked_frame(&mut output, payload, "test")
            .await
            .unwrap();
        let mut expected = format!("{:x}\r\n", payload.len()).into_bytes();
        expected.extend_from_slice(payload);
        expected.extend_from_slice(b"\r\n");
        assert_eq!(output, expected);
    }
}
