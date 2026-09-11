use super::*;

pub(crate) fn append_chat_messages_from_responses_input(
    input: Option<&Value>,
    messages: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    let Some(input) = input else {
        return Ok(());
    };
    match input {
        Value::String(text) => push_chat_text_message(messages, "user", text),
        Value::Array(items) => {
            for item in items {
                append_chat_message_item(item, messages, tool_bridge)?;
            }
            Ok(())
        }
        Value::Object(_) => append_chat_message_item(input, messages, tool_bridge),
        _ => anyhow::bail!("input 必须是字符串、对象或数组"),
    }
}

pub(crate) fn append_chat_message_item(
    item: &Value,
    messages: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    match item {
        Value::String(text) => push_chat_text_message(messages, "user", text),
        Value::Object(object) => match object.get("type").and_then(Value::as_str) {
            Some("compaction" | "compaction_trigger") => anyhow::bail!(
                "context_not_portable: compaction 历史不能转换到当前线路；请回到原线路完成本地摘要后再切换"
            ),
            Some("message") => append_responses_message_object(object, messages, tool_bridge),
            Some("reasoning") => {
                // Only raw reasoning is replayable; summaries and encrypted provider state are not.
                if let Some(parts) = object.get("content").and_then(Value::as_array) {
                    let mut reasoning = String::new();
                    let mut present = false;
                    for part in parts {
                        if part.get("type").and_then(Value::as_str) == Some("reasoning_text")
                            && let Some(text) = part.get("text").and_then(Value::as_str)
                        {
                            reasoning.push_str(text);
                            present = true;
                        }
                    }
                    if present {
                        messages.push(json!({
                            "role":"assistant", "content":Value::Null,
                            "reasoning_content":reasoning,
                        }));
                    }
                }
                Ok(())
            }
            Some("agent_message") => {
                append_responses_agent_message_object(object, messages, tool_bridge)
            }
            None if looks_like_message_object(object) => {
                append_responses_message_object(object, messages, tool_bridge)
            }
            Some("function_call") => {
                append_responses_function_call_item(object, messages, tool_bridge)
            }
            Some("function_call_output") => {
                append_responses_tool_call_output_item(object, messages, "function_call_output")
            }
            Some("custom_tool_call") => {
                append_responses_custom_tool_call_item(object, messages, tool_bridge)
            }
            Some("custom_tool_call_output") => {
                append_responses_tool_call_output_item(object, messages, "custom_tool_call_output")
            }
            Some("tool_search_call") => {
                append_responses_tool_search_call_item(object, messages, tool_bridge)
            }
            Some("tool_search_output") => {
                append_responses_tool_search_output_item(object, messages)
            }
            Some("web_search_call")
                if object.get("status").and_then(Value::as_str) == Some("completed") =>
            {
                Ok(())
            }
            Some(item_type) if is_opaque_responses_input_item_type(item_type) => Ok(()),
            Some("input_text" | "output_text" | "text" | "input_image" | "image_url") => {
                append_single_content_part_as_user_message(item, messages)
            }
            Some(item_type) => anyhow::bail!(
                "Responses input item 类型 {item_type} 不能无损转换为 Chat Completions message"
            ),
            None => anyhow::bail!("Responses input item 缺少可转换的 role/content/type 字段"),
        },
        _ => anyhow::bail!("Responses input 数组只能包含字符串或对象"),
    }
}

pub(crate) fn looks_like_message_object(object: &serde_json::Map<String, Value>) -> bool {
    object.contains_key("role")
        || object.contains_key("content")
        || object.contains_key("tool_calls")
        || object.contains_key("function_call")
}

pub(crate) fn append_responses_agent_message_object(
    object: &serde_json::Map<String, Value>,
    messages: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    let mut message = object.clone();
    message.insert("role".to_string(), Value::String("assistant".to_string()));
    if !message.contains_key("content")
        && let Some(text) = message.get("message").and_then(Value::as_str)
    {
        message.insert("content".to_string(), Value::String(text.to_string()));
    }
    append_responses_message_object(&message, messages, tool_bridge)
}

pub(crate) fn append_responses_message_object(
    object: &serde_json::Map<String, Value>,
    messages: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    let role = object
        .get("role")
        .and_then(Value::as_str)
        .map(normalize_chat_role)
        .transpose()?
        .unwrap_or("user");
    if role == "tool" {
        return append_responses_tool_call_output_item(object, messages, "tool message");
    }
    let mut message = serde_json::Map::new();
    message.insert("role".to_string(), Value::String(role.to_string()));
    if role == "assistant" {
        if let Some(mut reasoning) = messages.pop_if(|last| {
            last["role"] == "assistant"
                && last.get("reasoning_content").is_some()
                && last["content"].is_null()
                && last.get("tool_calls").is_none()
                && last.get("function_call").is_none()
        }) {
            message.insert(
                "reasoning_content".to_string(),
                reasoning["reasoning_content"].take(),
            );
        }
        if let Some(reasoning) = object.get("reasoning_content").and_then(Value::as_str) {
            message.insert("reasoning_content".to_string(), json!(reasoning));
        }
    }
    let chat_content = object
        .get("content")
        .filter(|content| role != "assistant" || !content.is_null())
        .map(|content| responses_content_to_chat_content(content, role))
        .transpose()?
        .flatten();
    if let Some(chat_content) = chat_content {
        message.insert("content".to_string(), chat_content);
    } else if let Some(text) = first_text_field(object) {
        message.insert("content".to_string(), Value::String(text.to_string()));
    }
    if let Some(tool_calls) = object.get("tool_calls") {
        message.insert(
            "tool_calls".to_string(),
            normalize_chat_tool_calls(tool_calls, tool_bridge)?,
        );
        message.entry("content".to_string()).or_insert(Value::Null);
    }
    if let Some(function_call) = object.get("function_call") {
        message.insert(
            "function_call".to_string(),
            normalize_chat_legacy_function_call(function_call, tool_bridge)?,
        );
        message.entry("content".to_string()).or_insert(Value::Null);
    }
    if message.contains_key("content")
        || message.contains_key("tool_calls")
        || message.contains_key("function_call")
        || message.contains_key("reasoning_content")
    {
        message.entry("content".to_string()).or_insert(Value::Null);
        messages.push(Value::Object(message));
    }
    Ok(())
}

pub(crate) fn normalize_chat_role(role: &str) -> Result<&'static str> {
    match role.trim() {
        "user" => Ok("user"),
        "assistant" => Ok("assistant"),
        "system" | "developer" => Ok("system"),
        "tool" => Ok("tool"),
        other => anyhow::bail!("不支持的 Responses message role：{other}"),
    }
}

pub(crate) fn first_text_field(object: &serde_json::Map<String, Value>) -> Option<&str> {
    object
        .get("text")
        .or_else(|| object.get("input_text"))
        .or_else(|| object.get("output_text"))
        .or_else(|| object.get("message"))
        .and_then(Value::as_str)
}

pub(crate) fn first_visible_content_part_text(
    object: &serde_json::Map<String, Value>,
) -> Option<&str> {
    first_text_field(object).or_else(|| object.get("refusal").and_then(Value::as_str))
}

// Encrypted provider state cannot be represented by Chat Completions.
pub(crate) fn is_opaque_responses_input_item_type(item_type: &str) -> bool {
    matches!(item_type, "encrypted_content" | "compaction")
}

pub(crate) fn is_opaque_responses_content_part_type(part_type: &str) -> bool {
    matches!(part_type, "encrypted_content" | "reasoning" | "compaction")
}

pub(crate) fn responses_content_to_chat_content(
    content: &Value,
    role: &str,
) -> Result<Option<Value>> {
    match content {
        Value::String(text) => Ok((!text.is_empty()).then(|| Value::String(text.clone()))),
        Value::Array(parts) => {
            let mut chat_parts = Vec::new();
            for part in parts {
                if let Some(chat_part) = responses_content_part_to_chat_part(part)? {
                    chat_parts.push(chat_part);
                }
            }
            if chat_parts.is_empty() {
                return Ok(None);
            }
            let has_image = chat_parts
                .iter()
                .any(|part| part.get("type").and_then(Value::as_str) == Some("image_url"));
            if role != "user" {
                if has_image {
                    anyhow::bail!(
                        "Chat Completions 只支持把用户消息中的 Responses 图片内容无损转换"
                    );
                }
                let text = chat_parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Ok((!text.is_empty()).then_some(Value::String(text)));
            }
            Ok(Some(Value::Array(chat_parts)))
        }
        Value::Object(_) => responses_content_part_to_chat_part(content)
            .map(|part| part.map(|part| Value::Array(vec![part]))),
        _ => anyhow::bail!("message.content 必须是字符串、对象或数组"),
    }
}

pub(crate) fn responses_content_part_to_chat_part(part: &Value) -> Result<Option<Value>> {
    match part {
        Value::String(text) => Ok(Some(json!({"type":"text","text":text}))),
        Value::Object(object) => {
            let part_type = object.get("type").and_then(Value::as_str);
            match part_type {
                Some("input_text" | "output_text" | "text" | "refusal") => {
                    let text = first_visible_content_part_text(object)
                        .ok_or_else(|| anyhow::anyhow!("文本 content part 缺少 text"))?;
                    Ok(Some(json!({"type":"text","text":text})))
                }
                None if first_visible_content_part_text(object).is_some() => {
                    let text = first_visible_content_part_text(object).unwrap_or_default();
                    Ok(Some(json!({"type":"text","text":text})))
                }
                Some("input_image" | "image_url") => Ok(Some(json!({
                    "type": "image_url",
                    "image_url": responses_image_url_to_chat_image_url(object)?
                }))),
                None if object.contains_key("image_url") || object.contains_key("url") => {
                    Ok(Some(json!({
                        "type": "image_url",
                        "image_url": responses_image_url_to_chat_image_url(object)?
                    })))
                }
                Some(part_type) if is_opaque_responses_content_part_type(part_type) => Ok(None),
                None if object.contains_key("encrypted_content") => Ok(None),
                Some(part_type) => anyhow::bail!(
                    "Responses content part 类型 {part_type} 不能无损转换为 Chat Completions content"
                ),
                None => anyhow::bail!("Responses content part 缺少 text 或 image_url"),
            }
        }
        _ => anyhow::bail!("Responses content part 必须是字符串或对象"),
    }
}

pub(crate) fn responses_image_url_to_chat_image_url(
    object: &serde_json::Map<String, Value>,
) -> Result<Value> {
    if object.contains_key("file_id")
        && !object.contains_key("image_url")
        && !object.contains_key("url")
    {
        anyhow::bail!(
            "input_image.file_id 依赖 Responses 文件状态，不能无损转换为 Chat Completions"
        );
    }
    let mut image_url = match object.get("image_url").or_else(|| object.get("url")) {
        Some(Value::String(url)) if !url.is_empty() => json!({ "url": url }),
        Some(Value::Object(image_url)) => Value::Object(image_url.clone()),
        Some(_) => anyhow::bail!("input_image.image_url 必须是字符串或对象"),
        None => anyhow::bail!("input_image 缺少 image_url"),
    };
    if let Some(detail) = object.get("detail") {
        let detail = detail
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("input_image.detail 必须是字符串"))?;
        if !matches!(detail, "auto" | "low" | "high") {
            anyhow::bail!(
                "input_image.detail={detail} 不能无损转换为 Chat Completions image_url.detail"
            );
        }
        if let Some(image_url) = image_url.as_object_mut() {
            image_url
                .entry("detail".to_string())
                .or_insert_with(|| Value::String(detail.to_string()));
        }
    }
    Ok(image_url)
}

pub(crate) fn append_single_content_part_as_user_message(
    item: &Value,
    messages: &mut Vec<Value>,
) -> Result<()> {
    if let Some(chat_part) = responses_content_part_to_chat_part(item)? {
        messages.push(json!({"role":"user","content":[chat_part]}));
    }
    Ok(())
}

pub(crate) fn append_responses_function_call_item(
    object: &serde_json::Map<String, Value>,
    messages: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    let call_id = object
        .get("call_id")
        .or_else(|| object.get("id"))
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("function_call 缺少 call_id"))?;
    let tool_name = responses_tool_name_from_call_object(object, "function_call")?;
    let upstream_name = tool_bridge.upstream_name_for_call(&tool_name)?;
    let arguments =
        json_value_as_chat_string(object.get("arguments")).unwrap_or_else(|| "{}".to_string());
    append_chat_assistant_tool_call(messages, call_id, &upstream_name, &arguments)
}

pub(crate) fn append_responses_custom_tool_call_item(
    object: &serde_json::Map<String, Value>,
    messages: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    let call_id = object
        .get("call_id")
        .or_else(|| object.get("id"))
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("custom_tool_call 缺少 call_id"))?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("custom_tool_call 缺少 name"))?;
    let input = object
        .get("input")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("custom_tool_call.input 必须是字符串"))?;
    let namespace =
        responses_namespace_path(object.get("namespace"), "custom_tool_call.namespace")?;
    let tool_name = ResponsesToolName::custom_in_namespace(&namespace, name);
    let upstream_name = tool_bridge.upstream_name_for_call(&tool_name)?;
    let arguments = wrap_custom_tool_input(input)?;
    append_chat_assistant_tool_call(messages, call_id, &upstream_name, &arguments)
}

pub(crate) fn append_responses_tool_search_call_item(
    object: &serde_json::Map<String, Value>,
    messages: &mut Vec<Value>,
    tool_bridge: &ResponsesToolBridge,
) -> Result<()> {
    if object.get("execution").and_then(Value::as_str) != Some("client") {
        anyhow::bail!("tool_search_call 只允许 execution=client 的历史调用");
    }
    let call_id = object
        .get("call_id")
        .or_else(|| object.get("id"))
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("tool_search_call 缺少 call_id"))?;
    let upstream_name = tool_bridge.upstream_name_for_call(&ResponsesToolName::tool_search())?;
    let arguments = object
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        anyhow::bail!("tool_search_call.arguments 必须是 JSON 对象");
    }
    let arguments =
        serde_json::to_string(&arguments).context("序列化 tool_search_call.arguments 失败")?;
    append_chat_assistant_tool_call(messages, call_id, &upstream_name, &arguments)
}

pub(crate) fn append_responses_tool_search_output_item(
    object: &serde_json::Map<String, Value>,
    messages: &mut Vec<Value>,
) -> Result<()> {
    validate_client_tool_search_execution(object, "tool_search_output")?;
    let call_id = object
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("tool_search_output 缺少 call_id"))?;
    let tools = object
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("tool_search_output.tools 必须是数组"))?;
    let content = serde_json::to_string(&json!({"tools":tools}))
        .context("序列化 tool_search_output.tools 失败")?;
    messages.push(json!({
        "role":"tool",
        "tool_call_id":call_id,
        "content":content,
    }));
    Ok(())
}

pub(crate) fn validate_client_tool_search_execution(
    object: &serde_json::Map<String, Value>,
    context: &str,
) -> Result<()> {
    match object.get("execution").and_then(Value::as_str) {
        Some("client") => Ok(()),
        Some(execution) => anyhow::bail!("{context}.execution={execution} 不受支持；必须是 client"),
        None => anyhow::bail!("{context} 缺少 execution=client"),
    }
}

pub(crate) fn append_chat_assistant_tool_call(
    messages: &mut Vec<Value>,
    call_id: &str,
    upstream_name: &str,
    arguments: &str,
) -> Result<()> {
    let tool_call = json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": upstream_name,
            "arguments": arguments
        }
    });
    if let Some(last) = messages.last_mut().and_then(Value::as_object_mut)
        && last.get("role").and_then(Value::as_str) == Some("assistant")
        && last.get("content").is_none_or(|content| {
            content.is_null()
                || content.as_str() == Some("")
                || last.contains_key("reasoning_content")
        })
        && !last.contains_key("function_call")
    {
        last.entry("content".to_string()).or_insert(Value::Null);
        match last.get_mut("tool_calls") {
            Some(Value::Array(tool_calls)) => {
                tool_calls.push(tool_call);
                return Ok(());
            }
            Some(_) => anyhow::bail!("assistant message.tool_calls 必须是数组"),
            None => {
                last.insert("tool_calls".to_string(), Value::Array(vec![tool_call]));
                return Ok(());
            }
        }
    }
    messages.push(json!({
        "role": "assistant",
        "content": Value::Null,
        "tool_calls": [tool_call]
    }));
    Ok(())
}

pub(crate) fn append_responses_tool_call_output_item(
    object: &serde_json::Map<String, Value>,
    messages: &mut Vec<Value>,
    context: &str,
) -> Result<()> {
    let call_id = object
        .get("call_id")
        .or_else(|| object.get("tool_call_id"))
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{context} 缺少 call_id"))?;
    let output = object
        .get("output")
        .or_else(|| object.get("content"))
        .ok_or_else(|| anyhow::anyhow!("{context} 缺少 output"))?;
    let (content, images) = responses_tool_output_content(output)?.unwrap_or_else(|| {
        (
            json_value_as_chat_string(Some(output)).unwrap_or_default(),
            Vec::new(),
        )
    });
    messages.push(json!({
        "role": "tool",
        "tool_call_id": call_id,
        "content": content,
    }));
    if !images.is_empty() {
        messages.push(json!({
            "role": "user",
            "content": images,
        }));
    }
    Ok(())
}

pub(crate) fn responses_tool_output_content(
    output: &Value,
) -> Result<Option<(String, Vec<Value>)>> {
    let parts = match output {
        Value::Array(parts) => parts.as_slice(),
        Value::Object(_) => std::slice::from_ref(output),
        _ => return Ok(None),
    };
    // Arbitrary JSON remains a supported tool result. Once a known content
    // part is present, reject mixed/unknown entries instead of serializing the
    // original array and accidentally restoring filtered encrypted content.
    let typed = parts.iter().any(|part| {
        part.get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| {
                matches!(
                    kind,
                    "input_text" | "output_text" | "text" | "refusal" | "input_image" | "image_url"
                ) || is_opaque_responses_content_part_type(kind)
            })
    });
    if !typed {
        return Ok(None);
    }
    let mut text = Vec::new();
    let mut images = Vec::new();
    for part in parts {
        let Some(part) = part.as_object() else {
            anyhow::bail!("工具结构化内容不能混合未识别的条目");
        };
        match part.get("type").and_then(Value::as_str) {
            Some("compaction" | "compaction_trigger") => {
                anyhow::bail!("context_not_portable: 工具输出包含无法转换的 compaction 内容")
            }
            Some("input_text" | "output_text" | "text" | "refusal") => {
                text.push(
                    first_visible_content_part_text(part)
                        .ok_or_else(|| anyhow::anyhow!("工具文本输出缺少 text"))?
                        .to_string(),
                );
            }
            Some("input_image" | "image_url") => images.push(json!({
                "type": "image_url",
                "image_url": responses_image_url_to_chat_image_url(part)?,
            })),
            Some(part_type) if is_opaque_responses_content_part_type(part_type) => {}
            _ => anyhow::bail!("工具结构化内容包含无法转换的类型"),
        }
    }
    Ok(Some((text.join("\n"), images)))
}

pub(crate) fn normalize_chat_tool_calls(
    tool_calls: &Value,
    tool_bridge: &ResponsesToolBridge,
) -> Result<Value> {
    let tool_calls = tool_calls
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("tool_calls 必须是数组"))?;
    let mut normalized = Vec::new();
    for tool_call in tool_calls {
        let object = tool_call
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("tool_calls 条目必须是对象"))?;
        let call_type = object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("function");
        if call_type != "function" {
            anyhow::bail!("Chat tool_call 类型 {call_type} 不支持");
        }
        let function = object
            .get("function")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("tool_call 缺少 function"))?;
        let tool_name = responses_tool_name_from_function_object(
            function,
            object.get("namespace"),
            "tool_call.function",
        )?;
        let upstream_name = tool_bridge.upstream_name_for_call(&tool_name)?;
        let arguments = json_value_as_chat_string(function.get("arguments"))
            .unwrap_or_else(|| "{}".to_string());
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("call_codey_{}", Uuid::new_v4()));
        normalized.push(json!({
            "id": id,
            "type": "function",
            "function": {
                "name": upstream_name,
                "arguments": arguments,
            }
        }));
    }
    Ok(Value::Array(normalized))
}

pub(crate) fn normalize_chat_legacy_function_call(
    function_call: &Value,
    tool_bridge: &ResponsesToolBridge,
) -> Result<Value> {
    match function_call {
        Value::String(choice) if matches!(choice.as_str(), "auto" | "none") => {
            Ok(Value::String(choice.clone()))
        }
        Value::Object(object) => {
            let tool_name = responses_tool_name_from_call_object(object, "function_call")?;
            let upstream_name = tool_bridge.upstream_name_for_call(&tool_name)?;
            Ok(json!({"name": upstream_name}))
        }
        _ => anyhow::bail!("function_call 必须是 auto/none 或包含 name 的对象"),
    }
}

pub(crate) const NAMESPACE_UPSTREAM_TOOL_PREFIX: &str = "codey_ns__";
pub(crate) const CUSTOM_UPSTREAM_TOOL_PREFIX: &str = "codey_custom__";
pub(crate) const TOOL_SEARCH_UPSTREAM_TOOL_NAME: &str = "codey_tool_search__client__bridge_v1";
pub(crate) const UPSTREAM_FUNCTION_NAME_MAX_BYTES: usize = 64;
pub(crate) const MAX_RESPONSES_NAMESPACE_DEPTH: usize = 8;

#[derive(Default)]
pub(crate) struct ResponsesChatTools {
    pub(crate) tools: Option<Value>,
    pub(crate) web_search_options: Option<Value>,
    pub(crate) web_search_tool_seen: bool,
}

pub(crate) struct ResponsesChatToolsConversion<'a> {
    pub(crate) tool_bridge: &'a mut ResponsesToolBridge,
    pub(crate) upstream_names: HashMap<String, ResponsesToolName>,
    pub(crate) bridged_definitions: HashMap<ResponsesToolName, Value>,
    pub(crate) converted: Vec<Value>,
    pub(crate) web_search_options: Option<Value>,
    pub(crate) web_search_tool_seen: bool,
}

// Tool calls always carry object arguments, but some OpenAI-compatible
// providers reject a root union when any branch also permits a scalar or null.
pub(crate) fn normalize_responses_tool_parameter_roots(body: &mut Value) -> bool {
    let Some(body) = body.as_object_mut() else {
        return false;
    };
    let mut changed = normalize_responses_tool_list(body.get_mut("tools"));
    match body.get_mut("input") {
        Some(Value::Array(items)) => {
            for item in items {
                changed |= normalize_responses_input_tool_list(item);
            }
        }
        Some(item) => changed |= normalize_responses_input_tool_list(item),
        None => {}
    }
    changed
}

pub(crate) fn normalize_responses_input_tool_list(item: &mut Value) -> bool {
    let Some(item) = item.as_object_mut() else {
        return false;
    };
    if !matches!(
        item.get("type").and_then(Value::as_str),
        Some("additional_tools" | "tool_search_output")
    ) {
        return false;
    }
    normalize_responses_tool_list(item.get_mut("tools"))
}

pub(crate) fn normalize_responses_tool_list(tools: Option<&mut Value>) -> bool {
    let Some(tools) = tools.and_then(Value::as_array_mut) else {
        return false;
    };
    let mut changed = false;
    for tool in tools {
        let Some(tool) = tool.as_object_mut() else {
            continue;
        };
        if let Some(parameters) = tool.get_mut("parameters") {
            changed |= normalize_tool_parameter_root(parameters);
        }
        if let Some(parameters) = tool
            .get_mut("function")
            .and_then(Value::as_object_mut)
            .and_then(|function| function.get_mut("parameters"))
        {
            changed |= normalize_tool_parameter_root(parameters);
        }
        if let Some(input_schema) = tool.get_mut("input_schema") {
            changed |= normalize_tool_parameter_root(input_schema);
        }
        for field in ["tools", "children"] {
            changed |= normalize_responses_tool_list(tool.get_mut(field));
        }
    }
    changed
}

pub(crate) fn normalize_tool_parameter_root(schema: &mut Value) -> bool {
    match restrict_tool_parameter_schema_to_object(schema) {
        Some(changed) => changed,
        None => {
            *schema = json!({"type":"object","properties":{}});
            true
        }
    }
}

pub(crate) fn restrict_tool_parameter_schema_to_object(schema: &mut Value) -> Option<bool> {
    match schema {
        Value::Bool(true) => {
            *schema = json!({"type":"object"});
            Some(true)
        }
        Value::Object(schema) => {
            let mut changed = match schema.get("type") {
                Some(Value::String(schema_type)) if schema_type == "object" => false,
                Some(Value::Array(schema_types))
                    if schema_types
                        .iter()
                        .any(|schema_type| schema_type.as_str() == Some("object")) =>
                {
                    schema.insert("type".to_string(), Value::String("object".to_string()));
                    true
                }
                None => {
                    schema.insert("type".to_string(), Value::String("object".to_string()));
                    true
                }
                _ => return None,
            };
            for keyword in ["anyOf", "oneOf"] {
                let mut remove_keyword = false;
                if let Some(branches) = schema.get_mut(keyword).and_then(Value::as_array_mut) {
                    branches.retain_mut(|branch| {
                        let Some(branch_changed) = restrict_tool_parameter_schema_to_object(branch)
                        else {
                            changed = true;
                            return false;
                        };
                        changed |= branch_changed;
                        true
                    });
                    remove_keyword = branches.is_empty();
                }
                if remove_keyword {
                    schema.remove(keyword);
                    changed = true;
                }
            }
            Some(changed)
        }
        _ => None,
    }
}

pub(crate) fn responses_tools_to_chat_tools_with_bridge(
    tools: &Value,
    tool_bridge: &mut ResponsesToolBridge,
) -> Result<ResponsesChatTools> {
    let tools = tools
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("tools 必须是数组"))?;
    let mut conversion = ResponsesChatToolsConversion {
        tool_bridge,
        upstream_names: HashMap::new(),
        bridged_definitions: HashMap::new(),
        converted: Vec::new(),
        web_search_options: None,
        web_search_tool_seen: false,
    };
    for tool in tools {
        append_responses_tool_to_chat_tools(tool, &[], &mut conversion)?;
    }
    Ok(ResponsesChatTools {
        tools: (!conversion.converted.is_empty()).then_some(Value::Array(conversion.converted)),
        web_search_options: conversion.web_search_options,
        web_search_tool_seen: conversion.web_search_tool_seen,
    })
}

pub(crate) fn append_responses_tool_to_chat_tools(
    tool: &Value,
    namespace_path: &[String],
    conversion: &mut ResponsesChatToolsConversion<'_>,
) -> Result<()> {
    let object = tool
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("tools 条目必须是对象"))?;
    let tool_type = object.get("type").and_then(Value::as_str);
    if matches!(tool_type, Some("function"))
        || (tool_type.is_none() && object.contains_key("function"))
    {
        let mut function = responses_function_tool_map(object)?;
        let name = response_function_name(&function, "function tool")?.to_string();
        let tool_name = ResponsesToolName {
            kind: ResponsesToolKind::Function,
            namespace: namespace_path.to_vec(),
            name: name.clone(),
        };
        let should_push = if namespace_path.is_empty() {
            register_plain_tool_name(
                &name,
                conversion.tool_bridge,
                &mut conversion.upstream_names,
            )?;
            true
        } else {
            validate_namespaced_function_name(&name)?;
            let original_definition = Value::Object(function.clone());
            match register_namespaced_tool_name(
                tool_name,
                &original_definition,
                conversion.tool_bridge,
                &mut conversion.upstream_names,
                &mut conversion.bridged_definitions,
            )? {
                Some(upstream_name) => {
                    function.insert("name".to_string(), Value::String(upstream_name));
                    true
                }
                None => false,
            }
        };
        if should_push {
            if namespace_path.is_empty() {
                function.insert("name".to_string(), Value::String(name));
            }
            conversion
                .converted
                .push(json!({"type":"function","function":Value::Object(function)}));
        }
        return Ok(());
    }
    if tool_type == Some("namespace") {
        return append_responses_namespace_tools(object, namespace_path, conversion);
    }
    if tool_type == Some("custom") {
        let name = response_function_name(object, "custom tool")?.to_string();
        let tool_name = ResponsesToolName::custom_in_namespace(namespace_path, &name);
        let original_definition = Value::Object(object.clone());
        let Some(upstream_name) = register_custom_tool_name(
            tool_name,
            &original_definition,
            conversion.tool_bridge,
            &mut conversion.upstream_names,
            &mut conversion.bridged_definitions,
        )?
        else {
            return Ok(());
        };
        conversion.converted.push(json!({
            "type":"function",
            "function":{
                "name":upstream_name,
                "description":custom_tool_bridge_description(object)?,
                "parameters":{
                    "type":"object",
                    "properties":{
                        "input":{
                            "type":"string",
                            "description":"Raw free-form input for the original Responses custom tool."
                        }
                    },
                    "required":["input"],
                    "additionalProperties":false
                }
            }
        }));
        return Ok(());
    }
    if tool_type == Some("tool_search") {
        if !namespace_path.is_empty() {
            anyhow::bail!("namespace.tools 不支持工具类型 tool_search");
        }
        match object.get("execution").and_then(Value::as_str) {
            Some("client") => {}
            Some("server") | None => anyhow::bail!(
                "Responses 托管工具 tool_search 不能转换为 Chat/Anthropic function 工具；仅 execution=client 可桥接"
            ),
            Some(execution) => {
                anyhow::bail!("tool_search.execution={execution} 不受支持；仅 client 可桥接")
            }
        }
        let description = object
            .get("description")
            .and_then(Value::as_str)
            .filter(|description| !description.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("execution=client tool_search.description 必须是非空字符串")
            })?;
        let parameters = object
            .get("parameters")
            .filter(|parameters| parameters.is_object())
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("execution=client tool_search.parameters 必须是 JSON 对象")
            })?;
        let tool_name = ResponsesToolName::tool_search();
        if let Some(existing) = conversion
            .upstream_names
            .get(TOOL_SEARCH_UPSTREAM_TOOL_NAME)
            && existing != &tool_name
        {
            anyhow::bail!(
                "客户端 tool_search 保留函数名 {TOOL_SEARCH_UPSTREAM_TOOL_NAME} 与 function 工具冲突"
            );
        }
        let original_definition = Value::Object(object.clone());
        if let Some(existing_definition) = conversion.bridged_definitions.get(&tool_name) {
            if existing_definition == &original_definition {
                return Ok(());
            }
            anyhow::bail!("客户端 tool_search 存在定义冲突");
        }
        conversion
            .bridged_definitions
            .insert(tool_name.clone(), original_definition);
        conversion.upstream_names.insert(
            TOOL_SEARCH_UPSTREAM_TOOL_NAME.to_string(),
            tool_name.clone(),
        );
        conversion.tool_bridge.response_to_upstream.insert(
            tool_name.clone(),
            TOOL_SEARCH_UPSTREAM_TOOL_NAME.to_string(),
        );
        conversion
            .tool_bridge
            .upstream_to_response
            .insert(TOOL_SEARCH_UPSTREAM_TOOL_NAME.to_string(), tool_name);
        conversion.converted.push(json!({
            "type":"function",
            "function":{
                "name":TOOL_SEARCH_UPSTREAM_TOOL_NAME,
                "description":description,
                "parameters":parameters,
            }
        }));
        return Ok(());
    }
    if matches!(tool_type, Some("web_search" | "web_search_preview")) {
        conversion.web_search_tool_seen = true;
        if !namespace_path.is_empty() {
            let tool_name = tool_type.unwrap_or("unknown");
            anyhow::bail!("namespace.tools 不支持工具类型 {tool_name}");
        }
        let options = responses_web_search_tool_to_chat_options(object)?;
        if let Some(existing) = conversion.web_search_options.as_ref() {
            if existing == &options {
                return Ok(());
            }
            anyhow::bail!("Responses web_search 工具存在定义冲突");
        }
        conversion.web_search_options = Some(options);
        return Ok(());
    }
    if !namespace_path.is_empty() {
        let tool_name = tool_type.unwrap_or("unknown");
        anyhow::bail!("namespace.tools 不支持工具类型 {tool_name}");
    }
    let tool_name = tool_type.unwrap_or("unknown");
    anyhow::bail!(
        "Responses 内置工具 {tool_name} 不能转换为 Chat Completions tools，请改用支持 Responses 的线路"
    )
}

pub(crate) fn responses_web_search_tool_to_chat_options(
    object: &serde_json::Map<String, Value>,
) -> Result<Value> {
    if let Some(filters) = object.get("filters")
        && !filters.is_null()
    {
        anyhow::bail!("Chat Completions web_search_options 不支持 Responses web_search.filters");
    }
    if let Some(return_token_budget) = object.get("return_token_budget")
        && !return_token_budget.is_null()
    {
        anyhow::bail!(
            "Chat Completions web_search_options 不支持 Responses web_search.return_token_budget"
        );
    }
    if object.get("external_web_access").and_then(Value::as_bool) == Some(false) {
        anyhow::bail!("Chat Completions web_search_options 不能表达 external_web_access=false");
    }

    let mut options = serde_json::Map::new();
    if let Some(search_context_size) = object.get("search_context_size") {
        let size = search_context_size
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("web_search.search_context_size 必须是字符串"))?;
        if !matches!(size, "low" | "medium" | "high") {
            anyhow::bail!("web_search.search_context_size 必须是 low/medium/high");
        }
        options.insert(
            "search_context_size".to_string(),
            Value::String(size.to_string()),
        );
    }
    if let Some(user_location) = object.get("user_location")
        && !user_location.is_null()
    {
        options.insert(
            "user_location".to_string(),
            responses_web_search_user_location_to_chat(user_location)?,
        );
    }
    Ok(Value::Object(options))
}

pub(crate) fn responses_web_search_user_location_to_chat(user_location: &Value) -> Result<Value> {
    let object = user_location
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("web_search.user_location 必须是对象"))?;
    let location_type = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("web_search.user_location 缺少 type"))?;
    if location_type != "approximate" {
        anyhow::bail!("web_search.user_location.type 只能是 approximate");
    }
    let approximate = if let Some(approximate) = object.get("approximate") {
        approximate
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("web_search.user_location.approximate 必须是对象"))?
            .clone()
    } else {
        let mut approximate = serde_json::Map::new();
        for key in ["city", "country", "region", "timezone"] {
            if let Some(value) = object.get(key) {
                if !value.is_string() && !value.is_null() {
                    anyhow::bail!("web_search.user_location.{key} 必须是字符串");
                }
                if !value.is_null() {
                    approximate.insert(key.to_string(), value.clone());
                }
            }
        }
        approximate
    };
    Ok(json!({
        "type":"approximate",
        "approximate":Value::Object(approximate),
    }))
}

pub(crate) fn append_responses_namespace_tools(
    object: &serde_json::Map<String, Value>,
    parent_namespace: &[String],
    conversion: &mut ResponsesChatToolsConversion<'_>,
) -> Result<()> {
    let namespace = responses_namespace_name(object)?;
    let mut namespace_path = parent_namespace.to_vec();
    namespace_path.push(namespace);
    if namespace_path.len() > MAX_RESPONSES_NAMESPACE_DEPTH {
        anyhow::bail!("namespace 嵌套层级超过 {MAX_RESPONSES_NAMESPACE_DEPTH}");
    }
    let tools = object.get("tools");
    let children = object.get("children");
    if tools.is_none() && children.is_none() {
        anyhow::bail!("namespace 工具缺少 tools 或 children");
    }
    if let Some(tools) = tools {
        let tools = tools
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("namespace.tools 必须是数组"))?;
        for tool in tools {
            append_responses_tool_to_chat_tools(tool, &namespace_path, conversion)?;
        }
    }
    if let Some(children) = children {
        let children = children
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("namespace.children 必须是数组"))?;
        for child in children {
            append_responses_tool_to_chat_tools(child, &namespace_path, conversion)?;
        }
    }
    Ok(())
}

pub(crate) fn responses_function_tool_map(
    object: &serde_json::Map<String, Value>,
) -> Result<serde_json::Map<String, Value>> {
    let mut function = if let Some(function) = object.get("function").and_then(Value::as_object) {
        function.clone()
    } else {
        object
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "type" | "defer_loading"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<serde_json::Map<_, _>>()
    };
    function.remove("defer_loading");
    let name = response_function_name(&function, "function tool")?.to_string();
    function.insert("name".to_string(), Value::String(name));
    Ok(function)
}

pub(crate) fn register_plain_tool_name(
    name: &str,
    tool_bridge: &mut ResponsesToolBridge,
    upstream_names: &mut HashMap<String, ResponsesToolName>,
) -> Result<()> {
    let tool_name = ResponsesToolName::plain(name);
    if let Some(existing) = upstream_names.get(name) {
        if existing != &tool_name {
            anyhow::bail!("桥接工具展开名称 {name} 与 function 工具冲突");
        }
    } else {
        upstream_names.insert(name.to_string(), tool_name.clone());
    }
    tool_bridge
        .upstream_to_response
        .entry(name.to_string())
        .or_insert_with(|| tool_name.clone());
    tool_bridge
        .response_to_upstream
        .entry(tool_name)
        .or_insert_with(|| name.to_string());
    Ok(())
}

pub(crate) fn register_namespaced_tool_name(
    tool_name: ResponsesToolName,
    original_definition: &Value,
    tool_bridge: &mut ResponsesToolBridge,
    upstream_names: &mut HashMap<String, ResponsesToolName>,
    namespace_definitions: &mut HashMap<ResponsesToolName, Value>,
) -> Result<Option<String>> {
    if let Some(existing_definition) = namespace_definitions.get(&tool_name) {
        if existing_definition == original_definition {
            return Ok(None);
        }
        anyhow::bail!(
            "namespace 工具 {}.{} 存在定义冲突",
            tool_name.namespace.join("."),
            tool_name.name
        );
    }
    let upstream_name = namespaced_upstream_tool_name(&tool_name.namespace, &tool_name.name);
    if let Some(existing) = upstream_names.get(&upstream_name)
        && existing != &tool_name
    {
        anyhow::bail!("namespace 工具展开名称 {upstream_name} 发生冲突");
    }
    namespace_definitions.insert(tool_name.clone(), original_definition.clone());
    upstream_names.insert(upstream_name.clone(), tool_name.clone());
    tool_bridge.has_namespace_tools = true;
    tool_bridge
        .response_to_upstream
        .insert(tool_name.clone(), upstream_name.clone());
    tool_bridge
        .upstream_to_response
        .insert(upstream_name.clone(), tool_name);
    Ok(Some(upstream_name))
}

pub(crate) fn register_custom_tool_name(
    tool_name: ResponsesToolName,
    original_definition: &Value,
    tool_bridge: &mut ResponsesToolBridge,
    upstream_names: &mut HashMap<String, ResponsesToolName>,
    bridged_definitions: &mut HashMap<ResponsesToolName, Value>,
) -> Result<Option<String>> {
    if let Some(existing_definition) = bridged_definitions.get(&tool_name) {
        if existing_definition == original_definition {
            return Ok(None);
        }
        anyhow::bail!(
            "custom 工具 {}{} 存在定义冲突",
            tool_name
                .namespace_string()
                .map(|namespace| format!("{namespace}."))
                .unwrap_or_default(),
            tool_name.name,
        );
    }
    let upstream_name = custom_upstream_tool_name(&tool_name.namespace, &tool_name.name);
    if let Some(existing) = upstream_names.get(&upstream_name)
        && existing != &tool_name
    {
        anyhow::bail!("custom 工具展开名称 {upstream_name} 发生冲突");
    }
    bridged_definitions.insert(tool_name.clone(), original_definition.clone());
    upstream_names.insert(upstream_name.clone(), tool_name.clone());
    tool_bridge.has_custom_tools = true;
    tool_bridge
        .response_to_upstream
        .insert(tool_name.clone(), upstream_name.clone());
    tool_bridge
        .upstream_to_response
        .insert(upstream_name.clone(), tool_name);
    Ok(Some(upstream_name))
}

pub(crate) fn custom_tool_bridge_description(
    object: &serde_json::Map<String, Value>,
) -> Result<String> {
    let mut description = object
        .get("description")
        .and_then(Value::as_str)
        .filter(|description| !description.is_empty())
        .map(|description| {
            format!(
                "{}\n\n",
                utf8_prefix(description, MAX_CUSTOM_TOOL_SOURCE_DESCRIPTION_BYTES)
            )
        })
        .unwrap_or_default();
    description.push_str(
        "[Codey compatibility bridge] This was an OpenAI Responses custom free-form tool. \
Call this function with exactly one `input` string containing the complete raw tool input. \
Do not JSON-encode the string again and do not add wrapper text.",
    );
    if let Some(format) = object.get("format") {
        description.push_str(" Format: ");
        description.push_str(
            &serde_json::to_string(format).context("序列化 Responses custom 工具格式提示失败")?,
        );
    }
    if description.len() > MAX_CUSTOM_TOOL_BRIDGE_DESCRIPTION_BYTES {
        let mut end = MAX_CUSTOM_TOOL_BRIDGE_DESCRIPTION_BYTES - '…'.len_utf8();
        while !description.is_char_boundary(end) {
            end -= 1;
        }
        description.truncate(end);
        description.push('…');
    }
    Ok(description)
}

pub(crate) fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub(crate) fn response_function_name<'a>(
    object: &'a serde_json::Map<String, Value>,
    context: &str,
) -> Result<&'a str> {
    object
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{context} 缺少 name"))
}

pub(crate) fn responses_namespace_name(object: &serde_json::Map<String, Value>) -> Result<String> {
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("namespace 工具缺少 name"))?;
    validate_namespace_segment(name, "namespace.name")
}

pub(crate) fn validate_namespace_segment(segment: &str, field: &str) -> Result<String> {
    if segment.is_empty()
        || segment.trim() != segment
        || segment.contains('.')
        || segment.chars().any(char::is_control)
    {
        anyhow::bail!("{field} 必须是非空、无控制字符且不包含点号的字符串");
    }
    Ok(segment.to_string())
}

pub(crate) fn validate_namespaced_function_name(name: &str) -> Result<()> {
    if name.is_empty() || name.trim() != name || name.chars().any(char::is_control) {
        anyhow::bail!("namespace function tool.name 必须是非空且无控制字符的字符串");
    }
    Ok(())
}

pub(crate) fn namespaced_upstream_tool_name(namespace: &[String], name: &str) -> String {
    let canonical = format!("{}\u{1e}{name}", namespace.join("\u{1f}"));
    let hash = stable_tool_hash_hex(&canonical);
    let stem_source = format!("{}__{name}", namespace.join("__"));
    let stem = sanitize_upstream_tool_stem(&stem_source);
    let suffix = format!("__{hash}");
    let max_stem_len = UPSTREAM_FUNCTION_NAME_MAX_BYTES
        .saturating_sub(NAMESPACE_UPSTREAM_TOOL_PREFIX.len())
        .saturating_sub(suffix.len());
    let stem = stem.chars().take(max_stem_len).collect::<String>();
    format!("{NAMESPACE_UPSTREAM_TOOL_PREFIX}{stem}{suffix}")
}

pub(crate) fn custom_upstream_tool_name(namespace: &[String], name: &str) -> String {
    let canonical = format!("custom\u{1e}{}\u{1e}{name}", namespace.join("\u{1f}"));
    let hash = stable_tool_hash_hex(&canonical);
    let stem_source = if namespace.is_empty() {
        name.to_string()
    } else {
        format!("{}__{name}", namespace.join("__"))
    };
    let stem = sanitize_upstream_tool_stem(&stem_source);
    let suffix = format!("__{hash}");
    let max_stem_len = UPSTREAM_FUNCTION_NAME_MAX_BYTES
        .saturating_sub(CUSTOM_UPSTREAM_TOOL_PREFIX.len())
        .saturating_sub(suffix.len());
    let stem = stem.chars().take(max_stem_len).collect::<String>();
    format!("{CUSTOM_UPSTREAM_TOOL_PREFIX}{stem}{suffix}")
}

pub(crate) fn stable_tool_hash_hex(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub(crate) fn sanitize_upstream_tool_stem(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    let mut last_was_separator = false;
    for ch in value.chars() {
        let next = if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
            ch
        } else {
            '_'
        };
        if next == '_' && last_was_separator {
            continue;
        }
        last_was_separator = next == '_';
        sanitized.push(next);
    }
    let sanitized = sanitized.trim_matches('_');
    if sanitized.is_empty() {
        "tool".to_string()
    } else {
        sanitized.to_string()
    }
}

pub(crate) fn looks_like_namespace_upstream_name(name: &str) -> bool {
    name.starts_with(NAMESPACE_UPSTREAM_TOOL_PREFIX)
}

pub(crate) fn looks_like_custom_upstream_name(name: &str) -> bool {
    name.starts_with(CUSTOM_UPSTREAM_TOOL_PREFIX)
}

pub(crate) fn could_be_namespace_upstream_name(name: &str) -> bool {
    !name.is_empty()
        && (NAMESPACE_UPSTREAM_TOOL_PREFIX.starts_with(name)
            || name.starts_with(NAMESPACE_UPSTREAM_TOOL_PREFIX))
}

pub(crate) fn could_be_custom_upstream_name(name: &str) -> bool {
    !name.is_empty()
        && (CUSTOM_UPSTREAM_TOOL_PREFIX.starts_with(name)
            || name.starts_with(CUSTOM_UPSTREAM_TOOL_PREFIX))
}

pub(crate) fn responses_namespace_path(value: Option<&Value>, field: &str) -> Result<Vec<String>> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(namespace)) => namespace
            .split('.')
            .map(|segment| validate_namespace_segment(segment, field))
            .collect(),
        Some(Value::Array(segments)) => segments
            .iter()
            .map(|segment| {
                segment
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("{field} 数组只能包含字符串"))
                    .and_then(|segment| validate_namespace_segment(segment, field))
            })
            .collect(),
        Some(_) => anyhow::bail!("{field} 必须是字符串或字符串数组"),
    }
}

pub(crate) fn merge_namespace_paths(
    outer: Vec<String>,
    inner: Vec<String>,
    field: &str,
) -> Result<Vec<String>> {
    if outer.is_empty() {
        return Ok(inner);
    }
    if inner.is_empty() || inner == outer {
        return Ok(outer);
    }
    anyhow::bail!("{field} 同时包含冲突的 namespace")
}

pub(crate) fn responses_tool_name_from_function_object(
    function: &serde_json::Map<String, Value>,
    outer_namespace: Option<&Value>,
    context: &str,
) -> Result<ResponsesToolName> {
    let name = response_function_name(function, context)?;
    let namespace = merge_namespace_paths(
        responses_namespace_path(outer_namespace, "namespace")?,
        responses_namespace_path(function.get("namespace"), "function.namespace")?,
        context,
    )?;
    Ok(ResponsesToolName {
        kind: ResponsesToolKind::Function,
        namespace,
        name: name.to_string(),
    })
}

pub(crate) fn responses_tool_name_from_call_object(
    object: &serde_json::Map<String, Value>,
    context: &str,
) -> Result<ResponsesToolName> {
    if let Some(function) = object.get("function").and_then(Value::as_object) {
        return responses_tool_name_from_function_object(
            function,
            object.get("namespace"),
            context,
        );
    }
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{context} 缺少 name"))?;
    Ok(ResponsesToolName {
        kind: ResponsesToolKind::Function,
        namespace: responses_namespace_path(object.get("namespace"), "namespace")?,
        name: name.to_string(),
    })
}

pub(crate) fn responses_tool_choice_to_chat_tool_choice(
    tool_choice: &Value,
    tool_bridge: &ResponsesToolBridge,
) -> Result<Value> {
    match tool_choice {
        Value::String(choice) if matches!(choice.as_str(), "auto" | "none" | "required") => {
            Ok(Value::String(choice.clone()))
        }
        Value::Object(object) => {
            let choice_type = object.get("type").and_then(Value::as_str);
            let tool_name = match choice_type {
                Some("function") => {
                    responses_tool_name_from_call_object(object, "function tool_choice")?
                }
                Some("custom") => {
                    let name = response_function_name(object, "custom tool_choice")?;
                    let namespace = responses_namespace_path(
                        object.get("namespace"),
                        "custom tool_choice.namespace",
                    )?;
                    ResponsesToolName::custom_in_namespace(&namespace, name)
                }
                Some("tool_search") => ResponsesToolName::tool_search(),
                _ => {
                    let tool_name = choice_type.unwrap_or("unknown");
                    anyhow::bail!(
                        "Responses tool_choice 类型 {tool_name} 不能转换为 Chat Completions tool_choice"
                    );
                }
            };
            let upstream_name = tool_bridge.upstream_name_for_call(&tool_name)?;
            Ok(json!({"type":"function","function":{"name":upstream_name}}))
        }
        _ => anyhow::bail!("tool_choice 必须是 auto/none/required 或 function 对象"),
    }
}

pub(crate) fn responses_function_call_choice_to_chat(
    function_call: &Value,
    tool_bridge: &ResponsesToolBridge,
) -> Result<Value> {
    match function_call {
        Value::String(choice) if matches!(choice.as_str(), "auto" | "none") => {
            Ok(Value::String(choice.clone()))
        }
        Value::Object(object) => {
            let tool_name = responses_tool_name_from_call_object(object, "function_call")?;
            let upstream_name = tool_bridge.upstream_name_for_call(&tool_name)?;
            Ok(json!({"name":upstream_name}))
        }
        _ => anyhow::bail!("function_call 必须是 auto/none 或 function name 对象"),
    }
}

pub(crate) fn json_value_as_chat_string(value: Option<&Value>) -> Option<String> {
    value.map(|value| match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    })
}

pub(crate) fn wrap_custom_tool_input(input: &str) -> Result<String> {
    serde_json::to_string(&json!({"input":input})).context("序列化 custom 工具 input 包装失败")
}

pub(crate) fn custom_tool_input_from_value(value: &Value, context: &str) -> Result<String> {
    value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{context} 必须是 JSON 对象"))?
        .get("input")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("{context}.input 必须是字符串"))
}

pub(crate) fn custom_tool_input_from_arguments(arguments: &str, context: &str) -> Result<String> {
    let value = serde_json::from_str::<Value>(arguments)
        .with_context(|| format!("{context} 不是有效 JSON"))?;
    custom_tool_input_from_value(&value, context)
}

pub(crate) fn responses_tool_call_item_from_upstream_arguments(
    tool_name: &ResponsesToolName,
    call_id: String,
    arguments: String,
    status: &str,
    context: &str,
) -> Result<Value> {
    let payload = if tool_name.is_custom() {
        Value::String(custom_tool_input_from_arguments(&arguments, context)?)
    } else if tool_name.is_tool_search() {
        let arguments = serde_json::from_str::<Value>(&arguments)
            .with_context(|| format!("{context} 不是有效 JSON"))?;
        if !arguments.is_object() {
            anyhow::bail!("{context} 必须是 JSON 对象");
        }
        arguments
    } else {
        Value::String(arguments)
    };
    Ok(responses_tool_call_item_from_payload(
        tool_name, call_id, payload, status,
    ))
}

pub(crate) fn responses_tool_call_item_from_payload(
    tool_name: &ResponsesToolName,
    call_id: String,
    payload: Value,
    status: &str,
) -> Value {
    let item_id = responses_tool_call_item_id(tool_name);
    responses_tool_call_item_with_id(tool_name, item_id, call_id, payload, status)
}

pub(crate) fn responses_tool_call_item_id(tool_name: &ResponsesToolName) -> String {
    let prefix = if tool_name.is_custom() {
        "ctc_codey_"
    } else if tool_name.is_tool_search() {
        "tsc_codey_"
    } else {
        "fc_codey_"
    };
    format!("{prefix}{}", Uuid::new_v4())
}

pub(crate) fn responses_tool_call_item_with_id(
    tool_name: &ResponsesToolName,
    item_id: String,
    call_id: String,
    payload: Value,
    status: &str,
) -> Value {
    let (item_type, payload_field) = match tool_name.kind {
        ResponsesToolKind::Function => ("function_call", "arguments"),
        ResponsesToolKind::Custom => ("custom_tool_call", "input"),
        ResponsesToolKind::ToolSearch => ("tool_search_call", "arguments"),
    };
    let mut item = serde_json::Map::from_iter([
        ("id".to_string(), Value::String(item_id)),
        ("type".to_string(), Value::String(item_type.to_string())),
        ("status".to_string(), Value::String(status.to_string())),
        ("call_id".to_string(), Value::String(call_id)),
        (payload_field.to_string(), payload),
    ]);
    tool_name.insert_response_fields(&mut item);
    Value::Object(item)
}

pub(crate) fn push_chat_text_message(
    messages: &mut Vec<Value>,
    role: &str,
    content: &str,
) -> Result<()> {
    if content.is_empty() {
        return Ok(());
    }
    messages.push(json!({"role":role,"content":content}));
    Ok(())
}

pub(crate) fn copy_number_or_string_field(
    from: &serde_json::Map<String, Value>,
    to: &mut serde_json::Map<String, Value>,
    source: &str,
    target: &str,
) {
    if let Some(value) = from.get(source)
        && (value.is_number() || value.is_string() || value.is_boolean())
    {
        to.insert(target.to_string(), value.clone());
    }
}

pub(crate) fn copy_json_field(
    from: &serde_json::Map<String, Value>,
    to: &mut serde_json::Map<String, Value>,
    source: &str,
    target: &str,
) {
    if let Some(value) = from.get(source) {
        to.insert(target.to_string(), value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_final_upstream_tool_schema_shapes() {
        let mut body = json!({
            "tools": [{
                "name": "automation_update",
                "input_schema": {
                    "anyOf": [
                        {"type": "object", "properties": {"mode": {"type": "string"}}},
                        {"type": "null"}
                    ]
                }
            }]
        });

        assert!(normalize_responses_tool_parameter_roots(&mut body));
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(
            body["tools"][0]["input_schema"]["anyOf"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
