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

pub(crate) struct PreparedUpstreamResponse {
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
    let deadline = tokio::time::Instant::now() + UPSTREAM_RESPONSE_TIMEOUT;
    if response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(is_sse_content_type)
    {
        return Ok(PreparedUpstreamResponse {
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
        .context("上游响应超过总时限")??
        else {
            return Ok(PreparedUpstreamResponse {
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
        anyhow::bail!("上游响应超过总时限");
    }
    let chunk = if let Some(chunk) = prepared.prefix.pop_front() {
        Some(chunk)
    } else {
        tokio::time::timeout_at(
            prepared.deadline,
            read_upstream_chunk(&mut prepared.response, operation, probe),
        )
        .await
        .context("上游响应超过总时限")??
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

/// Writes one `transfer-encoding: chunked` frame with a single `write_all`.
/// Emitting the size line, payload and trailing CRLF as three separate writes
/// produced three small TCP segments for every streamed event.
pub(crate) async fn write_chunked_frame<W>(
    stream: &mut W,
    payload: &[u8],
    operation: &'static str,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut frame = Vec::with_capacity(payload.len() + 16);
    frame.extend_from_slice(format!("{:x}\r\n", payload.len()).as_bytes());
    frame.extend_from_slice(payload);
    frame.extend_from_slice(b"\r\n");
    write_all_with_timeout(stream, &frame, operation).await
}

pub(crate) async fn write_proxy_response(
    stream: &mut TcpStream,
    response: reqwest::Response,
    probe: Option<&RouteRequestLogProbe>,
    validate_responses: bool,
) -> Result<()> {
    let status = response.status().as_u16();
    let reason = reason_phrase(status);
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
        let finished = match terminal.as_mut() {
            Some(terminal) => terminal.observe(&chunk)?,
            None => false,
        };
        write_chunked_frame(stream, &chunk, "写入上游响应块失败").await?;
        if let Some(tap) = log_tap.as_mut() {
            tap.observe(&chunk);
        }
        if !upstream_is_sse && let Some(probe) = probe {
            probe.mark_first_downstream_content();
        }
        if finished {
            break;
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
pub(crate) const REQUEST_LOG_USAGE_SCALAR_BYTES: usize = 64;
pub(crate) const REQUEST_LOG_USAGE_NESTING_DEPTH: usize = 64;
