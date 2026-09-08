use super::*;

#[derive(Clone, Copy, Debug)]
pub(crate) enum StreamOutputKind {
    Message,
    Tool(usize),
}

#[derive(Debug)]
pub(crate) struct ResponsesStreamMessage {
    pub(crate) item_id: String,
    pub(crate) output_index: usize,
    pub(crate) text: String,
    pub(crate) refusal: String,
    pub(crate) text_content_index: Option<usize>,
    pub(crate) refusal_content_index: Option<usize>,
    pub(crate) next_content_index: usize,
}

#[derive(Debug)]
pub(crate) struct ResponsesStreamTool {
    pub(crate) item_id: String,
    pub(crate) output_index: usize,
    pub(crate) call_id: String,
    pub(crate) name: String,
    pub(crate) response_name: Option<ResponsesToolName>,
    pub(crate) arguments: String,
    pub(crate) response_input: Option<String>,
    pub(crate) emitted_arguments: usize,
    pub(crate) fallback_arguments: Option<String>,
    pub(crate) added: bool,
}

#[derive(Debug)]
pub(crate) struct ResponsesSseState<'a> {
    pub(crate) response_id: String,
    pub(crate) model: String,
    pub(crate) tool_bridge: &'a ResponsesToolBridge,
    pub(crate) created_at: i64,
    pub(crate) next_output_index: usize,
    pub(crate) message: Option<ResponsesStreamMessage>,
    pub(crate) tools: BTreeMap<usize, ResponsesStreamTool>,
    pub(crate) output_order: Vec<StreamOutputKind>,
    pub(crate) terminal_started: bool,
}

impl<'a> ResponsesSseState<'a> {
    pub(crate) fn new(model: &str, tool_bridge: &'a ResponsesToolBridge) -> Self {
        Self {
            response_id: format!("resp_codey_{}", Uuid::new_v4()),
            model: model.to_string(),
            tool_bridge,
            created_at: current_unix_timestamp(),
            next_output_index: 0,
            message: None,
            tools: BTreeMap::new(),
            output_order: Vec::new(),
            terminal_started: false,
        }
    }

    pub(crate) async fn start<D>(&self, downstream: &mut D) -> Result<()>
    where
        D: ResponsesDownstream + ?Sized,
    {
        downstream.start_event_stream().await?;
        downstream
            .write_event(&json!({
                "type":"response.created",
                "response":{
                    "id":self.response_id,
                    "object":"response",
                    "created_at":self.created_at,
                    "status":"in_progress",
                    "model":self.model,
                    "output":[],
                    "error":Value::Null,
                    "incomplete_details":Value::Null,
                }
            }))
            .await
    }

    pub(crate) fn ensure_message(&mut self) -> Vec<Value> {
        if self.message.is_some() {
            return Vec::new();
        }
        let output_index = self.next_output_index;
        self.next_output_index += 1;
        let item_id = format!("msg_codey_{}", Uuid::new_v4());
        self.message = Some(ResponsesStreamMessage {
            item_id: item_id.clone(),
            output_index,
            text: String::new(),
            refusal: String::new(),
            text_content_index: None,
            refusal_content_index: None,
            next_content_index: 0,
        });
        self.output_order.push(StreamOutputKind::Message);
        vec![json!({
            "type":"response.output_item.added",
            "response_id":self.response_id,
            "output_index":output_index,
            "item":{
                "id":item_id,
                "type":"message",
                "status":"in_progress",
                "role":"assistant",
                "content":[],
            }
        })]
    }

    pub(crate) fn text_delta(&mut self, delta: &str) -> Vec<Value> {
        if delta.is_empty() {
            return Vec::new();
        }
        let mut events = self.ensure_message();
        let response_id = self.response_id.clone();
        let message = self
            .message
            .as_mut()
            .expect("message state must exist after ensure_message");
        let content_index = *message.text_content_index.get_or_insert_with(|| {
            let index = message.next_content_index;
            message.next_content_index += 1;
            index
        });
        if message.text.is_empty() {
            events.push(json!({
                "type":"response.content_part.added",
                "response_id":response_id,
                "item_id":message.item_id,
                "output_index":message.output_index,
                "content_index":content_index,
                "part":{"type":"output_text","text":"","annotations":[]},
            }));
        }
        message.text.push_str(delta);
        events.push(json!({
            "type":"response.output_text.delta",
            "response_id":response_id,
            "item_id":message.item_id,
            "output_index":message.output_index,
            "content_index":content_index,
            "delta":delta,
        }));
        events
    }

    pub(crate) fn refusal_delta(&mut self, delta: &str) -> Vec<Value> {
        if delta.is_empty() {
            return Vec::new();
        }
        let mut events = self.ensure_message();
        let response_id = self.response_id.clone();
        let message = self
            .message
            .as_mut()
            .expect("message state must exist after ensure_message");
        let content_index = *message.refusal_content_index.get_or_insert_with(|| {
            let index = message.next_content_index;
            message.next_content_index += 1;
            index
        });
        if message.refusal.is_empty() {
            events.push(json!({
                "type":"response.content_part.added",
                "response_id":response_id,
                "item_id":message.item_id,
                "output_index":message.output_index,
                "content_index":content_index,
                "part":{"type":"refusal","refusal":""},
            }));
        }
        message.refusal.push_str(delta);
        events.push(json!({
            "type":"response.refusal.delta",
            "response_id":response_id,
            "item_id":message.item_id,
            "output_index":message.output_index,
            "content_index":content_index,
            "delta":delta,
        }));
        events
    }

    pub(crate) fn tool_delta(
        &mut self,
        upstream_index: usize,
        call_id: Option<&str>,
        name_delta: Option<&str>,
        arguments_delta: Option<&str>,
        fallback_arguments: Option<String>,
    ) -> Result<Vec<Value>> {
        if !self.tools.contains_key(&upstream_index) {
            let output_index = self.next_output_index;
            self.next_output_index += 1;
            self.tools.insert(
                upstream_index,
                ResponsesStreamTool {
                    item_id: String::new(),
                    output_index,
                    call_id: String::new(),
                    name: String::new(),
                    response_name: None,
                    arguments: String::new(),
                    response_input: None,
                    emitted_arguments: 0,
                    fallback_arguments: None,
                    added: false,
                },
            );
            self.output_order
                .push(StreamOutputKind::Tool(upstream_index));
        }
        let response_id = self.response_id.clone();
        let tool = self
            .tools
            .get_mut(&upstream_index)
            .expect("tool state must exist after insertion");
        if tool.call_id.is_empty()
            && let Some(call_id) = call_id.filter(|value| !value.is_empty())
        {
            tool.call_id = call_id.to_string();
        }
        if let Some(name_delta) = name_delta {
            tool.name.push_str(name_delta);
        }
        if tool.fallback_arguments.is_none() {
            tool.fallback_arguments = fallback_arguments;
        }
        if let Some(arguments_delta) = arguments_delta {
            tool.arguments.push_str(arguments_delta);
        }
        if tool.response_name.is_none()
            && !tool.name.is_empty()
            && let Some(response_name) = self
                .tool_bridge
                .restore_stream_upstream_name(&tool.name, false)?
        {
            tool.response_name = Some(response_name);
        }

        let mut events = Vec::new();
        if !tool.added
            && let Some(response_name) = tool.response_name.as_ref()
        {
            if tool.call_id.is_empty() {
                tool.call_id = format!("call_codey_{}", Uuid::new_v4());
            }
            if tool.item_id.is_empty() {
                tool.item_id = responses_tool_call_item_id(response_name);
            }
            tool.added = true;
            events.push(json!({
                "type":"response.output_item.added",
                "response_id":response_id,
                "output_index":tool.output_index,
                "item":responses_tool_call_item_with_id(
                    response_name,
                    tool.item_id.clone(),
                    tool.call_id.clone(),
                    responses_tool_call_initial_payload(response_name),
                    "in_progress",
                )
            }));
        }
        if tool.added
            && tool
                .response_name
                .as_ref()
                .is_some_and(ResponsesToolName::is_function)
            && tool.emitted_arguments < tool.arguments.len()
        {
            let delta = &tool.arguments[tool.emitted_arguments..];
            events.push(json!({
                "type":"response.function_call_arguments.delta",
                "response_id":response_id,
                "item_id":tool.item_id,
                "output_index":tool.output_index,
                "delta":delta,
            }));
            tool.emitted_arguments = tool.arguments.len();
        }
        Ok(events)
    }

    pub(crate) async fn write_events<D>(&self, downstream: &mut D, events: Vec<Value>) -> Result<()>
    where
        D: ResponsesDownstream + ?Sized,
    {
        for event in events {
            downstream.write_event(&event).await?;
        }
        Ok(())
    }

    pub(crate) async fn finish<D>(
        &mut self,
        downstream: &mut D,
        usage: Option<Value>,
        incomplete_reason: Option<&str>,
    ) -> Result<()>
    where
        D: ResponsesDownstream + ?Sized,
    {
        if self.terminal_started {
            return Ok(());
        }
        let mut events = Vec::new();
        if let Some(message) = self.message.as_ref() {
            if let Some(content_index) = message.text_content_index {
                events.push(json!({
                    "type":"response.output_text.done",
                    "response_id":self.response_id,
                    "item_id":message.item_id,
                    "output_index":message.output_index,
                    "content_index":content_index,
                    "text":message.text,
                }));
                events.push(json!({
                    "type":"response.content_part.done",
                    "response_id":self.response_id,
                    "item_id":message.item_id,
                    "output_index":message.output_index,
                    "content_index":content_index,
                    "part":{"type":"output_text","text":message.text,"annotations":[]},
                }));
            }
            if let Some(content_index) = message.refusal_content_index {
                events.push(json!({
                    "type":"response.refusal.done",
                    "response_id":self.response_id,
                    "item_id":message.item_id,
                    "output_index":message.output_index,
                    "content_index":content_index,
                    "refusal":message.refusal,
                }));
                events.push(json!({
                    "type":"response.content_part.done",
                    "response_id":self.response_id,
                    "item_id":message.item_id,
                    "output_index":message.output_index,
                    "content_index":content_index,
                    "part":{"type":"refusal","refusal":message.refusal},
                }));
            }
            events.push(json!({
                "type":"response.output_item.done",
                "response_id":self.response_id,
                "output_index":message.output_index,
                "item":stream_message_item(message),
            }));
        }

        for tool in self.tools.values_mut() {
            if tool.arguments.is_empty()
                && let Some(fallback) = tool.fallback_arguments.take()
            {
                tool.arguments = fallback;
            }
            if tool.name.is_empty() {
                anyhow::bail!("流式 function_call 缺少 name");
            }
            if tool.response_name.is_none() {
                tool.response_name = self
                    .tool_bridge
                    .restore_stream_upstream_name(&tool.name, true)?;
            }
            if tool.call_id.is_empty() {
                tool.call_id = format!("call_codey_{}", Uuid::new_v4());
            }
            let response_name = tool
                .response_name
                .as_ref()
                .expect("response tool name must be restored before serialization");
            if response_name.is_custom() && tool.response_input.is_none() {
                tool.response_input = Some(custom_tool_input_from_arguments(
                    &tool.arguments,
                    "流式 custom function arguments",
                )?);
            }
            if tool.item_id.is_empty() {
                tool.item_id = responses_tool_call_item_id(response_name);
            }
            if !tool.added {
                tool.added = true;
                events.push(json!({
                    "type":"response.output_item.added",
                    "response_id":self.response_id,
                    "output_index":tool.output_index,
                    "item":responses_tool_call_item_with_id(
                        response_name,
                        tool.item_id.clone(),
                        tool.call_id.clone(),
                        responses_tool_call_initial_payload(response_name),
                        "in_progress",
                    )
                }));
            }
            if response_name.is_custom() {
                let input = tool.response_input.as_deref().unwrap_or_default();
                if !input.is_empty() {
                    events.push(json!({
                        "type":"response.custom_tool_call_input.delta",
                        "response_id":self.response_id,
                        "item_id":tool.item_id,
                        "output_index":tool.output_index,
                        "delta":input,
                    }));
                }
                events.push(json!({
                    "type":"response.custom_tool_call_input.done",
                    "response_id":self.response_id,
                    "item_id":tool.item_id,
                    "output_index":tool.output_index,
                    "input":input,
                }));
            } else if response_name.is_function() {
                if tool.emitted_arguments < tool.arguments.len() {
                    let delta = &tool.arguments[tool.emitted_arguments..];
                    events.push(json!({
                        "type":"response.function_call_arguments.delta",
                        "response_id":self.response_id,
                        "item_id":tool.item_id,
                        "output_index":tool.output_index,
                        "delta":delta,
                    }));
                    tool.emitted_arguments = tool.arguments.len();
                }
                events.push(json!({
                    "type":"response.function_call_arguments.done",
                    "response_id":self.response_id,
                    "item_id":tool.item_id,
                    "output_index":tool.output_index,
                    "arguments":tool.arguments,
                }));
            }
            events.push(json!({
                "type":"response.output_item.done",
                "response_id":self.response_id,
                "output_index":tool.output_index,
                "item":stream_tool_item(tool)?,
            }));
        }

        let output = self
            .output_order
            .iter()
            .filter_map(|kind| match kind {
                StreamOutputKind::Message => self
                    .message
                    .as_ref()
                    .map(|message| Ok(stream_message_item(message))),
                StreamOutputKind::Tool(index) => self.tools.get(index).map(stream_tool_item),
            })
            .collect::<Result<Vec<_>>>()?;
        let status = if incomplete_reason.is_some() {
            "incomplete"
        } else {
            "completed"
        };
        downstream.remember_adapted_response(&self.response_id, &output)?;
        let mut response = json!({
            "id":self.response_id,
            "object":"response",
            "created_at":self.created_at,
            "status":status,
            "model":self.model,
            "output":output,
            "output_text":self.message.as_ref().map(|message| message.text.as_str()).unwrap_or(""),
            "error":Value::Null,
            "incomplete_details":incomplete_reason.map(|reason| json!({"reason":reason})),
        });
        if let Some(usage) = usage {
            response
                .as_object_mut()
                .expect("streaming Responses wrapper must be an object")
                .insert("usage".to_string(), usage);
        }
        let terminal_type = if incomplete_reason.is_some() {
            "response.incomplete"
        } else {
            "response.completed"
        };
        self.write_events(downstream, events).await?;
        self.terminal_started = true;
        downstream
            .write_event(&json!({"type":terminal_type,"response":response}))
            .await?;
        downstream.finish_event_stream().await
    }

    pub(crate) async fn fail<D>(
        &mut self,
        downstream: &mut D,
        code: &str,
        message: &str,
    ) -> Result<()>
    where
        D: ResponsesDownstream + ?Sized,
    {
        if self.terminal_started {
            return Ok(());
        }
        self.terminal_started = true;
        downstream
            .write_event(&json!({
                "type":"response.failed",
                "response":{
                    "id":self.response_id,
                    "object":"response",
                    "created_at":self.created_at,
                    "status":"failed",
                    "model":self.model,
                    "output":[],
                    "error":{
                        "type":"codey_route_error",
                        "code":code,
                        "message":message,
                    },
                    "incomplete_details":Value::Null,
                }
            }))
            .await?;
        downstream.finish_event_stream().await
    }
}

pub(crate) fn stream_message_item(message: &ResponsesStreamMessage) -> Value {
    let mut content = Vec::new();
    if message.text_content_index.is_some() {
        content.push((
            message.text_content_index.unwrap_or_default(),
            json!({"type":"output_text","text":message.text,"annotations":[]}),
        ));
    }
    if message.refusal_content_index.is_some() {
        content.push((
            message.refusal_content_index.unwrap_or_default(),
            json!({"type":"refusal","refusal":message.refusal}),
        ));
    }
    content.sort_by_key(|(index, _)| *index);
    json!({
        "id":message.item_id,
        "type":"message",
        "status":"completed",
        "role":"assistant",
        "content":content.into_iter().map(|(_, part)| part).collect::<Vec<_>>(),
    })
}

pub(crate) fn responses_tool_call_initial_payload(tool_name: &ResponsesToolName) -> Value {
    if tool_name.is_tool_search() {
        Value::Object(serde_json::Map::new())
    } else {
        Value::String(String::new())
    }
}

pub(crate) fn stream_tool_item(tool: &ResponsesStreamTool) -> Result<Value> {
    let response_name = tool
        .response_name
        .as_ref()
        .expect("stream tool response name must be restored before serialization");
    let payload = if response_name.is_custom() {
        Value::String(tool.response_input.clone().unwrap_or_default())
    } else if response_name.is_tool_search() {
        let arguments = serde_json::from_str::<Value>(&tool.arguments)
            .context("流式 tool_search function arguments 不是有效 JSON")?;
        if !arguments.is_object() {
            anyhow::bail!("流式 tool_search function arguments 必须是 JSON 对象");
        }
        arguments
    } else {
        Value::String(tool.arguments.clone())
    };
    Ok(responses_tool_call_item_with_id(
        response_name,
        tool.item_id.clone(),
        tool.call_id.clone(),
        payload,
        "completed",
    ))
}

pub(crate) async fn write_responses_sse_event(stream: &mut TcpStream, event: &Value) -> Result<()> {
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("response.event");
    let payload = format!(
        "event: {event_type}\ndata: {}\n\n",
        serde_json::to_string(event).context("序列化 Responses SSE 事件失败")?
    );
    write_chunked_frame(stream, payload.as_bytes(), "写入 Responses SSE 事件失败").await
}

pub(crate) async fn finish_chunked_response(stream: &mut TcpStream) -> Result<()> {
    write_all_with_timeout(stream, b"0\r\n\r\n", "结束 Responses SSE 流失败").await
}

pub(crate) fn responses_event_sequence(response: &Value) -> Result<Vec<Value>> {
    let response_id = response
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("resp_codey");
    let mut created = response.clone();
    if let Some(created) = created.as_object_mut() {
        created.insert(
            "status".to_string(),
            Value::String("in_progress".to_string()),
        );
        created.insert("output".to_string(), Value::Array(Vec::new()));
        created.remove("usage");
    }
    let mut events = vec![json!({"type":"response.created","response":created})];
    for (output_index, item) in response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let item_id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("item_codey");
        let mut added_item = item.clone();
        if let Some(added_item) = added_item.as_object_mut() {
            added_item.insert(
                "status".to_string(),
                Value::String("in_progress".to_string()),
            );
            match added_item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    added_item.insert("content".to_string(), Value::Array(Vec::new()));
                }
                Some("function_call") => {
                    added_item.insert("arguments".to_string(), Value::String(String::new()));
                }
                Some("tool_search_call") => {
                    added_item.insert(
                        "arguments".to_string(),
                        Value::Object(serde_json::Map::new()),
                    );
                }
                Some("custom_tool_call") => {
                    added_item.insert("input".to_string(), Value::String(String::new()));
                }
                _ => {}
            }
        }
        events.push(json!({
            "type":"response.output_item.added",
            "response_id": response_id,
            "output_index": output_index,
            "item": added_item,
        }));
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                append_message_sse_events(&mut events, response_id, item_id, output_index, item)
            }
            Some("function_call") => {
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                events.push(json!({
                    "type":"response.function_call_arguments.delta",
                    "response_id": response_id,
                    "item_id": item_id,
                    "output_index": output_index,
                    "delta": arguments,
                }));
                events.push(json!({
                    "type":"response.function_call_arguments.done",
                    "response_id": response_id,
                    "item_id": item_id,
                    "output_index": output_index,
                    "arguments": arguments,
                }));
            }
            Some("custom_tool_call") => {
                let input = item.get("input").and_then(Value::as_str).unwrap_or("");
                if !input.is_empty() {
                    events.push(json!({
                        "type":"response.custom_tool_call_input.delta",
                        "response_id": response_id,
                        "item_id": item_id,
                        "output_index": output_index,
                        "delta": input,
                    }));
                }
                events.push(json!({
                    "type":"response.custom_tool_call_input.done",
                    "response_id": response_id,
                    "item_id": item_id,
                    "output_index": output_index,
                    "input": input,
                }));
            }
            _ => {}
        }
        events.push(json!({
            "type":"response.output_item.done",
            "response_id": response_id,
            "output_index": output_index,
            "item": item,
        }));
    }
    let terminal_event = if response.get("status").and_then(Value::as_str) == Some("incomplete") {
        "response.incomplete"
    } else {
        "response.completed"
    };
    events.push(json!({"type":terminal_event,"response":response}));
    Ok(events)
}

pub(crate) async fn write_responses_response_as_events<D>(
    downstream: &mut D,
    response: &Value,
) -> Result<()>
where
    D: ResponsesDownstream + ?Sized,
{
    downstream.start_event_stream().await?;
    for event in responses_event_sequence(response)? {
        downstream.write_event(&event).await?;
    }
    downstream.finish_event_stream().await
}

pub(crate) fn append_message_sse_events(
    events: &mut Vec<Value>,
    response_id: &str,
    item_id: &str,
    output_index: usize,
    item: &Value,
) {
    for (content_index, part) in item
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let part_type = part.get("type").and_then(Value::as_str);
        let empty_part = match part_type {
            Some("refusal") => json!({"type":"refusal","refusal":""}),
            _ => json!({"type":"output_text","text":"","annotations":[]}),
        };
        events.push(json!({
            "type":"response.content_part.added",
            "response_id": response_id,
            "item_id": item_id,
            "output_index": output_index,
            "content_index": content_index,
            "part": empty_part,
        }));
        if part_type == Some("refusal") {
            let refusal = part.get("refusal").and_then(Value::as_str).unwrap_or("");
            events.push(json!({
                "type":"response.refusal.delta",
                "response_id": response_id,
                "item_id": item_id,
                "output_index": output_index,
                "content_index": content_index,
                "delta": refusal,
            }));
            events.push(json!({
                "type":"response.refusal.done",
                "response_id": response_id,
                "item_id": item_id,
                "output_index": output_index,
                "content_index": content_index,
                "refusal": refusal,
            }));
        } else {
            let text = part.get("text").and_then(Value::as_str).unwrap_or("");
            events.push(json!({
                "type":"response.output_text.delta",
                "response_id": response_id,
                "item_id": item_id,
                "output_index": output_index,
                "content_index": content_index,
                "delta": text,
            }));
            events.push(json!({
                "type":"response.output_text.done",
                "response_id": response_id,
                "item_id": item_id,
                "output_index": output_index,
                "content_index": content_index,
                "text": text,
            }));
        }
        events.push(json!({
            "type":"response.content_part.done",
            "response_id": response_id,
            "item_id": item_id,
            "output_index": output_index,
            "content_index": content_index,
            "part": part,
        }));
    }
}

pub(crate) fn reason_phrase(status: u16) -> &'static str {
    // Clients key on the numeric status, but a reason phrase such as
    // `429 OK` still misleads log readers and proxies that surface it.
    WebSocketStatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("Unknown")
}
