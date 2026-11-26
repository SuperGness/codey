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
    if let Some(reasoning) = message.get("reasoning_content").and_then(Value::as_str) {
        output.push(chat_reasoning_item(
            &format!("rs_codey_{}", Uuid::new_v4()),
            reasoning,
            "completed",
        ));
    }
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

pub(crate) fn chat_reasoning_item(id: &str, text: &str, status: &str) -> Value {
    json!({
        "id":id, "type":"reasoning", "status":status, "summary":[],
        "content":[{"type":"reasoning_text", "text":text}],
    })
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
    let mut input_details = json!({"cached_tokens": cached_tokens});
    if let Some(writes) = [
        "/input_tokens_details/cache_write_tokens",
        "/prompt_tokens_details/cache_write_tokens",
        "/cache_creation_input_tokens",
        "/cache_creation_tokens",
        "/cache_write_input_tokens",
    ]
    .iter()
    .find_map(|path| usage.pointer(path).and_then(Value::as_u64))
    {
        input_details["cache_write_tokens"] = json!(writes);
    }
    json!({
        "input_tokens": input_tokens,
        "input_tokens_details": input_details,
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
    pub(crate) reasoning_content: Option<String>,
    pub(crate) refusal: String,
    pub(crate) annotations: Vec<Value>,
    pub(crate) saw_output: bool,
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
            reasoning_content: None,
            refusal: String::new(),
            annotations: Vec::new(),
            saw_output: false,
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
        check_upstream_stream_failure(chunk)?;
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
            // ResponsesSseState already retains text for the final streaming events,
            // while saw_output tracks whether any real output arrived across both modes.
            if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str) {
                self.saw_output |= !reasoning.is_empty();
                if self.retain_text {
                    self.reasoning_content
                        .get_or_insert_default()
                        .push_str(reasoning);
                }
            }
            if let Some(content) = delta.get("content") {
                match content {
                    Value::String(text) => {
                        self.saw_output |= !text.is_empty();
                        if self.retain_text {
                            self.content.push_str(text);
                        }
                    }
                    other => {
                        let text = chat_message_content_text(other);
                        self.saw_output |= !text.is_empty();
                        if self.retain_text {
                            self.content.push_str(&text);
                        }
                    }
                }
            }
            if let Some(refusal) = delta.get("refusal").and_then(Value::as_str) {
                self.saw_output |= !refusal.is_empty();
                if self.retain_text {
                    self.refusal.push_str(refusal);
                }
            }
            if self.retain_text
                && let Some(annotations) = delta.get("annotations").and_then(Value::as_array)
            {
                self.annotations
                    .extend(annotations.iter().filter_map(normalize_chat_annotation));
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
            let index = chat_stream_tool_slot(object);
            let state = self.tool_calls.entry(index).or_default();
            if let Some(id) = object
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            {
                state.id = id.to_string();
            }
            validate_chat_stream_tool_type(object.get("type"))?;
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
        let index = chat_legacy_tool_slot(self.tool_calls.get(&0).map(|tool| tool.name.as_str()));
        let state = self.tool_calls.entry(index).or_default();
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

    pub(crate) fn finish_streaming(
        mut self,
        done: bool,
        tool_bridge: &ResponsesToolBridge,
    ) -> Result<(String, Option<Value>, Option<&'static str>)> {
        let has_finish_reason = self
            .finish_reason
            .as_deref()
            .is_some_and(|reason| !reason.trim().is_empty());
        if !done && !has_finish_reason {
            anyhow::bail!("Chat Completions SSE 在 [DONE] 或 finish_reason 前断开");
        }
        self.tool_calls.retain(|_, tool| {
            !(tool.name.is_empty() && tool.arguments.is_empty() && tool.id.is_empty())
        });
        if !has_finish_reason && !self.saw_output && self.tool_calls.is_empty() {
            anyhow::bail!("Chat Completions SSE 未返回 finish_reason 或任何有效输出即结束");
        }
        if let Some((target, source)) =
            chat_tool_merge_pair(&self.tool_calls, |tool| tool.name.is_empty())
            && let Some(merged) = self.tool_calls.remove(&source)
        {
            let tool = self
                .tool_calls
                .get_mut(&target)
                .expect("named tool call exists");
            if tool.id.is_empty() {
                tool.id = merged.id;
            }
            if tool.arguments.is_empty() {
                tool.arguments = merged.arguments;
            } else {
                tool.arguments.push_str(&merged.arguments);
            }
        }
        let finish_reason = self.finish_reason.as_deref().unwrap_or("unknown");
        for tool_call in self.tool_calls.values() {
            if tool_call.name.is_empty() {
                anyhow::bail!("Chat stream tool_call 缺少 function.name");
            }
            let tool_name = tool_bridge
                .restore_upstream_name(&tool_call.name)
                .with_context(|| {
                    format!("转换 Chat Completions 流式结果失败（finish_reason={finish_reason}）")
                })?;
            if tool_name.is_custom() {
                custom_tool_input_from_arguments(
                    &tool_call.arguments,
                    "Chat custom tool_call.function.arguments",
                )
                .with_context(|| {
                    format!("转换 Chat Completions 流式结果失败（finish_reason={finish_reason}）")
                })?;
            }
        }
        let incomplete_reason = match self.finish_reason.as_deref() {
            Some("length") => Some("max_output_tokens"),
            Some("content_filter") => Some("content_filter"),
            _ => None,
        };
        let usage = self.usage.as_ref().map(chat_usage_to_responses_usage);
        Ok((self.model, usage, incomplete_reason))
    }

    pub(crate) fn into_chat_completion(mut self, done: bool) -> Result<Value> {
        let has_finish_reason = self
            .finish_reason
            .as_deref()
            .is_some_and(|reason| !reason.trim().is_empty());
        if !done && !has_finish_reason {
            anyhow::bail!("Chat Completions SSE 在 [DONE] 或 finish_reason 前断开");
        }
        // 与 Responses 桥保持一致：先丢弃没有任何内容的空槽位，再把被拆成两种形状的同一次
        // 调用合回一个槽位，最后才校验名字是否完整。
        self.tool_calls.retain(|_, tool| {
            !(tool.name.is_empty() && tool.arguments.is_empty() && tool.id.is_empty())
        });
        if !has_finish_reason && !self.saw_output && self.tool_calls.is_empty() {
            anyhow::bail!("Chat Completions SSE 未返回 finish_reason 或任何有效输出即结束");
        }
        if let Some((target, source)) =
            chat_tool_merge_pair(&self.tool_calls, |tool| tool.name.is_empty())
            && let Some(merged) = self.tool_calls.remove(&source)
        {
            let tool = self
                .tool_calls
                .get_mut(&target)
                .expect("named tool call exists");
            if tool.id.is_empty() {
                tool.id = merged.id;
            }
            if tool.arguments.is_empty() {
                tool.arguments = merged.arguments;
            } else {
                tool.arguments.push_str(&merged.arguments);
            }
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
        if let Some(reasoning) = self.reasoning_content {
            message.insert("reasoning_content".to_string(), Value::String(reasoning));
        }
        if !self.annotations.is_empty() {
            message.insert(
                "annotations".to_string(),
                Value::Array(self.annotations),
            );
        }
        if !self.tool_calls.is_empty() {
            let mut calls = Vec::new();
            for tool_call in self.tool_calls.into_values() {
                if tool_call.name.is_empty() {
                    anyhow::bail!("Chat stream tool_call 缺少 function.name");
                }
                calls.push(json!({
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
                }));
            }
            if !calls.is_empty() {
                message.insert("tool_calls".to_string(), Value::Array(calls));
            }
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
    let retry_advice = prepared.retry_advice.clone();
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
        let (upstream_model, usage, incomplete_reason) =
            accumulator.finish_streaming(done, tool_bridge)?;
        if let Some(probe) = request_log_probe.as_ref()
            && !upstream_model.is_empty()
        {
            probe.observe_upstream_response_model(&upstream_model);
        }
        if output.output_order.is_empty() && output.tools.is_empty() {
            let events = output.ensure_message();
            output.write_events(downstream, events).await?;
        }
        output.finish(downstream, usage, incomplete_reason).await
    }
    .await;
    if let Err(error) = result {
        if error.is::<DownstreamClosed>() {
            return Err(error);
        }
        observe_upstream_stream_error(request_log_probe.as_ref(), &error, route);
        let failure = streaming_response_failure(&error, route, retry_advice);
        let _ = output
            .fail_with_error(
                downstream,
                failure.normalized_event()["response"]["error"].clone(),
            )
            .await;
        return Err(error);
    }
    Ok(())
}

/// `tool_calls` 增量用 `index` 定位槽位。
fn chat_stream_tool_slot(object: &serde_json::Map<String, Value>) -> usize {
    object
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .unwrap_or(0)
}

/// legacy `function_call` 增量没有索引语义，一次响应里最多只有一个函数调用，默认使用
/// 独立槽位；只有 0 号索引槽位已经建立、还在等名字时才并入它，见 [`chat_legacy_tool_slot`]。
const CHAT_STREAM_LEGACY_TOOL_SLOT: usize = usize::MAX;

/// 上游有时把同一次调用拆成两种形状：索引式增量先给出调用 ID 和参数，legacy 增量再补
/// 名字。此时 0 号槽位正等着名字，legacy 增量并入它，调用 ID 和名字才能落在同一次调用上。
/// 其余情况 legacy 增量使用独立槽位：0 号槽位已经带有别的名字，或者还没有索引式槽位，
/// 强行并入会把两个真实的调用拼成一个，静默丢掉其中一个工具。
pub(crate) fn chat_legacy_tool_slot(indexed_name: Option<&str>) -> usize {
    match indexed_name {
        Some("") => 0,
        _ => CHAT_STREAM_LEGACY_TOOL_SLOT,
    }
}

/// 判断两种形状是否指向同一次工具调用：恰好一个 legacy 槽位与一个索引式槽位，且其中
/// 只有一个已经拿到名字时，返回 `(保留槽位, 并入槽位)`。缺名字的槽位还没有向下游发出
/// 任何事件，并入不会与已下发的事件冲突；其他组合保持原样，由收尾逻辑决定是否报错。
pub(crate) fn chat_tool_merge_pair<T>(
    tools: &BTreeMap<usize, T>,
    name_is_empty: impl Fn(&T) -> bool,
) -> Option<(usize, usize)> {
    let mut legacy_named = Vec::new();
    let mut legacy_nameless = Vec::new();
    let mut indexed_named = Vec::new();
    let mut indexed_nameless = Vec::new();
    for (index, tool) in tools {
        match (*index == CHAT_STREAM_LEGACY_TOOL_SLOT, name_is_empty(tool)) {
            (true, false) => legacy_named.push(*index),
            (true, true) => legacy_nameless.push(*index),
            (false, false) => indexed_named.push(*index),
            (false, true) => indexed_nameless.push(*index),
        }
    }
    match (
        legacy_named.as_slice(),
        legacy_nameless.as_slice(),
        indexed_named.as_slice(),
        indexed_nameless.as_slice(),
    ) {
        ([legacy], [], [], [indexed]) => Some((*legacy, *indexed)),
        ([], [legacy], [indexed], []) => Some((*indexed, *legacy)),
        _ => None,
    }
}

fn validate_chat_stream_tool_type(call_type: Option<&Value>) -> Result<()> {
    // 部分上游会在工具参数增量中重复发送空类型，按字段省略处理。
    if let Some(call_type) = call_type.and_then(Value::as_str)
        && !call_type.trim().is_empty()
        && call_type != "function"
    {
        anyhow::bail!("Chat stream tool_call 类型 {call_type} 不受支持");
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
    check_upstream_stream_failure(event)?;
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
        if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str) {
            let events = output.reasoning_delta(reasoning);
            output.write_events(downstream, events).await?;
        }
        if let Some(content) = delta.get("content") {
            match content {
                Value::String(text) => output.emit_text_delta(downstream, text).await?,
                other => {
                    let text = chat_message_content_text(other);
                    output.emit_text_delta(downstream, &text).await?;
                }
            }
        }
        if let Some(refusal) = delta.get("refusal").and_then(Value::as_str) {
            output.emit_refusal_delta(downstream, refusal).await?;
        }
        if let Some(annotations) = delta.get("annotations").and_then(Value::as_array) {
            let normalized = annotations
                .iter()
                .filter_map(normalize_chat_annotation)
                .collect::<Vec<_>>();
            if !normalized.is_empty() {
                let added = output.ensure_message();
                output.write_events(downstream, added).await?;
                if let Some(message) = output.message.as_mut() {
                    message.annotations.extend(normalized);
                }
            }
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
                validate_chat_stream_tool_type(tool_call.get("type"))?;
                let index = chat_stream_tool_slot(tool_call);
                let function = tool_call.get("function").and_then(Value::as_object);
                let arguments_delta = function
                    .and_then(|function| function.get("arguments"))
                    .and_then(|arguments| json_value_as_chat_string(Some(arguments)));
                output
                    .emit_tool_delta(
                        downstream,
                        index,
                        tool_call.get("id").and_then(Value::as_str),
                        function
                            .and_then(|function| function.get("name"))
                            .and_then(Value::as_str),
                        arguments_delta.as_deref(),
                        None,
                    )
                    .await?;
            }
        }
        if let Some(function_call) = delta.get("function_call") {
            let function_call = function_call
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("Chat stream function_call delta 必须是对象"))?;
            let arguments_delta = function_call
                .get("arguments")
                .and_then(|arguments| json_value_as_chat_string(Some(arguments)));
            output
                .emit_tool_delta(
                    downstream,
                    chat_legacy_tool_slot(output.tools.get(&0).map(|tool| tool.name.as_str())),
                    None,
                    function_call.get("name").and_then(Value::as_str),
                    arguments_delta.as_deref(),
                    None,
                )
                .await?;
        }
    }
    Ok(())
}

pub(crate) fn parse_chat_completion_sse_bytes(bytes: &[u8], model: &str) -> Result<Value> {
    let mut accumulator = ChatSseAccumulator::new(model);
    parse_sse_frames(bytes, &mut accumulator)?;
    let done = accumulator.done;
    accumulator.into_chat_completion(done)
}
