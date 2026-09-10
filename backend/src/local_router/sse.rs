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
    for index in cursor.scanned..buffer.len() {
        let length = if buffer.get(index..index + 4) == Some(b"\r\n\r\n") {
            4
        } else if buffer.get(index..index + 2) == Some(b"\n\n") {
            2
        } else {
            continue;
        };
        let frame = &buffer[cursor.consumed..index];
        cursor.consumed = index + length;
        cursor.scanned = cursor.consumed;
        return Some(frame);
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
}
