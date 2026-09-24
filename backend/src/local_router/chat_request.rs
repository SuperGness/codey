use super::*;

#[cfg(test)]
pub(crate) fn responses_to_chat_completions_body(body: &Value) -> Result<Value> {
    Ok(responses_to_chat_completions_request(body)?.body)
}

pub(crate) fn responses_to_chat_completions_request(
    body: &Value,
) -> Result<ConvertedResponsesRequest> {
    validate_portable_context(body)?;
    validate_adapted_agent_payloads(body)?;
    let object = body
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Responses 请求体必须是 JSON 对象"))?;
    let model = object
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| anyhow::anyhow!("缺少 model 字段"))?;
    let mut messages = Vec::new();
    if let Some(instructions) = object.get("instructions").and_then(Value::as_str)
        && !instructions.is_empty()
    {
        messages.push(json!({"role":"system","content":instructions}));
    }
    // Responses can add tools at a specific point in `input`. Chat and
    // Anthropic only accept request-level tool declarations, so retain the
    // message order while promoting those declarations for the current model
    // generation. Historical assistant items are not re-executed.
    let (normalized_input, additional_tools) =
        responses_input_without_additional_tools(object.get("input"))?;
    let merged_tools = merge_responses_tools(object.get("tools"), additional_tools)?;
    let mut tool_bridge = ResponsesToolBridge::default();
    let chat_tools = merged_tools
        .as_ref()
        .map(|tools| responses_tools_to_chat_tools_with_bridge(tools, &mut tool_bridge))
        .transpose()?;
    let chat_reasoning_summaries = append_chat_messages_from_responses_input(
        normalized_input.as_ref(),
        &mut messages,
        &mut tool_bridge,
    )?;
    if messages.is_empty() {
        anyhow::bail!("缺少可转换为 Chat Completions messages 的 input");
    }
    if object
        .get("previous_response_id")
        .is_some_and(|value| !value.is_null())
    {
        anyhow::bail!(
            "previous_response_id 依赖 Responses 服务端状态，不能无损转换为 Chat Completions"
        );
    }
    let stream = object
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut chat = serde_json::Map::from_iter([
        ("model".to_string(), Value::String(model.to_string())),
        ("messages".to_string(), Value::Array(messages)),
        ("stream".to_string(), Value::Bool(stream)),
    ]);
    copy_number_or_string_field(object, &mut chat, "temperature", "temperature");
    copy_number_or_string_field(object, &mut chat, "top_p", "top_p");
    copy_number_or_string_field(object, &mut chat, "presence_penalty", "presence_penalty");
    copy_number_or_string_field(object, &mut chat, "frequency_penalty", "frequency_penalty");
    copy_number_or_string_field(object, &mut chat, "reasoning_effort", "reasoning_effort");
    if !chat.contains_key("reasoning_effort")
        && let Some(reasoning) = object.get("reasoning").and_then(Value::as_object)
    {
        copy_number_or_string_field(reasoning, &mut chat, "effort", "reasoning_effort");
    }
    copy_number_or_string_field(object, &mut chat, "user", "user");
    copy_json_field(object, &mut chat, "stop", "stop");
    copy_json_field(object, &mut chat, "seed", "seed");
    copy_json_field(object, &mut chat, "logit_bias", "logit_bias");
    copy_json_field(object, &mut chat, "logprobs", "logprobs");
    copy_json_field(object, &mut chat, "top_logprobs", "top_logprobs");
    if stream {
        let mut stream_options = object
            .get("stream_options")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        // Chat Completions only reports usage for streams when explicitly
        // requested. The final Responses event needs that usage shape.
        stream_options.insert("include_usage".to_string(), Value::Bool(true));
        chat.insert("stream_options".to_string(), Value::Object(stream_options));
    }
    if let Some(text) = object.get("text").and_then(Value::as_object)
        && let Some(format) = text.get("format")
    {
        chat.insert(
            "response_format".to_string(),
            responses_text_format_to_chat_response_format(format)?,
        );
    }
    if let Some(max_tokens) = object
        .get("max_output_tokens")
        .or_else(|| object.get("max_tokens"))
        .cloned()
    {
        chat.insert("max_tokens".to_string(), max_tokens);
    }
    let web_search_tool_seen = chat_tools
        .as_ref()
        .is_some_and(|tools| tools.web_search_tool_seen);
    let has_chat_tools = chat_tools
        .as_ref()
        .and_then(|tools| tools.tools.as_ref())
        .is_some();
    if let Some(web_search_options) = chat_web_search_options_for_tool_choice(
        chat_tools
            .as_ref()
            .and_then(|tools| tools.web_search_options.as_ref()),
        object.get("tool_choice"),
        has_chat_tools,
    )? {
        chat.insert("web_search_options".to_string(), web_search_options);
    }
    let web_search_enabled = chat.contains_key("web_search_options");
    if web_search_enabled {
        reject_unportable_chat_web_search_include(object.get("include"))?;
    }
    if let Some(tools) = chat_tools.and_then(|tools| tools.tools) {
        chat.insert("tools".to_string(), tools);
    }
    if let Some(tool_choice) = object.get("tool_choice")
        && !should_omit_chat_tool_choice_for_web_search(
            tool_choice,
            has_chat_tools,
            web_search_tool_seen,
            web_search_enabled,
        )?
    {
        chat.insert(
            "tool_choice".to_string(),
            responses_tool_choice_to_chat_tool_choice(tool_choice, &mut tool_bridge)?,
        );
    }
    if let Some(parallel_tool_calls) = object.get("parallel_tool_calls") {
        if !parallel_tool_calls.is_boolean() {
            anyhow::bail!("parallel_tool_calls 必须是布尔值");
        }
        chat.insert(
            "parallel_tool_calls".to_string(),
            parallel_tool_calls.clone(),
        );
    }
    if let Some(function_call) = object.get("function_call") {
        chat.insert(
            "function_call".to_string(),
            responses_function_call_choice_to_chat(function_call, &mut tool_bridge)?,
        );
    }
    Ok(ConvertedResponsesRequest {
        body: Value::Object(chat),
        tool_bridge,
        chat_reasoning_summaries,
    })
}

pub(crate) fn responses_input_without_additional_tools(
    input: Option<&Value>,
) -> Result<(Option<Value>, Vec<Value>)> {
    let Some(input) = input else {
        return Ok((None, Vec::new()));
    };
    let mut additional_tools = Vec::new();
    match input {
        Value::Array(items) => {
            let mut normalized = Vec::with_capacity(items.len());
            for item in items {
                if append_responses_additional_tools(item, &mut additional_tools)? {
                    continue;
                }
                append_responses_tool_search_output_tools(item, &mut additional_tools)?;
                if item
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(is_opaque_responses_input_item_type)
                {
                    continue;
                }
                normalized.push(item.clone());
            }
            Ok((Some(Value::Array(normalized)), additional_tools))
        }
        Value::Object(_) if append_responses_additional_tools(input, &mut additional_tools)? => {
            Ok((None, additional_tools))
        }
        Value::Object(_)
            if input
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(is_opaque_responses_input_item_type) =>
        {
            Ok((None, additional_tools))
        }
        Value::Object(_) => {
            append_responses_tool_search_output_tools(input, &mut additional_tools)?;
            Ok((Some(input.clone()), additional_tools))
        }
        _ => Ok((Some(input.clone()), additional_tools)),
    }
}

pub(crate) fn append_responses_tool_search_output_tools(
    item: &Value,
    tools: &mut Vec<Value>,
) -> Result<()> {
    let Some(object) = item.as_object() else {
        return Ok(());
    };
    if object.get("type").and_then(Value::as_str) != Some("tool_search_output") {
        return Ok(());
    }
    validate_client_tool_search_execution(object, "tool_search_output")?;
    let loaded = object
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("tool_search_output.tools 必须是数组"))?;
    tools.extend(loaded.iter().cloned());
    Ok(())
}

pub(crate) fn append_responses_additional_tools(
    item: &Value,
    tools: &mut Vec<Value>,
) -> Result<bool> {
    let Some(object) = item.as_object() else {
        return Ok(false);
    };
    if object.get("type").and_then(Value::as_str) != Some("additional_tools") {
        return Ok(false);
    }
    if object.get("role").and_then(Value::as_str) != Some("developer") {
        anyhow::bail!("additional_tools.role 必须是 developer");
    }
    let additional = object
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("additional_tools.tools 必须是数组"))?;
    tools.extend(additional.iter().cloned());
    Ok(true)
}

pub(crate) fn merge_responses_tools(
    configured: Option<&Value>,
    additional: Vec<Value>,
) -> Result<Option<Value>> {
    if configured.is_none() && additional.is_empty() {
        return Ok(None);
    }
    let configured = match configured {
        Some(Value::Array(tools)) => tools.as_slice(),
        Some(_) => anyhow::bail!("tools 必须是数组"),
        None => &[],
    };
    let mut merged = Vec::with_capacity(configured.len() + additional.len());
    let mut identities = HashMap::with_capacity(configured.len() + additional.len());
    for tool in configured {
        if let Some(identity) = responses_tool_identity(tool) {
            if identity.starts_with("function/")
                && identities
                    .get(&identity)
                    .and_then(|index| merged.get(*index))
                    == Some(tool)
            {
                continue;
            }
            identities.entry(identity).or_insert(merged.len());
        }
        merged.push(tool.clone());
    }
    for tool in additional {
        if let Some(identity) = responses_tool_identity(&tool) {
            if let Some(index) = identities.get(&identity).copied() {
                if merged[index] == tool {
                    continue;
                }
                anyhow::bail!("additional_tools 包含定义冲突的工具 {identity}");
            }
            identities.insert(identity, merged.len());
        }
        merged.push(tool);
    }
    Ok(Some(Value::Array(merged)))
}

pub(crate) fn responses_tool_identity(tool: &Value) -> Option<String> {
    let object = tool.as_object()?;
    let tool_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("function");
    if matches!(tool_type, "web_search" | "web_search_preview") {
        return Some("web_search".to_string());
    }
    if tool_type == "tool_search" {
        let execution = object
            .get("execution")
            .and_then(Value::as_str)
            .unwrap_or("server");
        return Some(format!("tool_search/{execution}"));
    }
    let name = object
        .get("name")
        .or_else(|| {
            object
                .get("function")
                .and_then(|function| function.get("name"))
        })
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())?;
    Some(format!("{tool_type}/{name}"))
}

pub(crate) fn chat_web_search_options_for_tool_choice(
    options: Option<&Value>,
    tool_choice: Option<&Value>,
    has_chat_tools: bool,
) -> Result<Option<Value>> {
    let Some(tool_choice) = tool_choice else {
        if options.is_some() {
            anyhow::bail!(
                "Responses 可选 web_search 不能无损转换为 Chat Completions；请明确选择 web_search、required 或 none"
            );
        }
        return Ok(None);
    };
    match tool_choice {
        Value::String(choice) if choice == "auto" => {
            // Codex app-server currently attaches an ambient optional
            // `web_search` tool even when the selected model descriptor does
            // not advertise search. Adapted Chat/Anthropic routes cannot
            // execute that hosted Responses tool, but `auto` also proves the
            // caller did not require it. Remove only this ambient declaration;
            // explicit and required search choices remain fail-closed or use
            // the dedicated Chat search mapping below.
            Ok(None)
        }
        Value::String(choice) if choice == "none" => Ok(None),
        Value::String(choice) if choice == "required" => {
            let Some(options) = options else {
                return Ok(None);
            };
            if has_chat_tools {
                anyhow::bail!(
                    "Responses tool_choice=required 同时包含 web_search 和其他工具，Chat Completions 无法无损表达"
                );
            }
            Ok(Some(options.clone()))
        }
        Value::Object(object) => match object.get("type").and_then(Value::as_str) {
            Some("web_search" | "web_search_preview") => {
                let options = options.ok_or_else(|| {
                    anyhow::anyhow!("Responses tool_choice 选择了未在 tools 中声明的 web_search")
                })?;
                if has_chat_tools {
                    anyhow::bail!(
                        "Responses tool_choice 明确选择 web_search 时仍包含其他工具，Chat Completions 无法无损表达"
                    );
                }
                Ok(Some(options.clone()))
            }
            Some("function" | "custom" | "tool_search") => Ok(None),
            Some(tool_type) => anyhow::bail!(
                "Responses tool_choice 类型 {tool_type} 不能转换为 Chat Completions tool_choice"
            ),
            None => anyhow::bail!("Responses tool_choice 缺少 type"),
        },
        _ => anyhow::bail!("tool_choice 必须是 auto/none/required 或对象"),
    }
}

pub(crate) fn reject_unportable_chat_web_search_include(include: Option<&Value>) -> Result<()> {
    let Some(include) = include else {
        return Ok(());
    };
    let entries = include
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Responses include 必须是数组"))?;
    if entries
        .iter()
        .any(|entry| entry.as_str() == Some("web_search_call.action.sources"))
    {
        anyhow::bail!(
            "Responses include=web_search_call.action.sources 不能无损转换为 Chat Completions"
        );
    }
    Ok(())
}

pub(crate) fn responses_tool_choice_targets_web_search(tool_choice: &Value) -> Result<bool> {
    match tool_choice {
        Value::Object(object) => match object.get("type").and_then(Value::as_str) {
            Some("web_search" | "web_search_preview") => Ok(true),
            Some("function" | "custom" | "tool_search") => Ok(false),
            Some(tool_type) => anyhow::bail!(
                "Responses tool_choice 类型 {tool_type} 不能转换为 Chat Completions tool_choice"
            ),
            None => anyhow::bail!("Responses tool_choice 缺少 type"),
        },
        _ => Ok(false),
    }
}

pub(crate) fn should_omit_chat_tool_choice_for_web_search(
    tool_choice: &Value,
    has_chat_tools: bool,
    web_search_tool_seen: bool,
    web_search_enabled: bool,
) -> Result<bool> {
    if !web_search_tool_seen {
        return responses_tool_choice_targets_web_search(tool_choice);
    }
    if !has_chat_tools {
        return match tool_choice {
            Value::String(choice) if matches!(choice.as_str(), "auto" | "none" | "required") => {
                Ok(true)
            }
            Value::Object(object)
                if matches!(
                    object.get("type").and_then(Value::as_str),
                    Some("web_search" | "web_search_preview")
                ) =>
            {
                Ok(true)
            }
            _ => responses_tool_choice_targets_web_search(tool_choice),
        };
    }
    if web_search_enabled {
        responses_tool_choice_targets_web_search(tool_choice)
    } else {
        Ok(false)
    }
}

pub(crate) const DEFAULT_ANTHROPIC_MAX_TOKENS: u64 = 8192;

/// 客户端省略输出上限、且推理强度高于 medium 时写入的上限。
/// 第三方会把 high 及以上译成不小于 8192 的思考预算，Anthropic 要求预算严格小于
/// 输出上限。64000 能盖住已验证的 high / ultra，也没有超过 Claude 4.5 一代的输出能力。
const CLAUDE_HIGH_EFFORT_OUTPUT_TOKENS: u64 = 64_000;

/// 客户端没有声明输出上限时，为会触发思考预算约束的请求补上上限。
/// 已有 `max_output_tokens` 或 `max_tokens` 时保持原值。非 Claude 的 Chat / Responses
/// 请求不补，避免把 GPT、DeepSeek 的输出上限改掉。
pub(crate) fn ensure_omitted_reasoning_output_limit(
    body: &mut Value,
    bridge: ProtocolBridge,
) -> bool {
    let Some(object) = body.as_object_mut() else {
        return false;
    };
    if object.contains_key("max_output_tokens") || object.contains_key("max_tokens") {
        return false;
    }
    let effort = object
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .or_else(|| {
            object
                .get("reasoning")
                .and_then(Value::as_object)
                .and_then(|reasoning| reasoning.get("effort"))
                .and_then(Value::as_str)
        })
        .unwrap_or("");
    if !reasoning_effort_needs_output_headroom(effort) {
        return false;
    }
    let model = object.get("model").and_then(Value::as_str).unwrap_or("");
    let Some(limit) = omitted_reasoning_output_limit(model, bridge) else {
        return false;
    };
    object.insert("max_output_tokens".to_string(), Value::Number(limit.into()));
    true
}

fn reasoning_effort_needs_output_headroom(effort: &str) -> bool {
    let effort = effort.trim();
    !(effort.is_empty()
        || effort.eq_ignore_ascii_case("none")
        || effort.eq_ignore_ascii_case("minimal")
        || effort.eq_ignore_ascii_case("low")
        || effort.eq_ignore_ascii_case("medium"))
}

fn omitted_reasoning_output_limit(model: &str, bridge: ProtocolBridge) -> Option<u64> {
    match claude_output_token_cap(model) {
        Some(cap) => {
            let limit = cap.min(CLAUDE_HIGH_EFFORT_OUTPUT_TOKENS);
            (limit > DEFAULT_ANTHROPIC_MAX_TOKENS).then_some(limit)
        }
        None if bridge == ProtocolBridge::ResponsesToAnthropicMessages => {
            Some(CLAUDE_HIGH_EFFORT_OUTPUT_TOKENS)
        }
        None => None,
    }
}

/// 已知的同步 Messages 输出上限。不是 Claude 家族时返回 None。
/// 旧模型停在 8192 或更低，调用方因此不会把它们抬高。
fn claude_output_token_cap(model: &str) -> Option<u64> {
    let tokens = claude_model_tokens(model);
    if !tokens
        .iter()
        .any(|token| matches!(token.as_str(), "claude" | "fable" | "mythos"))
    {
        return None;
    }
    let Some(index) = tokens.iter().position(|token| {
        matches!(
            token.as_str(),
            "opus" | "sonnet" | "haiku" | "fable" | "mythos"
        )
    }) else {
        let major = tokens.iter().find_map(|token| small_model_version(token));
        return Some(match major {
            Some(major) if major <= 3 => DEFAULT_ANTHROPIC_MAX_TOKENS,
            _ => CLAUDE_HIGH_EFFORT_OUTPUT_TOKENS,
        });
    };
    let family = tokens[index].as_str();
    if matches!(family, "fable" | "mythos") {
        return Some(128_000);
    }
    let version = version_after(&tokens, index).or_else(|| version_before(&tokens, index));
    Some(match (family, version) {
        (_, None) => CLAUDE_HIGH_EFFORT_OUTPUT_TOKENS,
        ("opus", Some((major, _))) if major <= 3 => 4_096,
        ("opus", Some((4, minor))) if minor <= 1 => 32_000,
        ("opus", Some((4, minor))) if minor <= 5 => CLAUDE_HIGH_EFFORT_OUTPUT_TOKENS,
        ("opus", Some(_)) => 128_000,
        ("sonnet", Some((3, minor))) if minor >= 7 => CLAUDE_HIGH_EFFORT_OUTPUT_TOKENS,
        ("sonnet", Some((major, _))) if major <= 3 => DEFAULT_ANTHROPIC_MAX_TOKENS,
        ("sonnet", Some((4, minor))) if minor <= 5 => CLAUDE_HIGH_EFFORT_OUTPUT_TOKENS,
        ("sonnet", Some(_)) => 128_000,
        ("haiku", Some((major, _))) if major <= 3 => DEFAULT_ANTHROPIC_MAX_TOKENS,
        ("haiku", Some(_)) => CLAUDE_HIGH_EFFORT_OUTPUT_TOKENS,
        _ => CLAUDE_HIGH_EFFORT_OUTPUT_TOKENS,
    })
}

fn claude_model_tokens(model: &str) -> Vec<String> {
    let trimmed = model.trim();
    let without_marker = strip_anthropic_context_1m_model(trimmed).unwrap_or(trimmed);
    let segment = without_marker
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(without_marker);
    segment
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn small_model_version(token: &str) -> Option<u32> {
    if token.len() > 2 || !token.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    token.parse().ok()
}

fn version_after(tokens: &[String], index: usize) -> Option<(u32, u32)> {
    let major = small_model_version(tokens.get(index + 1)?)?;
    let minor = tokens
        .get(index + 2)
        .and_then(|token| small_model_version(token))
        .unwrap_or(0);
    Some((major, minor))
}

fn version_before(tokens: &[String], index: usize) -> Option<(u32, u32)> {
    let mut numbers = Vec::new();
    let mut cursor = index;
    while cursor > 0 {
        cursor -= 1;
        match small_model_version(&tokens[cursor]) {
            Some(number) => numbers.push(number),
            None => break,
        }
        if numbers.len() == 2 {
            break;
        }
    }
    numbers.reverse();
    match numbers.as_slice() {
        [major, minor] => Some((*major, *minor)),
        [major] => Some((*major, 0)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output_limit(model: &str, effort: &str, bridge: ProtocolBridge) -> Option<u64> {
        let mut body = json!({
            "model": model,
            "reasoning": {"effort": effort}
        });
        ensure_omitted_reasoning_output_limit(&mut body, bridge)
            .then(|| body["max_output_tokens"].as_u64().unwrap())
    }

    #[test]
    fn high_effort_claude_requests_fill_a_model_capped_output_limit() {
        let cases = [
            (
                "claude-opus-5",
                "high",
                ProtocolBridge::NativeResponses,
                Some(64_000),
            ),
            (
                "claude-opus-5",
                "ULTRA",
                ProtocolBridge::ResponsesToChatCompletions,
                Some(64_000),
            ),
            (
                "claude-opus-5",
                "xhigh",
                ProtocolBridge::ResponsesToAnthropicMessages,
                Some(64_000),
            ),
            (
                "claude-opus-5",
                "max",
                ProtocolBridge::NativeResponses,
                Some(64_000),
            ),
            (
                "claude-opus-5[1m]",
                "high",
                ProtocolBridge::NativeResponses,
                Some(64_000),
            ),
            (
                "relay/claude-opus-5",
                "high",
                ProtocolBridge::ResponsesToChatCompletions,
                Some(64_000),
            ),
            (
                "claude-opus-4-20250514",
                "high",
                ProtocolBridge::NativeResponses,
                Some(32_000),
            ),
            (
                "claude-opus-4-1-20250805",
                "high",
                ProtocolBridge::ResponsesToAnthropicMessages,
                Some(32_000),
            ),
            (
                "claude-opus-4.5",
                "high",
                ProtocolBridge::ResponsesToChatCompletions,
                Some(64_000),
            ),
            (
                "claude-sonnet-4-6",
                "high",
                ProtocolBridge::NativeResponses,
                Some(64_000),
            ),
            (
                "claude-3-7-sonnet-20250219",
                "high",
                ProtocolBridge::ResponsesToAnthropicMessages,
                Some(64_000),
            ),
            (
                "claude-haiku-4-5",
                "high",
                ProtocolBridge::ResponsesToChatCompletions,
                Some(64_000),
            ),
            (
                "claude-3-5-sonnet-20241022",
                "high",
                ProtocolBridge::ResponsesToAnthropicMessages,
                None,
            ),
            (
                "claude-3-opus-20240229",
                "ultra",
                ProtocolBridge::NativeResponses,
                None,
            ),
            (
                "claude-opus-5",
                "medium",
                ProtocolBridge::ResponsesToAnthropicMessages,
                None,
            ),
            (
                "claude-opus-5",
                "low",
                ProtocolBridge::NativeResponses,
                None,
            ),
            (
                "claude-opus-5",
                "minimal",
                ProtocolBridge::ResponsesToChatCompletions,
                None,
            ),
            ("gpt-5.4", "high", ProtocolBridge::NativeResponses, None),
            (
                "gpt-5.4",
                "high",
                ProtocolBridge::ResponsesToChatCompletions,
                None,
            ),
            (
                "deepseek-v4-pro",
                "xhigh",
                ProtocolBridge::ResponsesToChatCompletions,
                None,
            ),
            (
                "gpt-5.4",
                "high",
                ProtocolBridge::ResponsesToAnthropicMessages,
                Some(64_000),
            ),
        ];
        for (model, effort, bridge, expected) in cases {
            assert_eq!(
                output_limit(model, effort, bridge),
                expected,
                "{model} {effort} {bridge:?}"
            );
        }
    }

    #[test]
    fn explicit_output_limit_and_top_level_effort_keep_their_meaning() {
        let mut explicit = json!({
            "model": "claude-opus-5",
            "reasoning": {"effort": "ultra"},
            "max_output_tokens": 2048
        });
        assert!(!ensure_omitted_reasoning_output_limit(
            &mut explicit,
            ProtocolBridge::ResponsesToAnthropicMessages
        ));
        assert_eq!(explicit["max_output_tokens"], 2048);

        let mut top_level = json!({
            "model": "claude-sonnet-4-6",
            "reasoning_effort": "high",
            "input": "hi"
        });
        assert!(ensure_omitted_reasoning_output_limit(
            &mut top_level,
            ProtocolBridge::ResponsesToChatCompletions
        ));
        let chat = responses_to_chat_completions_body(&top_level).unwrap();
        assert_eq!(chat["max_tokens"], 64_000);
        let anthropic = responses_to_anthropic_messages_body(&json!({
            "model": "claude-opus-5",
            "input": [{"type":"message","role":"user","content":"hi"}],
            "reasoning": {"effort": "high"},
            "max_output_tokens": 64_000
        }))
        .unwrap();
        assert_eq!(anthropic["max_tokens"], 64_000);
        assert_eq!(anthropic["output_config"]["effort"], "high");
    }
}
