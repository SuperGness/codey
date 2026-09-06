use super::*;

#[cfg(test)]
pub(crate) fn responses_to_anthropic_messages_body(body: &Value) -> Result<Value> {
    Ok(responses_to_anthropic_messages_request(body)?.body)
}

pub(crate) fn responses_to_anthropic_messages_request(
    body: &Value,
) -> Result<ConvertedResponsesRequest> {
    // Reuse the normalized Chat message representation so Responses message,
    // image, function-call and function-result variants have one parser. The
    // second stage below changes only the Anthropic-specific wire semantics.
    let ConvertedResponsesRequest {
        body: chat,
        tool_bridge,
    } = responses_to_chat_completions_request(body)?;
    let mut chat = match chat {
        Value::Object(chat) => chat,
        _ => anyhow::bail!("Responses 请求无法归一化为消息对象"),
    };
    for unsupported in [
        "presence_penalty",
        "frequency_penalty",
        "seed",
        "logit_bias",
        "logprobs",
        "top_logprobs",
        "web_search_options",
    ] {
        if chat.contains_key(unsupported) {
            anyhow::bail!("Anthropic Messages 不支持 Responses 字段 {unsupported}");
        }
    }
    if let Some(response_format) = chat.get("response_format")
        && response_format.get("type").and_then(Value::as_str) != Some("text")
    {
        anyhow::bail!("Anthropic Messages 暂不支持当前 Responses 结构化输出格式");
    }

    let model = chat
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("缺少 model 字段"))?
        .to_string();
    let mut system_parts = Vec::new();
    let mut messages = Vec::new();
    let chat_messages = match chat.remove("messages") {
        Some(Value::Array(messages)) => messages,
        _ => Vec::new(),
    };
    for message in chat_messages {
        append_anthropic_message_from_chat(&message, &mut system_parts, &mut messages)?;
    }
    if messages.is_empty() {
        anyhow::bail!("缺少可转换为 Anthropic Messages messages 的 input");
    }

    let max_tokens = chat
        .get("max_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_ANTHROPIC_MAX_TOKENS);
    if max_tokens == 0 {
        anyhow::bail!("max_output_tokens 必须大于 0");
    }
    let mut anthropic = serde_json::Map::from_iter([
        ("model".to_string(), Value::String(model)),
        ("messages".to_string(), Value::Array(messages)),
        ("max_tokens".to_string(), Value::Number(max_tokens.into())),
        (
            "stream".to_string(),
            Value::Bool(chat.get("stream").and_then(Value::as_bool).unwrap_or(false)),
        ),
    ]);
    if !system_parts.is_empty() {
        anthropic.insert(
            "system".to_string(),
            Value::String(system_parts.join("\n\n")),
        );
    }
    copy_number_or_string_field(&chat, &mut anthropic, "temperature", "temperature");
    copy_number_or_string_field(&chat, &mut anthropic, "top_p", "top_p");
    if let Some(top_k) = body.get("top_k") {
        if !top_k.is_number() {
            anyhow::bail!("top_k 必须是数字");
        }
        anthropic.insert("top_k".to_string(), top_k.clone());
    }
    if let Some(stop) = chat.get("stop") {
        anthropic.insert(
            "stop_sequences".to_string(),
            anthropic_stop_sequences(stop)?,
        );
    }
    if let Some(user_id) = chat
        .get("user")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        anthropic.insert("metadata".to_string(), json!({"user_id":user_id}));
    }
    if let Some(effort) = chat
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .map(normalize_anthropic_effort)
    {
        anthropic.insert("output_config".to_string(), json!({"effort":effort}));
    }

    let parallel_tool_calls = chat
        .get("parallel_tool_calls")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if let Some(tools) = chat.remove("tools") {
        let converted_tools = chat_tools_to_anthropic_tools(&tools)?;
        let mut tool_choice = chat
            .get("tool_choice")
            .map(chat_tool_choice_to_anthropic_tool_choice)
            .transpose()?;
        if tool_choice.is_none()
            && let Some(function_call) = chat.get("function_call")
        {
            tool_choice = Some(chat_function_call_to_anthropic_tool_choice(function_call)?);
        }
        if tool_choice
            .as_ref()
            .is_some_and(|choice| choice.get("type").and_then(Value::as_str) == Some("none"))
        {
            tool_choice = None;
        } else {
            anthropic.insert("tools".to_string(), converted_tools);
            if !parallel_tool_calls {
                let choice = tool_choice.get_or_insert_with(|| json!({"type":"auto"}));
                choice
                    .as_object_mut()
                    .expect("Anthropic tool choice must be an object")
                    .insert("disable_parallel_tool_use".to_string(), Value::Bool(true));
            }
        }
        if let Some(tool_choice) = tool_choice {
            anthropic.insert("tool_choice".to_string(), tool_choice);
        }
    }
    Ok(ConvertedResponsesRequest {
        body: Value::Object(anthropic),
        tool_bridge,
    })
}

pub(crate) fn append_anthropic_message_from_chat(
    message: &Value,
    system_parts: &mut Vec<String>,
    messages: &mut Vec<Value>,
) -> Result<()> {
    let message = message
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("归一化消息必须是对象"))?;
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("归一化消息缺少 role"))?;
    if role == "system" {
        let text = message
            .get("content")
            .map(chat_message_content_text)
            .unwrap_or_default();
        if !text.is_empty() {
            system_parts.push(text);
        }
        return Ok(());
    }
    if role == "tool" {
        let call_id = message
            .get("tool_call_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("tool 消息缺少 tool_call_id"))?;
        let content = json_value_as_chat_string(message.get("content")).unwrap_or_default();
        return push_anthropic_message(
            messages,
            "user",
            vec![json!({
                "type":"tool_result",
                "tool_use_id":call_id,
                "content":content,
            })],
        );
    }
    if !matches!(role, "user" | "assistant") {
        anyhow::bail!("Anthropic Messages 不支持消息角色 {role}");
    }
    let mut blocks = message
        .get("content")
        .map(|content| chat_content_to_anthropic_blocks(content, role == "user"))
        .transpose()?
        .unwrap_or_default();
    if let Some(tool_calls) = message.get("tool_calls") {
        blocks.extend(chat_tool_calls_to_anthropic_blocks(tool_calls)?);
    }
    if let Some(function_call) = message.get("function_call") {
        blocks.extend(chat_legacy_function_call_to_anthropic_blocks(
            function_call,
        )?);
    }
    if blocks.is_empty() {
        return Ok(());
    }
    push_anthropic_message(messages, role, blocks)
}

pub(crate) fn push_anthropic_message(
    messages: &mut Vec<Value>,
    role: &str,
    blocks: Vec<Value>,
) -> Result<()> {
    if let Some(last) = messages.last_mut().and_then(Value::as_object_mut)
        && last.get("role").and_then(Value::as_str) == Some(role)
    {
        let content = last
            .get_mut("content")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| anyhow::anyhow!("Anthropic message.content 必须是数组"))?;
        content.extend(blocks);
        return Ok(());
    }
    messages.push(json!({"role":role,"content":blocks}));
    Ok(())
}

pub(crate) fn chat_content_to_anthropic_blocks(
    content: &Value,
    allow_images: bool,
) -> Result<Vec<Value>> {
    match content {
        Value::Null => Ok(Vec::new()),
        Value::String(text) => Ok(if text.is_empty() {
            Vec::new()
        } else {
            vec![json!({"type":"text","text":text})]
        }),
        Value::Array(parts) => {
            let mut blocks = Vec::new();
            for part in parts {
                let part = part
                    .as_object()
                    .ok_or_else(|| anyhow::anyhow!("消息 content 条目必须是对象"))?;
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let text = part
                            .get("text")
                            .and_then(Value::as_str)
                            .ok_or_else(|| anyhow::anyhow!("text content 缺少 text"))?;
                        blocks.push(json!({"type":"text","text":text}));
                    }
                    Some("image_url") if allow_images => {
                        let image_url = part
                            .get("image_url")
                            .ok_or_else(|| anyhow::anyhow!("image_url content 缺少 image_url"))?;
                        blocks.push(json!({
                            "type":"image",
                            "source":chat_image_url_to_anthropic_source(image_url)?,
                        }));
                    }
                    Some("image_url") => anyhow::bail!("Anthropic Messages 只允许用户消息包含图片"),
                    Some(part_type) => {
                        anyhow::bail!("消息 content 类型 {part_type} 不能转换为 Anthropic Messages")
                    }
                    None => anyhow::bail!("消息 content 条目缺少 type"),
                }
            }
            Ok(blocks)
        }
        _ => anyhow::bail!("消息 content 必须是字符串或数组"),
    }
}

pub(crate) fn chat_image_url_to_anthropic_source(image_url: &Value) -> Result<Value> {
    let url = match image_url {
        Value::String(url) => url.as_str(),
        Value::Object(object) => object
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("image_url 缺少 url"))?,
        _ => anyhow::bail!("image_url 必须是字符串或对象"),
    };
    if let Some(data) = url.strip_prefix("data:") {
        let (media_type, data) = data
            .split_once(";base64,")
            .ok_or_else(|| anyhow::anyhow!("Anthropic 图片 data URL 必须使用 base64"))?;
        if !matches!(
            media_type,
            "image/jpeg" | "image/png" | "image/gif" | "image/webp"
        ) {
            anyhow::bail!("Anthropic Messages 不支持图片类型 {media_type}");
        }
        if data.is_empty() {
            anyhow::bail!("Anthropic 图片 data URL 缺少数据");
        }
        return Ok(json!({"type":"base64","media_type":media_type,"data":data}));
    }
    let parsed = reqwest::Url::parse(url).context("图片 URL 格式无效")?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        anyhow::bail!("Anthropic 图片 URL 必须是 HTTP(S) 或 base64 data URL");
    }
    Ok(json!({"type":"url","url":url}))
}

pub(crate) fn chat_tool_calls_to_anthropic_blocks(tool_calls: &Value) -> Result<Vec<Value>> {
    let tool_calls = tool_calls
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("tool_calls 必须是数组"))?;
    tool_calls
        .iter()
        .map(|tool_call| {
            let tool_call = tool_call
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("tool_call 必须是对象"))?;
            let id = tool_call
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow::anyhow!("tool_call 缺少 id"))?;
            let function = tool_call
                .get("function")
                .and_then(Value::as_object)
                .ok_or_else(|| anyhow::anyhow!("tool_call 缺少 function"))?;
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow::anyhow!("tool_call.function 缺少 name"))?;
            let input = parse_anthropic_tool_input(function.get("arguments"))?;
            Ok(json!({"type":"tool_use","id":id,"name":name,"input":input}))
        })
        .collect()
}

pub(crate) fn chat_legacy_function_call_to_anthropic_blocks(
    function_call: &Value,
) -> Result<Vec<Value>> {
    let function_call = function_call
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("function_call 必须是对象"))?;
    let name = function_call
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("function_call 缺少 name"))?;
    Ok(vec![json!({
        "type":"tool_use",
        "id":format!("call_codey_{}", Uuid::new_v4()),
        "name":name,
        "input":parse_anthropic_tool_input(function_call.get("arguments"))?,
    })])
}

pub(crate) fn parse_anthropic_tool_input(arguments: Option<&Value>) -> Result<Value> {
    let input = match arguments {
        None | Some(Value::Null) => json!({}),
        Some(Value::Object(object)) => Value::Object(object.clone()),
        Some(Value::String(arguments)) if arguments.trim().is_empty() => json!({}),
        Some(Value::String(arguments)) => {
            serde_json::from_str::<Value>(arguments).context("工具调用 arguments 不是有效 JSON")?
        }
        Some(value) => value.clone(),
    };
    if !input.is_object() {
        anyhow::bail!("Anthropic tool_use.input 必须是 JSON 对象");
    }
    Ok(input)
}

pub(crate) fn chat_tools_to_anthropic_tools(tools: &Value) -> Result<Value> {
    let tools = tools
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("tools 必须是数组"))?;
    let mut converted = Vec::new();
    for tool in tools {
        let tool = tool
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("tool 条目必须是对象"))?;
        if tool.get("type").and_then(Value::as_str) != Some("function") {
            anyhow::bail!("Anthropic Messages 只支持 Responses function 工具");
        }
        let function = tool
            .get("function")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("function tool 缺少 function"))?;
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("function tool 缺少 name"))?;
        let input_schema = function
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({"type":"object","properties":{}}));
        if !input_schema.is_object() {
            anyhow::bail!("function tool.parameters 必须是 JSON Schema 对象");
        }
        let mut converted_tool = serde_json::Map::from_iter([
            ("name".to_string(), Value::String(name.to_string())),
            ("input_schema".to_string(), input_schema),
        ]);
        if let Some(description) = function.get("description").and_then(Value::as_str) {
            converted_tool.insert(
                "description".to_string(),
                Value::String(description.to_string()),
            );
        }
        converted.push(Value::Object(converted_tool));
    }
    Ok(Value::Array(converted))
}

pub(crate) fn chat_tool_choice_to_anthropic_tool_choice(tool_choice: &Value) -> Result<Value> {
    match tool_choice {
        Value::String(choice) => match choice.as_str() {
            "auto" => Ok(json!({"type":"auto"})),
            "required" => Ok(json!({"type":"any"})),
            "none" => Ok(json!({"type":"none"})),
            _ => anyhow::bail!("Anthropic Messages 不支持 tool_choice={choice}"),
        },
        Value::Object(object) => {
            let name = object
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow::anyhow!("function tool_choice 缺少 name"))?;
            Ok(json!({"type":"tool","name":name}))
        }
        _ => anyhow::bail!("tool_choice 必须是字符串或 function 对象"),
    }
}

pub(crate) fn chat_function_call_to_anthropic_tool_choice(function_call: &Value) -> Result<Value> {
    match function_call {
        Value::String(choice) if choice == "auto" => Ok(json!({"type":"auto"})),
        Value::String(choice) if choice == "none" => Ok(json!({"type":"none"})),
        Value::Object(function) => {
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow::anyhow!("function_call 缺少 name"))?;
            Ok(json!({"type":"tool","name":name}))
        }
        _ => anyhow::bail!("function_call 不能转换为 Anthropic tool_choice"),
    }
}

pub(crate) fn anthropic_stop_sequences(stop: &Value) -> Result<Value> {
    match stop {
        Value::String(stop) => Ok(Value::Array(vec![Value::String(stop.clone())])),
        Value::Array(stops) if stops.iter().all(Value::is_string) => {
            Ok(Value::Array(stops.clone()))
        }
        _ => anyhow::bail!("stop 必须是字符串或字符串数组"),
    }
}

pub(crate) fn normalize_anthropic_effort(effort: &str) -> &'static str {
    let effort = effort.trim();
    if effort.eq_ignore_ascii_case("low") || effort.eq_ignore_ascii_case("minimal") {
        "low"
    } else if effort.eq_ignore_ascii_case("medium") {
        "medium"
    } else if effort.eq_ignore_ascii_case("max")
        || effort.eq_ignore_ascii_case("xhigh")
        || effort.eq_ignore_ascii_case("ultra")
    {
        "max"
    } else {
        "high"
    }
}

pub(crate) fn responses_text_format_to_chat_response_format(format: &Value) -> Result<Value> {
    let object = format
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("text.format 必须是对象"))?;
    match object.get("type").and_then(Value::as_str) {
        Some("text") => Ok(json!({"type":"text"})),
        Some("json_object") => Ok(json!({"type":"json_object"})),
        Some("json_schema") => {
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| anyhow::anyhow!("text.format.json_schema 缺少 name"))?;
            let schema = object
                .get("schema")
                .ok_or_else(|| anyhow::anyhow!("text.format.json_schema 缺少 schema"))?;
            let mut json_schema = serde_json::Map::from_iter([
                ("name".to_string(), Value::String(name.to_string())),
                ("schema".to_string(), schema.clone()),
            ]);
            for field in ["description", "strict"] {
                if let Some(value) = object.get(field) {
                    json_schema.insert(field.to_string(), value.clone());
                }
            }
            Ok(json!({
                "type": "json_schema",
                "json_schema": Value::Object(json_schema),
            }))
        }
        Some(format_type) => {
            anyhow::bail!("Responses text.format 类型 {format_type} 不能转换为 Chat Completions")
        }
        None => anyhow::bail!("text.format 缺少 type"),
    }
}
