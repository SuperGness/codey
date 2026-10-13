use super::*;

/// Protocol-specific accumulation of one SSE `data:` payload. The frame
/// splitting, buffering and tail handling are shared by [`collect_sse_frames`]
/// and [`parse_sse_frames`]; only the payload interpretation differs between
/// Anthropic Messages and Chat Completions upstreams.
pub(crate) trait SseFrameAccumulator {
    const PROTOCOL_LABEL: &'static str;
    const READ_OPERATION: &'static str;
    const MAX_BUFFER_BYTES: usize = MAX_UPSTREAM_SSE_BUFFER_BYTES;
    fn ingest_frame(&mut self, data: &str, trailing: bool) -> Result<()>;
    fn finished(&self) -> bool;
}

pub(crate) fn sse_json_frame(data: &str, protocol: &str, trailing: bool) -> Result<Value> {
    serde_json::from_str::<Value>(data).with_context(|| {
        if trailing {
            format!("{protocol} SSE 末尾 data 不是有效 JSON")
        } else {
            format!("{protocol} SSE data 不是有效 JSON")
        }
    })
}

impl SseFrameAccumulator for AnthropicSseAccumulator {
    const PROTOCOL_LABEL: &'static str = "Anthropic Messages";
    const READ_OPERATION: &'static str = "读取 Anthropic Messages SSE 流失败";

    fn ingest_frame(&mut self, data: &str, trailing: bool) -> Result<()> {
        self.ingest(&sse_json_frame(data, Self::PROTOCOL_LABEL, trailing)?)
    }

    fn finished(&self) -> bool {
        self.stopped
    }
}

impl SseFrameAccumulator for ChatSseAccumulator {
    const PROTOCOL_LABEL: &'static str = "Chat Completions";
    const READ_OPERATION: &'static str = "读取 Chat Completions SSE 流失败";

    fn ingest_frame(&mut self, data: &str, trailing: bool) -> Result<()> {
        if data.trim() == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        self.ingest(&sse_json_frame(data, Self::PROTOCOL_LABEL, trailing)?)
    }

    fn finished(&self) -> bool {
        self.done
    }
}

/// Drain a buffered (non-streaming to the client) upstream SSE body into the
/// accumulator, stopping at the protocol's terminal frame.
pub(crate) async fn collect_sse_frames<A: SseFrameAccumulator>(
    prepared: &mut PreparedUpstreamResponse,
    accumulator: &mut A,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<()> {
    prepared.retained.get_or_insert_with(Default::default);
    let mut buffer = Vec::new();
    let mut cursor = SseCursor::default();
    while let Some(chunk) = read_prepared_upstream_chunk(prepared, A::READ_OPERATION, probe).await?
    {
        compact_sse_buffer(&mut buffer, &mut cursor);
        buffer.extend_from_slice(&chunk);
        if buffer.len().saturating_sub(cursor.consumed) > A::MAX_BUFFER_BYTES {
            anyhow::bail!("上游 SSE 单帧超过 Codey 安全上限");
        }
        while let Some(frame) = take_next_sse_frame(&buffer, &mut cursor) {
            let Some(data) = sse_frame_data(frame)? else {
                continue;
            };
            accumulator.ingest_frame(&data, false)?;
            if accumulator.finished() {
                break;
            }
        }
        if accumulator.finished() {
            break;
        }
    }
    if !accumulator.finished()
        && !buffer[cursor.consumed..]
            .iter()
            .all(u8::is_ascii_whitespace)
        && let Some(data) = sse_frame_data(&buffer[cursor.consumed..])?
    {
        accumulator.ingest_frame(&data, true)?;
    }
    Ok(())
}

/// Parse an already fully-read SSE body.
pub(crate) fn parse_sse_frames<A: SseFrameAccumulator>(
    bytes: &[u8],
    accumulator: &mut A,
) -> Result<()> {
    if bytes.len() > MAX_UPSTREAM_RESPONSE_BYTES {
        anyhow::bail!("上游响应累计大小超过 Codey 安全上限");
    }
    let mut cursor = SseCursor::default();
    while let Some(frame) = take_next_sse_frame(bytes, &mut cursor) {
        let Some(data) = sse_frame_data(frame)? else {
            continue;
        };
        accumulator.ingest_frame(&data, false)?;
        if accumulator.finished() {
            return Ok(());
        }
    }
    if !bytes[cursor.consumed..].iter().all(u8::is_ascii_whitespace)
        && let Some(data) = sse_frame_data(&bytes[cursor.consumed..])?
    {
        accumulator.ingest_frame(&data, true)?;
    }
    Ok(())
}

/// Validate native SSE completion while forwarding the original bytes. Only
/// one event is retained; unknown JSON fields are skipped without building a tree.
#[derive(Default)]
pub(crate) struct NativeSseTerminal {
    buffer: Vec<u8>,
    cursor: SseCursor,
    terminal: bool,
    budget: RetainedMemoryBudget,
}

impl NativeSseTerminal {
    fn ingest(&mut self, frame: &[u8]) -> Result<()> {
        #[derive(serde::Deserialize)]
        struct EventKind {
            #[serde(rename = "type")]
            kind: String,
        }
        if let Some(data) = sse_frame_data(frame)? {
            if data.trim() == "[DONE]" {
                if !self.terminal {
                    anyhow::bail!("Responses SSE 在终态事件前结束");
                }
                return Ok(());
            }
            let event: EventKind =
                serde_json::from_str(&data).context("Responses SSE data 无效")?;
            self.terminal |= matches!(
                event.kind.as_str(),
                "response.completed" | "response.failed" | "response.incomplete" | "error"
            );
        }
        Ok(())
    }

    pub(crate) fn observe(&mut self, bytes: &[u8]) -> Result<bool> {
        compact_sse_buffer(&mut self.buffer, &mut self.cursor);
        let size = self.buffer.len().saturating_add(bytes.len());
        if size > MAX_UPSTREAM_RESPONSE_BYTES {
            anyhow::bail!("Responses SSE 单帧超过上限");
        }
        // Clearing consumed bytes does not release Vec capacity.
        self.budget.resize(size.max(self.buffer.capacity()))?;
        self.buffer.extend_from_slice(bytes);
        // Split borrows: the frame is borrowed from the buffer, while ingest
        // only needs the terminal flag. Move the bounded buffer temporarily.
        let buffer = std::mem::take(&mut self.buffer);
        let result = (|| {
            while let Some(frame) = take_next_sse_frame(&buffer, &mut self.cursor) {
                self.ingest(frame)?;
                if self.terminal {
                    break;
                }
            }
            Ok(self.terminal)
        })();
        self.buffer = buffer;
        result
    }

    pub(crate) fn finish(&mut self) -> Result<()> {
        if !self.terminal {
            let buffer = std::mem::take(&mut self.buffer);
            let tail = &buffer[self.cursor.consumed..];
            if !tail.iter().all(u8::is_ascii_whitespace) {
                self.ingest(tail)?;
            }
        }
        if !self.terminal {
            anyhow::bail!("Responses SSE 在终态事件前断开");
        }
        Ok(())
    }
}

pub(crate) struct ResponsesSseRewriter<'a> {
    response_fix: Option<&'a NativeResponseFix>,
    normalize_failures: bool,
    retry_advice: Option<&'a ResponseRetryAdvice>,
    buffer: Vec<u8>,
    cursor: SseCursor,
    forwarded: usize,
    budget: RetainedMemoryBudget,
}

impl<'a> ResponsesSseRewriter<'a> {
    pub(crate) fn new(
        response_fix: Option<&'a NativeResponseFix>,
        normalize_failures: bool,
        retry_advice: Option<&'a ResponseRetryAdvice>,
    ) -> Self {
        Self {
            response_fix,
            normalize_failures,
            retry_advice,
            buffer: Vec::new(),
            cursor: SseCursor::default(),
            forwarded: 0,
            budget: RetainedMemoryBudget::default(),
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) -> Result<Vec<u8>> {
        let size = self.buffer.len().saturating_add(chunk.len());
        if size > MAX_UPSTREAM_RESPONSE_BYTES {
            anyhow::bail!("Responses SSE 单帧超过上限");
        }
        self.budget.resize(size.max(self.buffer.capacity()))?;
        self.buffer.extend_from_slice(chunk);
        let mut output = Vec::new();
        while let Some(frame) = take_next_sse_frame(&self.buffer, &mut self.cursor) {
            output.extend(
                self.rewrite_frame(frame, &self.buffer[self.forwarded..self.cursor.consumed]),
            );
            self.forwarded = self.cursor.consumed;
        }
        if self.forwarded > 0 {
            let consumed = self.cursor.consumed;
            compact_sse_buffer(&mut self.buffer, &mut self.cursor);
            self.forwarded -= consumed - self.cursor.consumed;
        }
        Ok(output)
    }

    pub(crate) fn finish(&mut self) -> Vec<u8> {
        let buffer = std::mem::take(&mut self.buffer);
        let output = self.rewrite_frame(&buffer[self.cursor.consumed..], &buffer[self.forwarded..]);
        self.cursor = SseCursor::default();
        self.forwarded = 0;
        output
    }

    fn rewrite_frame(&self, frame: &[u8], original: &[u8]) -> Vec<u8> {
        #[derive(serde::Deserialize)]
        struct EventKind<'a> {
            #[serde(rename = "type", borrow)]
            kind: Cow<'a, str>,
        }
        let passthrough = || original.to_vec();
        let Ok(Some(data)) = sse_frame_data(frame) else {
            return passthrough();
        };
        if data.trim() == "[DONE]" {
            return passthrough();
        }
        if self.response_fix.is_none() {
            if !self.normalize_failures {
                return passthrough();
            }
            let Ok(event) = serde_json::from_str::<EventKind<'_>>(&data) else {
                return passthrough();
            };
            if !matches!(event.kind.as_ref(), "error" | "response.failed") {
                return passthrough();
            }
        }
        let Ok(mut event) = serde_json::from_str::<Value>(&data) else {
            return passthrough();
        };
        let mut changed =
            self.normalize_failures && normalize_response_failure(&mut event, self.retry_advice);
        if let Some(fix) = self.response_fix {
            changed |= fix.apply(&mut event);
        }
        let Ok(text) = std::str::from_utf8(frame) else {
            return passthrough();
        };
        let failure = self.normalize_failures
            && event.get("type").and_then(Value::as_str) == Some("response.failed");
        changed |= failure
            && text.lines().any(|line| {
                line.strip_prefix("event:")
                    .is_some_and(|kind| kind.trim() != "response.failed")
            });
        if !changed {
            return passthrough();
        }
        let Ok(json) = serde_json::to_string(&event) else {
            return passthrough();
        };
        let newline: &[u8] = if original.windows(2).any(|bytes| bytes == b"\r\n") {
            b"\r\n"
        } else {
            b"\n"
        };
        let mut output = Vec::new();
        if original.starts_with(b"\xef\xbb\xbf") {
            output.extend_from_slice(b"\xef\xbb\xbf");
        }
        let mut wrote_data = false;
        for line in text.split_inclusive('\n') {
            let field = line.trim_end_matches(['\r', '\n']);
            if field.starts_with("data:") {
                if !wrote_data {
                    output.extend_from_slice(b"data: ");
                    output.extend_from_slice(json.as_bytes());
                    output.extend_from_slice(newline);
                    wrote_data = true;
                }
            } else if failure && field.starts_with("event:") {
                output.extend_from_slice(b"event: response.failed");
                output.extend_from_slice(newline);
            } else {
                output.extend_from_slice(line.as_bytes());
                if !line.ends_with('\n') {
                    output.extend_from_slice(newline);
                }
            }
        }
        output.extend_from_slice(newline);
        output
    }
}

#[derive(Default)]
pub(crate) struct SseCursor {
    pub(crate) consumed: usize,
    pub(crate) scanned: usize,
    started: bool,
}

pub(crate) fn take_next_sse_frame<'a>(
    buffer: &'a [u8],
    cursor: &mut SseCursor,
) -> Option<&'a [u8]> {
    if !cursor.started {
        const BOM: &[u8] = b"\xef\xbb\xbf";
        if buffer.len() < BOM.len() && BOM.starts_with(buffer) {
            return None;
        }
        cursor.started = true;
        if buffer.starts_with(BOM) {
            cursor.consumed = BOM.len();
            cursor.scanned = BOM.len();
        }
    }
    // One SIMD scan for the next newline. `\n\n` and `\r\n\r\n` are the only
    // frame delimiters, and the latter does not contain the former.
    let mut index = cursor.scanned;
    while index < buffer.len() {
        let Some(relative) = memchr::memchr(b'\n', &buffer[index..]) else {
            break;
        };
        let newline = index + relative;
        if buffer.get(newline + 1) == Some(&b'\n') {
            let frame = &buffer[cursor.consumed..newline];
            cursor.consumed = newline + 2;
            cursor.scanned = cursor.consumed;
            return Some(frame);
        }
        if newline > 0
            && buffer[newline - 1] == b'\r'
            && buffer.get(newline + 1..newline + 3) == Some(b"\r\n")
        {
            let start = newline - 1;
            if start >= cursor.consumed {
                let frame = &buffer[cursor.consumed..start];
                cursor.consumed = start + 4;
                cursor.scanned = cursor.consumed;
                return Some(frame);
            }
        }
        index = newline + 1;
    }
    // Revisit only the suffix that can begin a delimiter split across chunks.
    cursor.scanned = buffer.len().saturating_sub(3).max(cursor.consumed);
    None
}

pub(crate) fn compact_sse_buffer(buffer: &mut Vec<u8>, cursor: &mut SseCursor) {
    if cursor.consumed == 0 {
        return;
    }
    if cursor.consumed == buffer.len() {
        buffer.clear();
        cursor.consumed = 0;
        cursor.scanned = 0;
        return;
    }
    if cursor.consumed >= 64 * 1024 || cursor.consumed.saturating_mul(2) >= buffer.len() {
        buffer.drain(..cursor.consumed);
        cursor.scanned -= cursor.consumed;
        cursor.consumed = 0;
    }
}

pub(crate) fn sse_frame_data(frame: &[u8]) -> Result<Option<Cow<'_, str>>> {
    let frame = std::str::from_utf8(frame).context("上游 SSE 不是 UTF-8")?;
    let mut data = frame
        .lines()
        .filter_map(|line| line.trim_end_matches('\r').strip_prefix("data:"))
        .map(str::trim_start);
    let Some(first) = data.next() else {
        return Ok(None);
    };
    let Some(second) = data.next() else {
        return Ok(Some(Cow::Borrowed(first)));
    };
    let mut joined = String::with_capacity(first.len() + second.len() + 1);
    joined.push_str(first);
    joined.push('\n');
    joined.push_str(second);
    for line in data {
        joined.push('\n');
        joined.push_str(line);
    }
    Ok(Some(Cow::Owned(joined)))
}

pub(crate) fn current_unix_timestamp() -> i64 {
    crate::fs_util::timestamp_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewrite_chunks(input: &[u8], chunk_size: usize, normalize: bool) -> Vec<u8> {
        let mut rewriter = ResponsesSseRewriter::new(None, normalize, None);
        let mut output = Vec::new();
        for chunk in input.chunks(chunk_size) {
            output.extend(rewriter.push(chunk).unwrap());
        }
        output.extend(rewriter.finish());
        output
    }

    fn rewritten_event(bytes: &[u8]) -> Value {
        let mut cursor = SseCursor::default();
        let frame = take_next_sse_frame(bytes, &mut cursor).unwrap();
        serde_json::from_str(&sse_frame_data(frame).unwrap().unwrap()).unwrap()
    }

    #[test]
    fn responses_rewriter_preserves_normal_frame_bytes_and_eof_tail() {
        let input = b"\xef\xbb\xbf: keep-alive\r\n\r\nid: one\r\nevent: response.created\r\ndata: {\"type\":\"response.created\",\r\ndata: \"response\":{\"id\":\"resp\"}}\r\n\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\ndata: {\"type\":\"response.completed\"}\n\n \r\n";
        for chunk_size in 1..=input.len() {
            assert_eq!(
                rewrite_chunks(input, chunk_size, true),
                input,
                "chunk size {chunk_size}"
            );
        }
        let tail = b"\xef\xbb\xbfdata: {\"type\":\"response.completed\"}";
        for chunk_size in 1..=tail.len() {
            assert_eq!(
                rewrite_chunks(tail, chunk_size, true),
                tail,
                "tail chunk size {chunk_size}"
            );
        }
    }

    #[test]
    fn responses_rewriter_normalizes_bare_errors_and_preserves_sse_fields() {
        let input = b"\xef\xbb\xbfid: last\r\nevent: error\r\n: diagnostic\r\nretry: 2000\r\ndata: {\"type\":\"error\",\r\ndata: \"error\":{\"code\":\"authentication_error\",\"message\":\"bad key\"}}\r\n\r\n";
        for chunk_size in 1..=input.len() {
            let output = rewrite_chunks(input, chunk_size, true);
            let text = std::str::from_utf8(&output).unwrap();
            assert!(output.starts_with(b"\xef\xbb\xbfid: last\r\n"));
            assert!(text.contains("event: response.failed\r\n: diagnostic\r\nretry: 2000\r\n"));
            assert!(output.ends_with(b"\r\n\r\n"));
            assert!(!text.contains("event: error"));
            let event = rewritten_event(&output);
            assert_eq!(event["type"], "response.failed");
            assert_eq!(event["response"]["error"]["code"], "invalid_prompt");
            assert_eq!(
                event["response"]["error"]["codey"]["originalCode"],
                "authentication_error"
            );
            assert_eq!(event["response"]["error"]["message"], "bad key");
            let mut terminal = NativeSseTerminal::default();
            assert!(terminal.observe(&output).unwrap());
            terminal.finish().unwrap();
        }
    }

    #[test]
    fn responses_rewriter_normalizes_failure_tail_and_preserves_response_fields() {
        let input = b"event: response.failed\ndata: {\"type\":\"response.failed\",\"sequence_number\":3,\"response\":{\"id\":\"resp_upstream\",\"status\":\"failed\",\"error\":{\"code\":\"insufficient_quota\",\"message\":\"no credit\"}}}";
        let output = rewrite_chunks(input, 1, true);
        let event = rewritten_event(&output);
        assert_eq!(event["sequence_number"], 3);
        assert_eq!(event["response"]["id"], "resp_upstream");
        assert_eq!(event["response"]["error"]["code"], "insufficient_quota");
        let mut terminal = NativeSseTerminal::default();
        assert!(terminal.observe(&output).unwrap());
        terminal.finish().unwrap();
    }

    #[test]
    fn responses_rewriter_is_idempotent_and_keeps_failure_event_headers_consistent() {
        let input = b"event: error\ndata: {\"type\":\"error\",\"error\":{\"code\":\"invalid_request_error\",\"message\":\"invalid\"}}\n\n";
        let output = rewrite_chunks(input, 1, true);
        assert_eq!(rewrite_chunks(&output, 1, true), output);
        let wrong_header = String::from_utf8(output.clone()).unwrap().replacen(
            "event: response.failed",
            "event: error",
            1,
        );
        assert_eq!(rewrite_chunks(wrong_header.as_bytes(), 1, true), output);
        let fix = NativeResponseFix::from(XaiResponseFix::default());
        let mut rewriter = ResponsesSseRewriter::new(Some(&fix), true, None);
        let event = rewritten_event(&rewriter.push(input).unwrap());
        assert_eq!(event["response"]["error"]["code"], "invalid_prompt");
    }

    #[test]
    fn responses_rewriter_applies_retry_after_to_failure_only() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("2"));
        let advice = ResponseRetryAdvice::from_headers(&headers).unwrap();
        let mut rewriter = ResponsesSseRewriter::new(None, true, Some(&advice));
        let normal = b"data: {\"type\":\"response.created\"}\n\n";
        assert_eq!(rewriter.push(normal).unwrap(), normal);
        let error = b"event: error\ndata: {\"type\":\"error\",\"error\":{\"code\":\"server_error\",\"message\":\"busy\"}}\n\n";
        let event = rewritten_event(&rewriter.push(error).unwrap());
        let error = &event["response"]["error"];
        assert_eq!(error["code"], "rate_limit_exceeded");
        let delay = error["codey"]["retryAfterMs"].as_u64().unwrap();
        assert!(delay <= 2000);
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .starts_with("Please try again in ")
        );
        assert!(error["message"].as_str().unwrap().ends_with("s. busy"));
    }

    #[test]
    fn responses_rewriter_leaves_non_responses_errors_unchanged() {
        let input = b"event: error\r\nid: image\r\ndata: {\"type\":\"error\",\"error\":{\"code\":\"invalid_request_error\"}}\r\n\r\n";
        for chunk_size in 1..=input.len() {
            assert_eq!(rewrite_chunks(input, chunk_size, false), input);
        }
    }

    #[test]
    fn responses_rewriter_does_not_hide_missing_or_invalid_terminal_events() {
        for input in [
            b"data: {\"type\":\"response.created\"}\n\n".as_slice(),
            b"data: [DONE]\n\n".as_slice(),
            b"data: malformed\n\n".as_slice(),
        ] {
            let output = rewrite_chunks(input, 1, true);
            assert_eq!(output, input);
            let mut terminal = NativeSseTerminal::default();
            assert!(
                terminal
                    .observe(&output)
                    .and_then(|_| terminal.finish())
                    .is_err()
            );
        }
    }

    #[test]
    fn bom_is_removed_once_even_across_chunks_and_buffer_compaction() {
        let input = b"\xef\xbb\xbfdata: first\n\ndata: second\r\n\r\n\xef\xbb\xbfdata: ignored\n\n";
        for chunk_size in 1..=input.len() {
            let mut buffer = Vec::new();
            let mut cursor = SseCursor::default();
            let mut data = Vec::new();
            for chunk in input.chunks(chunk_size) {
                compact_sse_buffer(&mut buffer, &mut cursor);
                buffer.extend_from_slice(chunk);
                while let Some(frame) = take_next_sse_frame(&buffer, &mut cursor) {
                    if let Some(value) = sse_frame_data(frame).unwrap() {
                        data.push(value.into_owned());
                    }
                }
            }
            assert_eq!(data, ["first", "second"], "chunk size {chunk_size}");
        }
        let mut cursor = SseCursor::default();
        let tail = b"\xef\xbb\xbfdata: tail";
        assert!(take_next_sse_frame(tail, &mut cursor).is_none());
        assert_eq!(
            sse_frame_data(&tail[cursor.consumed..]).unwrap().as_deref(),
            Some("tail")
        );
    }

    #[test]
    fn sse_delimiters_keep_the_earlier_frame_boundary() {
        let input = b"data: one\r\n\r\ndata: two\n\n";
        let mut cursor = SseCursor::default();
        let first = take_next_sse_frame(input, &mut cursor).unwrap();
        assert_eq!(sse_frame_data(first).unwrap().as_deref(), Some("one"));
        let second = take_next_sse_frame(input, &mut cursor).unwrap();
        assert_eq!(sse_frame_data(second).unwrap().as_deref(), Some("two"));
        assert!(take_next_sse_frame(input, &mut cursor).is_none());
    }
}
