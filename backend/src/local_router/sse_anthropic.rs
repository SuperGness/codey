use super::*;

#[cfg(test)]
pub(crate) fn anthropic_message_to_responses_body(
    message: &Value,
    fallback_model: &str,
) -> Result<Value> {
    let tool_bridge = ResponsesToolBridge::default();
    anthropic_message_to_responses_body_with_tool_bridge(message, fallback_model, &tool_bridge)
}

pub(crate) fn anthropic_message_to_responses_body_with_tool_bridge(
    message: &Value,
    fallback_model: &str,
    tool_bridge: &ResponsesToolBridge,
) -> Result<Value> {
    check_context_length_error(message)?;
    if message.get("type").and_then(Value::as_str) == Some("error") {
        let detail = message
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("未知错误");
        anyhow::bail!("Anthropic Messages 返回错误：{detail}");
    }
    let content = message
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("Anthropic message 缺少 content 数组"))?;
    let mut output = Vec::new();
    let mut message_content = Vec::new();
    let mut output_text_parts = Vec::new();
    for block in content {
        let block_type = block
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Anthropic content block 缺少 type"))?;
        match block_type {
            "text" => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("Anthropic text block 缺少 text"))?;
                output_text_parts.push(text.to_string());
                message_content.push(json!({
                    "type":"output_text",
                    "text":text,
                    "annotations":[],
                }));
            }
            "refusal" => {
                let refusal = block
                    .get("refusal")
                    .or_else(|| block.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                message_content.push(json!({"type":"refusal","refusal":refusal}));
            }
            "tool_use" => {
                let call_id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("Anthropic tool_use 缺少 id"))?;
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("Anthropic tool_use 缺少 name"))?;
                let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                if !input.is_object() {
                    anyhow::bail!("Anthropic tool_use.input 必须是 JSON 对象");
                }
                let tool_name = tool_bridge.restore_upstream_name(name)?;
                let arguments = serde_json::to_string(&input)
                    .context("序列化 Anthropic tool_use.input 失败")?;
                output.push(responses_tool_call_item_from_upstream_arguments(
                    &tool_name,
                    call_id.to_string(),
                    arguments,
                    "completed",
                    "Anthropic custom tool_use.input",
                )?);
            }
            // Raw chain-of-thought must not be surfaced as assistant text.
            // Signature/redacted blocks are provider state and have no safe
            // stateless Responses representation.
            "thinking" | "redacted_thinking" => {}
            other => {
                anyhow::bail!("Anthropic content block 类型 {other} 不能转换为 Responses output")
            }
        }
    }
    if !message_content.is_empty() {
        output.insert(
            0,
            json!({
                "id":format!("msg_codey_{}", Uuid::new_v4()),
                "type":"message",
                "status":"completed",
                "role":"assistant",
                "content":message_content,
            }),
        );
    }
    if output.is_empty() {
        output.push(json!({
            "id":format!("msg_codey_{}", Uuid::new_v4()),
            "type":"message",
            "status":"completed",
            "role":"assistant",
            "content":[{"type":"output_text","text":"","annotations":[]}],
        }));
    }
    let stop_reason = message.get("stop_reason").and_then(Value::as_str);
    let incomplete_reason = match stop_reason {
        Some("max_tokens") => Some("max_output_tokens"),
        Some("refusal") => Some("content_filter"),
        _ => None,
    };
    let status = if incomplete_reason.is_some() {
        "incomplete"
    } else {
        "completed"
    };
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_model);
    let mut responses = json!({
        "id":format!("resp_codey_{}", Uuid::new_v4()),
        "object":"response",
        "created_at":current_unix_timestamp(),
        "status":status,
        "model":model,
        "output":output,
        "output_text":output_text_parts.join(""),
        "error":Value::Null,
        "incomplete_details":incomplete_reason.map(|reason| json!({"reason":reason})),
    });
    if let Some(usage) = message.get("usage") {
        responses
            .as_object_mut()
            .expect("Responses wrapper must be an object")
            .insert(
                "usage".to_string(),
                anthropic_usage_to_responses_usage(usage),
            );
    }
    Ok(responses)
}

pub(crate) fn anthropic_usage_to_responses_usage(usage: &Value) -> Value {
    let uncached_input_tokens = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_creation_tokens = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached_tokens = usage
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let input_tokens = uncached_input_tokens
        .saturating_add(cache_creation_tokens)
        .saturating_add(cached_tokens);
    let output_tokens = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "input_tokens":input_tokens,
        "input_tokens_details":{"cached_tokens":cached_tokens},
        "output_tokens":output_tokens,
        "output_tokens_details":{"reasoning_tokens":0},
        "total_tokens":input_tokens.saturating_add(output_tokens),
    })
}

#[derive(Debug, Default)]
pub(crate) struct AnthropicSseBlock {
    pub(crate) block_type: String,
    pub(crate) text: String,
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) input: Option<Value>,
    pub(crate) partial_json: String,
}

#[derive(Debug)]
pub(crate) struct AnthropicSseAccumulator {
    retain_text: bool,
    pub(crate) id: String,
    pub(crate) model: String,
    pub(crate) blocks: BTreeMap<usize, AnthropicSseBlock>,
    pub(crate) stop_reason: Option<String>,
    pub(crate) usage: serde_json::Map<String, Value>,
    pub(crate) stopped: bool,
}

impl AnthropicSseAccumulator {
    pub(crate) fn new(model: &str) -> Self {
        Self {
            retain_text: true,
            id: format!("msg_codey_{}", Uuid::new_v4()),
            model: model.to_string(),
            blocks: BTreeMap::new(),
            stop_reason: None,
            usage: serde_json::Map::new(),
            stopped: false,
        }
    }

    pub(crate) fn for_streaming(model: &str) -> Self {
        Self {
            retain_text: false,
            ..Self::new(model)
        }
    }

    pub(crate) fn ingest(&mut self, event: &Value) -> Result<()> {
        check_context_length_error(event)?;
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Anthropic SSE 事件缺少 type"))?;
        match event_type {
            "ping" => {}
            "error" => {
                let detail = event
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("未知错误");
                anyhow::bail!("Anthropic SSE 返回错误：{detail}");
            }
            "message_start" => {
                let message = event
                    .get("message")
                    .and_then(Value::as_object)
                    .ok_or_else(|| anyhow::anyhow!("message_start 缺少 message"))?;
                if let Some(id) = message
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    self.id = id.to_string();
                }
                if let Some(model) = message
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    self.model = model.to_string();
                }
                if let Some(usage) = message.get("usage").and_then(Value::as_object) {
                    self.usage.extend(usage.clone());
                }
                if let Some(content) = message.get("content").and_then(Value::as_array) {
                    for (index, block) in content.iter().enumerate() {
                        self.blocks.insert(
                            index,
                            anthropic_sse_block_from_value(block, self.retain_text)?,
                        );
                    }
                }
            }
            "content_block_start" => {
                let index = anthropic_sse_index(event)?;
                let block = event
                    .get("content_block")
                    .ok_or_else(|| anyhow::anyhow!("content_block_start 缺少 content_block"))?;
                self.blocks.insert(
                    index,
                    anthropic_sse_block_from_value(block, self.retain_text)?,
                );
            }
            "content_block_delta" => {
                let index = anthropic_sse_index(event)?;
                let delta = event
                    .get("delta")
                    .and_then(Value::as_object)
                    .ok_or_else(|| anyhow::anyhow!("content_block_delta 缺少 delta"))?;
                let delta_type = delta
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("Anthropic content delta 缺少 type"))?;
                let block = self.blocks.entry(index).or_default();
                match delta_type {
                    "text_delta" => {
                        if block.block_type.is_empty() {
                            block.block_type = "text".to_string();
                        }
                        if self.retain_text {
                            block.text.push_str(
                                delta
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default(),
                            );
                        }
                    }
                    "input_json_delta" => {
                        if block.block_type.is_empty() {
                            block.block_type = "tool_use".to_string();
                        }
                        block.partial_json.push_str(
                            delta
                                .get("partial_json")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        );
                    }
                    "thinking_delta" => {
                        if block.block_type.is_empty() {
                            block.block_type = "thinking".to_string();
                        }
                        if self.retain_text {
                            block.text.push_str(
                                delta
                                    .get("thinking")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default(),
                            );
                        }
                    }
                    "signature_delta" | "citations_delta" => {}
                    other => anyhow::bail!("不支持的 Anthropic content delta 类型 {other}"),
                }
            }
            "content_block_stop" => {}
            "message_delta" => {
                if let Some(stop_reason) = event
                    .get("delta")
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    self.stop_reason = Some(stop_reason.to_string());
                }
                if let Some(usage) = event.get("usage").and_then(Value::as_object) {
                    self.usage.extend(usage.clone());
                }
            }
            "message_stop" => self.stopped = true,
            other => anyhow::bail!("不支持的 Anthropic SSE 事件类型 {other}"),
        }
        Ok(())
    }

    pub(crate) fn into_message(self) -> Result<Value> {
        if !self.stopped
            && self
                .stop_reason
                .as_deref()
                .is_none_or(|reason| reason.trim().is_empty())
        {
            anyhow::bail!("Anthropic Messages SSE 在结束事件或 stop_reason 前断开");
        }
        let content = self
            .blocks
            .into_values()
            .map(anthropic_sse_block_into_value)
            .collect::<Result<Vec<_>>>()?;
        Ok(json!({
            "id":self.id,
            "type":"message",
            "role":"assistant",
            "model":self.model,
            "content":content,
            "stop_reason":self.stop_reason,
            "usage":Value::Object(self.usage),
        }))
    }
}

pub(crate) fn anthropic_sse_index(event: &Value) -> Result<usize> {
    event
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| anyhow::anyhow!("Anthropic SSE 事件缺少有效 index"))
}

fn anthropic_sse_block_from_value(block: &Value, retain_text: bool) -> Result<AnthropicSseBlock> {
    let block_type = block
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Anthropic content block 缺少 type"))?;
    Ok(AnthropicSseBlock {
        block_type: block_type.to_string(),
        text: if retain_text {
            block
                .get("text")
                .or_else(|| block.get("thinking"))
                .or_else(|| block.get("refusal"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        } else {
            String::new()
        },
        id: block
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        name: block
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        input: block.get("input").cloned(),
        partial_json: String::new(),
    })
}

pub(crate) fn anthropic_sse_block_into_value(block: AnthropicSseBlock) -> Result<Value> {
    match block.block_type.as_str() {
        "text" => Ok(json!({"type":"text","text":block.text})),
        "refusal" => Ok(json!({"type":"refusal","refusal":block.text})),
        "thinking" | "redacted_thinking" => {
            Ok(json!({"type":block.block_type,"thinking":block.text}))
        }
        "tool_use" => {
            if block.id.is_empty() || block.name.is_empty() {
                anyhow::bail!("Anthropic 流式 tool_use 缺少 id 或 name");
            }
            let input = if block.partial_json.trim().is_empty() {
                block.input.unwrap_or_else(|| json!({}))
            } else {
                serde_json::from_str::<Value>(&block.partial_json)
                    .context("Anthropic 流式 tool_use 参数不是有效 JSON")?
            };
            if !input.is_object() {
                anyhow::bail!("Anthropic 流式 tool_use.input 必须是 JSON 对象");
            }
            Ok(json!({
                "type":"tool_use",
                "id":block.id,
                "name":block.name,
                "input":input,
            }))
        }
        other => anyhow::bail!("不支持的 Anthropic content block 类型 {other}"),
    }
}

pub(crate) async fn collect_anthropic_message_sse(
    prepared: &mut PreparedUpstreamResponse,
    model: &str,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<Value> {
    let mut accumulator = AnthropicSseAccumulator::new(model);
    collect_sse_frames(prepared, &mut accumulator, probe).await?;
    accumulator.into_message()
}

pub(crate) fn parse_anthropic_message_sse_bytes(bytes: &[u8], model: &str) -> Result<Value> {
    let mut accumulator = AnthropicSseAccumulator::new(model);
    parse_sse_frames(bytes, &mut accumulator)?;
    accumulator.into_message()
}

pub(crate) async fn stream_anthropic_messages_as_responses<D>(
    downstream: &mut D,
    mut prepared: PreparedUpstreamResponse,
    model: &str,
    route: &RouteTarget,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()>
where
    D: ResponsesDownstream + ?Sized,
{
    let mut output = ResponsesSseState::new(model, tool_bridge);
    prepared.retained.get_or_insert_with(Default::default);
    output.start(downstream).await?;
    let mut accumulator = AnthropicSseAccumulator::for_streaming(model);
    let request_log_probe = downstream.request_log_probe().cloned();
    let result: Result<()> = async {
        let mut buffer = Vec::new();
        let mut cursor = SseCursor::default();
        while let Some(chunk) = await_upstream(
            downstream,
            read_prepared_upstream_chunk(
                &mut prepared,
                "读取 Anthropic Messages SSE 流失败",
                request_log_probe.as_ref(),
            ),
        )
        .await??
        {
            compact_sse_buffer(&mut buffer, &mut cursor);
            buffer.extend_from_slice(&chunk);
            ensure_sse_buffer_within_limit(&buffer, cursor.consumed)?;
            while let Some(frame) = take_next_sse_frame(&buffer, &mut cursor) {
                let Some(data) = sse_frame_data(frame)? else {
                    continue;
                };
                let event = serde_json::from_str::<Value>(&data)
                    .context("Anthropic Messages SSE data 不是有效 JSON")?;
                accumulator.ingest(&event)?;
                emit_anthropic_stream_event(&mut output, downstream, &event).await?;
            }
            if accumulator.stopped {
                break;
            }
        }
        if !accumulator.stopped
            && !buffer[cursor.consumed..]
                .iter()
                .all(u8::is_ascii_whitespace)
            && let Some(data) = sse_frame_data(&buffer[cursor.consumed..])?
        {
            let event = serde_json::from_str::<Value>(&data)
                .context("Anthropic Messages SSE 末尾 data 不是有效 JSON")?;
            accumulator.ingest(&event)?;
            emit_anthropic_stream_event(&mut output, downstream, &event).await?;
        }
        // Text is already retained by output. Keep tool and unknown blocks in
        // the existing conversion so validation and error ordering stay intact.
        accumulator.blocks.retain(|_, block| {
            !matches!(
                block.block_type.as_str(),
                "text" | "refusal" | "thinking" | "redacted_thinking"
            )
        });
        let message = accumulator.into_message()?;
        let completed =
            anthropic_message_to_responses_body_with_tool_bridge(&message, model, tool_bridge)?;
        drop(message);
        if output.output_order.is_empty() {
            let events = output.ensure_message();
            output.write_events(downstream, events).await?;
        }
        let usage = completed.get("usage").cloned();
        let incomplete_reason = completed
            .get("incomplete_details")
            .and_then(|details| details.get("reason"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        drop(completed);
        output
            .finish(downstream, usage, incomplete_reason.as_deref())
            .await
    }
    .await;
    if let Err(error) = result {
        if error.is::<DownstreamClosed>() {
            return Err(error);
        }
        let (code, message) = streaming_failure_message(&error, route);
        let _ = output.fail(downstream, code, &message).await;
        return Err(error);
    }
    Ok(())
}

pub(crate) async fn emit_anthropic_stream_event<D>(
    output: &mut ResponsesSseState<'_>,
    downstream: &mut D,
    event: &Value,
) -> Result<()>
where
    D: ResponsesDownstream + ?Sized,
{
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Anthropic SSE 事件缺少 type"))?;
    let mut events = Vec::new();
    match event_type {
        "message_start" => {
            if let Some(content) = event
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array)
            {
                for (index, block) in content.iter().enumerate() {
                    events.extend(anthropic_stream_block_start(output, index, block)?);
                }
            }
        }
        "content_block_start" => {
            let index = anthropic_sse_index(event)?;
            let block = event
                .get("content_block")
                .ok_or_else(|| anyhow::anyhow!("content_block_start 缺少 content_block"))?;
            events.extend(anthropic_stream_block_start(output, index, block)?);
        }
        "content_block_delta" => {
            let index = anthropic_sse_index(event)?;
            let delta = event
                .get("delta")
                .and_then(Value::as_object)
                .ok_or_else(|| anyhow::anyhow!("content_block_delta 缺少 delta"))?;
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => events.extend(
                    output.text_delta(
                        delta
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    ),
                ),
                Some("input_json_delta") => events.extend(output.tool_delta(
                    index,
                    None,
                    None,
                    delta.get("partial_json").and_then(Value::as_str),
                    None,
                )?),
                Some("thinking_delta" | "signature_delta" | "citations_delta") => {}
                Some(other) => anyhow::bail!("不支持的 Anthropic content delta 类型 {other}"),
                None => anyhow::bail!("Anthropic content delta 缺少 type"),
            }
        }
        "ping" | "content_block_stop" | "message_delta" | "message_stop" => {}
        "error" => anyhow::bail!("Anthropic SSE 返回错误"),
        other => anyhow::bail!("不支持的 Anthropic SSE 事件类型 {other}"),
    }
    output.write_events(downstream, events).await
}

pub(crate) fn anthropic_stream_block_start(
    output: &mut ResponsesSseState<'_>,
    index: usize,
    block: &Value,
) -> Result<Vec<Value>> {
    let block_type = block
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Anthropic content block 缺少 type"))?;
    match block_type {
        "text" => Ok(output.text_delta(
            block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )),
        "refusal" => Ok(output.refusal_delta(
            block
                .get("refusal")
                .or_else(|| block.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )),
        "tool_use" => {
            let fallback_arguments = block
                .get("input")
                .map(serde_json::to_string)
                .transpose()
                .context("序列化 Anthropic tool_use.input 失败")?;
            output.tool_delta(
                index,
                block.get("id").and_then(Value::as_str),
                block.get("name").and_then(Value::as_str),
                None,
                fallback_arguments,
            )
        }
        "thinking" | "redacted_thinking" => Ok(Vec::new()),
        other => anyhow::bail!("不支持的 Anthropic content block 类型 {other}"),
    }
}

pub(crate) fn ensure_sse_buffer_within_limit(buffer: &[u8], cursor: usize) -> Result<()> {
    if buffer.len().saturating_sub(cursor) > MAX_UPSTREAM_SSE_BUFFER_BYTES {
        anyhow::bail!("上游 SSE 单帧超过 Codey 安全上限");
    }
    Ok(())
}

pub(crate) fn streaming_failure_message(
    error: &anyhow::Error,
    route: &RouteTarget,
) -> (&'static str, String) {
    if error.is::<ContextLengthExceeded>() {
        (CONTEXT_LENGTH_EXCEEDED, ContextLengthExceeded.to_string())
    } else if error.downcast_ref::<UpstreamReadIdleTimeout>().is_some() {
        (
            "upstream_idle_timeout",
            format!(
                "线路「{}」的上游流长时间没有返回新数据",
                route_display_name(route)
            ),
        )
    } else {
        (
            "upstream_stream_error",
            format!(
                "线路「{}」返回了无法继续处理的流式响应",
                route_display_name(route)
            ),
        )
    }
}

pub(crate) fn chat_message_content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                part.as_object().and_then(|object| {
                    object
                        .get("text")
                        .or_else(|| object.get("content"))
                        .and_then(Value::as_str)
                })
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub(crate) fn chat_message_annotations(message: &serde_json::Map<String, Value>) -> Value {
    message
        .get("annotations")
        .filter(|annotations| annotations.is_array())
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()))
}
