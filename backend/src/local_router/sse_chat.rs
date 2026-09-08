use super::*;

#[cfg(test)]
pub(crate) fn chat_completion_to_responses_body(chat: Value, model: &str) -> Result<Value> {
    let tool_bridge = ResponsesToolBridge::default();
    chat_completion_to_responses_body_with_tool_bridge(chat, model, &tool_bridge)
}

pub(crate) fn chat_completion_to_responses_body_with_tool_bridge(
    mut chat: Value,
    model: &str,
    tool_bridge: &ResponsesToolBridge,
) -> Result<Value> {
    check_context_length_error(&chat)?;
    let response_id = chat
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("resp_codey_{}", Uuid::new_v4()));
    let created_at = chat
        .get("created")
        .and_then(Value::as_i64)
        .unwrap_or_else(current_unix_timestamp);
    let choice = chat
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("Chat Completions 响应缺少 choices[0]"))?;
    let message = choice
        .get("message")
        .or_else(|| choice.get("delta"))
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("Chat Completions 响应缺少 assistant message"))?;
    let text = message
        .get("content")
        .map(chat_message_content_text)
        .unwrap_or_default();
    let refusal = message
        .get("refusal")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let annotations = chat_message_annotations(message);
    let mut output = Vec::new();
    if !text.is_empty() || !refusal.is_empty() {
        let mut content = Vec::new();
        if !text.is_empty() {
            content.push(json!({
                "type": "output_text",
                "text": text,
                "annotations": annotations
            }));
        }
        if !refusal.is_empty() {
            content.push(json!({"type":"refusal","refusal":refusal}));
        }
        output.push(json!({
            "id": format!("msg_codey_{}", Uuid::new_v4()),
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": content,
        }));
    }
    if let Some(tool_calls) = message.get("tool_calls") {
        append_chat_tool_calls_to_responses_output(tool_calls, &mut output, tool_bridge)?;
    }
    if let Some(function_call) = message.get("function_call") {
        append_legacy_chat_function_call_to_responses_output(
            function_call,
            &mut output,
            tool_bridge,
        )?;
    }
    if output.is_empty() {
        output.push(json!({
            "id": format!("msg_codey_{}", Uuid::new_v4()),
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": "",
                "annotations": []
            }]
        }));
    }
    let finish_reason = choice.get("finish_reason").and_then(Value::as_str);
    let incomplete_reason = match finish_reason {
        Some("length") => Some("max_output_tokens"),
        Some("content_filter") => Some("content_filter"),
        _ => None,
    };
    let status = if incomplete_reason.is_some() {
        "incomplete"
    } else {
        "completed"
    };
    let mut response = json!({
        "id": response_id,
        "object": "response",
        "created_at": created_at,
        "status": status,
        "model": model,
        "output": output,
        "output_text": text,
        "error": Value::Null,
        "incomplete_details": incomplete_reason.map(|reason| json!({"reason":reason})),
    });
    if let Some(usage) = chat
        .as_object_mut()
        .and_then(|object| object.remove("usage"))
    {
        response
            .as_object_mut()
            .expect("Responses wrapper must be an object")
            .insert("usage".to_string(), chat_usage_to_responses_usage(&usage));
    }
    Ok(response)
}

pub(crate) fn append_chat_tool_calls_to_responses_output(
    tool_calls: &Value,
    output: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    let tool_calls = tool_calls
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Chat message.tool_calls 必须是数组"))?;
    for tool_call in tool_calls {
        let object = tool_call
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Chat tool_call 必须是对象"))?;
        let call_type = object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("function");
        if call_type != "function" {
            anyhow::bail!("Chat tool_call 类型 {call_type} 不能转换为 Responses item");
        }
        let function = object
            .get("function")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("Chat tool_call 缺少 function"))?;
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Chat tool_call.function 缺少 name"))?;
        let arguments = json_value_as_chat_string(function.get("arguments"))
            .unwrap_or_else(|| "{}".to_string());
        let call_id = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("call_codey_{}", Uuid::new_v4()));
        let tool_name = tool_bridge.restore_upstream_name(name)?;
        output.push(responses_tool_call_item_from_upstream_arguments(
            &tool_name,
            call_id,
            arguments,
            "completed",
            "Chat custom tool_call.function.arguments",
        )?);
    }
    Ok(())
}

pub(crate) fn append_legacy_chat_function_call_to_responses_output(
    function_call: &Value,
    output: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    let function = function_call
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Chat message.function_call 必须是对象"))?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Chat message.function_call 缺少 name"))?;
    let tool_name = tool_bridge.restore_upstream_name(name)?;
    output.push(responses_tool_call_item_from_upstream_arguments(
        &tool_name,
        format!("call_codey_{}", Uuid::new_v4()),
        json_value_as_chat_string(function.get("arguments")).unwrap_or_else(|| "{}".to_string()),
        "completed",
        "Chat legacy custom function_call.arguments",
    )?);
    Ok(())
}

pub(crate) fn chat_usage_to_responses_usage(usage: &Value) -> Value {
    let input_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(input_tokens.saturating_add(output_tokens));
    let cached_tokens = usage
        .get("prompt_tokens_details")
        .or_else(|| usage.get("input_tokens_details"))
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let reasoning_tokens = usage
        .get("completion_tokens_details")
        .or_else(|| usage.get("output_tokens_details"))
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "input_tokens": input_tokens,
        "input_tokens_details": {"cached_tokens": cached_tokens},
        "output_tokens": output_tokens,
        "output_tokens_details": {"reasoning_tokens": reasoning_tokens},
        "total_tokens": total_tokens,
    })
}

#[derive(Debug, Default)]
pub(crate) struct ChatSseToolCall {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) arguments: String,
}

#[derive(Debug)]
pub(crate) struct ChatSseAccumulator {
    retain_text: bool,
    pub(crate) id: String,
    pub(crate) created: i64,
    pub(crate) model: String,
    pub(crate) content: String,
    pub(crate) refusal: String,
    pub(crate) tool_calls: BTreeMap<usize, ChatSseToolCall>,
    pub(crate) finish_reason: Option<String>,
    pub(crate) usage: Option<Value>,
    /// Set once the upstream emitted the `[DONE]` sentinel.
    pub(crate) done: bool,
}

impl ChatSseAccumulator {
    pub(crate) fn new(model: &str) -> Self {
        Self {
            retain_text: true,
            id: format!("chatcmpl_codey_{}", Uuid::new_v4()),
            created: current_unix_timestamp(),
            model: model.to_string(),
            content: String::new(),
            refusal: String::new(),
            tool_calls: BTreeMap::new(),
            finish_reason: None,
            usage: None,
            done: false,
        }
    }

    pub(crate) fn for_streaming(model: &str) -> Self {
        Self {
            retain_text: false,
            ..Self::new(model)
        }
    }

    pub(crate) fn ingest(&mut self, chunk: &Value) -> Result<()> {
        check_context_length_error(chunk)?;
        if let Some(error) = chunk.get("error") {
            anyhow::bail!("Chat Completions 流返回错误：{error}");
        }
        if let Some(id) = chunk
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            self.id = id.to_string();
        }
        if let Some(created) = chunk.get("created").and_then(Value::as_i64) {
            self.created = created;
        }
        if let Some(model) = chunk
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty())
        {
            self.model = model.to_string();
        }
        if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
            self.usage = Some(usage.clone());
        }
        let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
            return Ok(());
        };
        for choice in choices {
            if choice.get("index").and_then(Value::as_u64).unwrap_or(0) != 0 {
                continue;
            }
            if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str) {
                self.finish_reason = Some(finish_reason.to_string());
            }
            let Some(delta) = choice
                .get("delta")
                .or_else(|| choice.get("message"))
                .and_then(Value::as_object)
            else {
                continue;
            };
            // ResponsesSseState already retains text for the final streaming events.
            if self.retain_text {
                if let Some(content) = delta.get("content") {
                    self.content.push_str(&chat_message_content_text(content));
                }
                if let Some(refusal) = delta.get("refusal").and_then(Value::as_str) {
                    self.refusal.push_str(refusal);
                }
            }
            if let Some(tool_calls) = delta
                .get("tool_calls")
                .filter(|tool_calls| !tool_calls.is_null())
            {
                self.ingest_tool_calls(tool_calls)?;
            }
            if let Some(function_call) = delta.get("function_call") {
                self.ingest_legacy_function_call(function_call)?;
            }
        }
        Ok(())
    }

    pub(crate) fn ingest_tool_calls(&mut self, tool_calls: &Value) -> Result<()> {
        let tool_calls = tool_calls
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Chat stream delta.tool_calls 必须是数组"))?;
        for tool_call in tool_calls {
            let object = tool_call
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("Chat stream tool_call delta 必须是对象"))?;
            let index = object.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let state = self.tool_calls.entry(index).or_default();
            if let Some(id) = object
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            {
                state.id = id.to_string();
            }
            if let Some(call_type) = object.get("type").and_then(Value::as_str)
                && call_type != "function"
            {
                anyhow::bail!("Chat stream tool_call 类型 {call_type} 不受支持");
            }
            if let Some(function) = object.get("function").and_then(Value::as_object) {
                if let Some(name_delta) = function.get("name").and_then(Value::as_str) {
                    state.name.push_str(name_delta);
                }
                if let Some(arguments_delta) = function.get("arguments") {
                    state.arguments.push_str(
                        &json_value_as_chat_string(Some(arguments_delta)).unwrap_or_default(),
                    );
                }
            }
        }
        Ok(())
    }

    pub(crate) fn ingest_legacy_function_call(&mut self, function_call: &Value) -> Result<()> {
        let function = function_call
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Chat stream function_call delta 必须是对象"))?;
        let state = self.tool_calls.entry(0).or_default();
        if let Some(name_delta) = function.get("name").and_then(Value::as_str) {
            state.name.push_str(name_delta);
        }
        if let Some(arguments_delta) = function.get("arguments") {
            state
                .arguments
                .push_str(&json_value_as_chat_string(Some(arguments_delta)).unwrap_or_default());
        }
        Ok(())
    }

    pub(crate) fn into_chat_completion(self, done: bool) -> Result<Value> {
        if !done
            && self
                .finish_reason
                .as_deref()
                .is_none_or(|reason| reason.trim().is_empty())
        {
            anyhow::bail!("Chat Completions SSE 在 [DONE] 或 finish_reason 前断开");
        }
        let mut message = serde_json::Map::from_iter([(
            "role".to_string(),
            Value::String("assistant".to_string()),
        )]);
        if !self.content.is_empty() {
            message.insert("content".to_string(), Value::String(self.content));
        } else {
            message.insert("content".to_string(), Value::Null);
        }
        if !self.refusal.is_empty() {
            message.insert("refusal".to_string(), Value::String(self.refusal));
        }
        if !self.tool_calls.is_empty() {
            let tool_calls = self
                .tool_calls
                .into_values()
                .map(|tool_call| {
                    if tool_call.name.is_empty() {
                        anyhow::bail!("Chat stream tool_call 缺少 function.name");
                    }
                    Ok(json!({
                        "id": if tool_call.id.is_empty() {
                            format!("call_codey_{}", Uuid::new_v4())
                        } else {
                            tool_call.id
                        },
                        "type": "function",
                        "function": {
                            "name": tool_call.name,
                            "arguments": tool_call.arguments,
                        }
                    }))
                })
                .collect::<Result<Vec<_>>>()?;
            message.insert("tool_calls".to_string(), Value::Array(tool_calls));
        }
        let mut chat = json!({
            "id": self.id,
            "object": "chat.completion",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "message": Value::Object(message),
                "finish_reason": self.finish_reason,
            }],
        });
        if let Some(usage) = self.usage {
            chat.as_object_mut()
                .expect("Chat completion wrapper must be an object")
                .insert("usage".to_string(), usage);
        }
        Ok(chat)
    }
}

pub(crate) async fn collect_chat_completion_sse(
    prepared: &mut PreparedUpstreamResponse,
    model: &str,
    probe: Option<&RouteRequestLogProbe>,
) -> Result<Value> {
    let mut accumulator = ChatSseAccumulator::new(model);
    collect_sse_frames(prepared, &mut accumulator, probe).await?;
    let done = accumulator.done;
    accumulator.into_chat_completion(done)
}

pub(crate) async fn stream_chat_completions_as_responses<D>(
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
    let mut accumulator = ChatSseAccumulator::for_streaming(model);
    let request_log_probe = downstream.request_log_probe().cloned();
    let result: Result<()> = async {
        let mut buffer = Vec::new();
        let mut cursor = SseCursor::default();
        let mut done = false;
        while let Some(chunk) = await_upstream(
            downstream,
            read_prepared_upstream_chunk(
                &mut prepared,
                "读取 Chat Completions SSE 流失败",
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
                if data.trim() == "[DONE]" {
                    done = true;
                    break;
                }
                let event = serde_json::from_str::<Value>(&data)
                    .context("Chat Completions SSE data 不是有效 JSON")?;
                accumulator.ingest(&event)?;
                emit_chat_stream_event(&mut output, downstream, &event).await?;
            }
            if done {
                break;
            }
        }
        if !done
            && !buffer[cursor.consumed..]
                .iter()
                .all(u8::is_ascii_whitespace)
            && let Some(data) = sse_frame_data(&buffer[cursor.consumed..])?
        {
            done = data.trim() == "[DONE]";
            if !done {
                let event = serde_json::from_str::<Value>(&data)
                    .context("Chat Completions SSE 末尾 data 不是有效 JSON")?;
                accumulator.ingest(&event)?;
                emit_chat_stream_event(&mut output, downstream, &event).await?;
            }
        }
        let chat = accumulator.into_chat_completion(done)?;
        let completed =
            chat_completion_to_responses_body_with_tool_bridge(chat, model, tool_bridge)?;
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

pub(crate) async fn emit_chat_stream_event<D>(
    output: &mut ResponsesSseState<'_>,
    downstream: &mut D,
    event: &Value,
) -> Result<()>
where
    D: ResponsesDownstream + ?Sized,
{
    let mut events = Vec::new();
    check_context_length_error(event)?;
    let Some(choices) = event.get("choices").and_then(Value::as_array) else {
        return Ok(());
    };
    for choice in choices {
        if choice.get("index").and_then(Value::as_u64).unwrap_or(0) != 0 {
            continue;
        }
        let Some(delta) = choice
            .get("delta")
            .or_else(|| choice.get("message"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        if let Some(content) = delta.get("content") {
            events.extend(output.text_delta(&chat_message_content_text(content)));
        }
        if let Some(refusal) = delta.get("refusal").and_then(Value::as_str) {
            events.extend(output.refusal_delta(refusal));
        }
        if let Some(tool_calls) = delta
            .get("tool_calls")
            .filter(|tool_calls| !tool_calls.is_null())
        {
            let tool_calls = tool_calls
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Chat stream delta.tool_calls 必须是数组"))?;
            for tool_call in tool_calls {
                let tool_call = tool_call
                    .as_object()
                    .ok_or_else(|| anyhow::anyhow!("Chat stream tool_call delta 必须是对象"))?;
                if let Some(call_type) = tool_call.get("type").and_then(Value::as_str)
                    && call_type != "function"
                {
                    anyhow::bail!("Chat stream tool_call 类型 {call_type} 不受支持");
                }
                let index = tool_call
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .unwrap_or(0);
                let function = tool_call.get("function").and_then(Value::as_object);
                let arguments_delta = function
                    .and_then(|function| function.get("arguments"))
                    .and_then(|arguments| json_value_as_chat_string(Some(arguments)));
                events.extend(
                    output.tool_delta(
                        index,
                        tool_call.get("id").and_then(Value::as_str),
                        function
                            .and_then(|function| function.get("name"))
                            .and_then(Value::as_str),
                        arguments_delta.as_deref(),
                        None,
                    )?,
                );
            }
        }
        if let Some(function_call) = delta.get("function_call") {
            let function_call = function_call
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("Chat stream function_call delta 必须是对象"))?;
            let arguments_delta = function_call
                .get("arguments")
                .and_then(|arguments| json_value_as_chat_string(Some(arguments)));
            events.extend(output.tool_delta(
                usize::MAX,
                None,
                function_call.get("name").and_then(Value::as_str),
                arguments_delta.as_deref(),
                None,
            )?);
        }
    }
    output.write_events(downstream, events).await
}

pub(crate) fn parse_chat_completion_sse_bytes(bytes: &[u8], model: &str) -> Result<Value> {
    let mut accumulator = ChatSseAccumulator::new(model);
    parse_sse_frames(bytes, &mut accumulator)?;
    let done = accumulator.done;
    accumulator.into_chat_completion(done)
}
