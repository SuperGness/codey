use super::*;

tokio::task_local! {
    pub(crate) static XAI_RESPONSE_FIX: XaiResponseFix;
}

const XAI_HOST_SUFFIX: &str = "x.ai";
const XAI_SUPPORTED_TOOL_TYPES: &[&str] = &[
    "function",
    "web_search",
    "x_search",
    "image_generation",
    "collections_search",
    "file_search",
    "code_execution",
    "code_interpreter",
    "mcp",
    "shell",
];
const XAI_TOP_LEVEL_UNSUPPORTED_FIELDS: &[&str] = &["prompt_cache_retention", "safety_identifier"];
const GROK_45_UNSUPPORTED_FIELDS: &[&str] = &[
    "presence_penalty",
    "presencePenalty",
    "frequency_penalty",
    "frequencyPenalty",
    "stop",
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct XaiToolOrigin {
    namespace: String,
    name: String,
}

/// 请求侧展平出的工具名，用来把 xAI 回包里的扁平 `function_call` 还原成
/// Codex 认识的 `namespace` + 短名。没有 namespace 时仍然要改整数值浮点。
#[derive(Clone, Debug, Default)]
pub(crate) struct XaiResponseFix {
    names: HashMap<String, XaiToolOrigin>,
}

#[derive(Debug)]
pub(crate) struct XaiNativePrepared {
    pub(crate) request_changed: bool,
    pub(crate) response: XaiResponseFix,
}

pub(crate) fn upstream_is_xai(upstream_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(upstream_url.trim()) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    host == XAI_HOST_SUFFIX
        || host
            .strip_suffix(XAI_HOST_SUFFIX)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

/// 官方 xAI 地址，以及任何线路上的 Grok 模型。第三方中转不会用 `x.ai` 做域名，
/// 但上游模型名仍然是 `grok-…`。
pub(crate) fn native_upstream_needs_xai_compat(upstream_url: &str, upstream_model: &str) -> bool {
    upstream_is_xai(upstream_url) || model_is_grok(upstream_model)
}

pub(crate) fn model_is_grok(model: &str) -> bool {
    let bare = model.trim().rsplit('/').next().unwrap_or("").trim();
    let bytes = bare.as_bytes();
    bytes.eq_ignore_ascii_case(b"grok")
        || bytes.get(..5).is_some_and(|prefix| {
            prefix[..4].eq_ignore_ascii_case(b"grok") && matches!(prefix[4], b'-' | b'.' | b'_')
        })
}

pub(crate) fn current_xai_response_fix() -> Option<XaiResponseFix> {
    XAI_RESPONSE_FIX.try_with(|fix| fix.clone()).ok()
}

/// 整理发往 Grok 原生 Responses 的请求。先提升 `additional_tools`，再展平
/// namespace，最后删掉 Grok 不接受的字段和工具。无法展开或无法表达的
/// 加密任务直接拒绝。
pub(crate) fn prepare_xai_native_request(body: &mut Value) -> Result<XaiNativePrepared> {
    let mut changed = promote_additional_tools(body);
    let (flattened, names) = flatten_namespaces(body)?;
    changed |= flattened;
    changed |= remove_unsupported_fields(body);
    changed |= strip_null_reasoning_content(body);
    changed |= filter_unsupported_tools(body);
    changed |= normalize_function_tool_schemas(body);
    changed |= prepare_agent_messages(body)?;
    Ok(XaiNativePrepared {
        request_changed: changed,
        response: XaiResponseFix { names },
    })
}

impl XaiResponseFix {
    pub(crate) fn apply(&self, value: &mut Value) -> bool {
        let mut changed = restore_function_names(value, &self.names);
        changed |= rewrite_completed_integer_arguments(value);
        changed
    }
}

fn promote_additional_tools(body: &mut Value) -> bool {
    let Some(input) = body.get("input").and_then(Value::as_array) else {
        return false;
    };
    if !input
        .iter()
        .any(|item| item.get("type").and_then(Value::as_str) == Some("additional_tools"))
    {
        return false;
    }
    let input = input.clone();
    let mut merged = Vec::new();
    let mut seen = HashSet::new();
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        for tool in tools {
            seen.insert(tool_dedup_key(tool));
            merged.push(tool.clone());
        }
    }
    let mut filtered = Vec::with_capacity(input.len());
    let mut promoted = false;
    for item in input {
        if item.get("type").and_then(Value::as_str) == Some("additional_tools") {
            if let Some(tools) = item.get("tools").and_then(Value::as_array) {
                for tool in tools {
                    if seen.insert(tool_dedup_key(tool)) {
                        merged.push(tool.clone());
                        promoted = true;
                    }
                }
            }
            continue;
        }
        filtered.push(item);
    }
    let Some(body) = body.as_object_mut() else {
        return false;
    };
    body.insert("input".to_string(), Value::Array(filtered));
    if promoted {
        body.insert("tools".to_string(), Value::Array(merged));
    }
    true
}

fn tool_dedup_key(tool: &Value) -> String {
    let tool_type = tool
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if !tool_type.is_empty()
        && let Some(name) = tool.get("name").and_then(Value::as_str).map(str::trim)
        && !name.is_empty()
    {
        return format!("type:{tool_type}\u{0}name:{name}");
    }
    if tool_type == "mcp"
        && let Some(label) = tool
            .get("server_label")
            .and_then(Value::as_str)
            .map(str::trim)
        && !label.is_empty()
    {
        return format!("type:mcp\u{0}server_label:{label}");
    }
    format!("json:{tool}")
}

fn flatten_namespaces(body: &mut Value) -> Result<(bool, HashMap<String, XaiToolOrigin>)> {
    let Some(tools) = body.get("tools").and_then(Value::as_array) else {
        return Ok((false, HashMap::new()));
    };
    if !tools.iter().any(contains_namespace_tool) {
        return Ok((false, HashMap::new()));
    }
    let mut top_level = HashSet::new();
    for tool in tools {
        let tool_type = tool.get("type").and_then(Value::as_str).unwrap_or("");
        if (tool_type == "function" || tool_type == "custom")
            && let Some(name) = tool.get("name").and_then(Value::as_str).map(str::trim)
            && !name.is_empty()
        {
            top_level.insert(name.to_string());
        }
    }
    let mut owners = HashMap::new();
    collect_namespace_owners(tools, &[], &top_level, &mut owners)?;
    let tools = tools.clone();
    let mut flattened = Vec::new();
    let mut seen = HashSet::new();
    for tool in tools {
        if tool.get("type").and_then(Value::as_str) == Some("namespace") {
            lift_namespace_tool(&tool, &[], &owners, &mut seen, &mut flattened);
        } else {
            flattened.push(tool);
        }
    }
    if let Some(body) = body.as_object_mut() {
        body.insert("tools".to_string(), Value::Array(flattened));
        if let Some(input) = body.get_mut("input") {
            rewrite_namespace_calls(input, &owners);
        }
        if let Some(choice) = body.get_mut("tool_choice") {
            if choice.get("type").and_then(Value::as_str) == Some("namespace") {
                *choice = json!("auto");
            } else {
                rewrite_namespace_call(choice, &owners);
            }
        }
    }
    Ok((true, owners))
}

fn contains_namespace_tool(tool: &Value) -> bool {
    if tool.get("type").and_then(Value::as_str) == Some("namespace") {
        return true;
    }
    tool.get("tools")
        .or_else(|| tool.get("children"))
        .and_then(Value::as_array)
        .is_some_and(|tools| tools.iter().any(contains_namespace_tool))
}

fn collect_namespace_owners(
    tools: &[Value],
    namespace: &[String],
    top_level: &HashSet<String>,
    owners: &mut HashMap<String, XaiToolOrigin>,
) -> Result<()> {
    for tool in tools {
        if tool.get("type").and_then(Value::as_str) != Some("namespace") {
            continue;
        }
        let Some(name) = tool.get("name").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let mut path = namespace.to_vec();
        path.push(name.to_string());
        if path.len() > MAX_RESPONSES_NAMESPACE_DEPTH {
            anyhow::bail!("namespace 嵌套层级超过 {MAX_RESPONSES_NAMESPACE_DEPTH}");
        }
        let children = namespace_children(tool);
        for child in &children {
            if child.get("type").and_then(Value::as_str) == Some("namespace") {
                collect_namespace_owners(std::slice::from_ref(child), &path, top_level, owners)?;
                continue;
            }
            if child.get("type").and_then(Value::as_str) != Some("function") {
                continue;
            }
            let Some(child_name) = child.get("name").and_then(Value::as_str).map(str::trim) else {
                continue;
            };
            if child_name.is_empty() {
                continue;
            }
            let flat = namespaced_upstream_tool_name(&path, child_name);
            let origin = XaiToolOrigin {
                namespace: path.join("."),
                name: child_name.to_string(),
            };
            if top_level.contains(&flat) {
                anyhow::bail!(
                    "namespace 工具 {}.{} 展开后与已有工具 {flat} 重名",
                    origin.namespace,
                    origin.name
                );
            }
            if let Some(previous) = owners.get(&flat)
                && previous != &origin
            {
                anyhow::bail!(
                    "namespace 工具 {}.{} 与 {}.{} 展开后重名",
                    previous.namespace,
                    previous.name,
                    origin.namespace,
                    origin.name
                );
            }
            owners.insert(flat, origin);
        }
    }
    Ok(())
}

fn lift_namespace_tool(
    tool: &Value,
    namespace: &[String],
    owners: &HashMap<String, XaiToolOrigin>,
    seen: &mut HashSet<String>,
    flattened: &mut Vec<Value>,
) {
    let Some(name) = tool.get("name").and_then(Value::as_str).map(str::trim) else {
        return;
    };
    if name.is_empty() {
        return;
    }
    let mut path = namespace.to_vec();
    path.push(name.to_string());
    for child in namespace_children(tool) {
        if child.get("type").and_then(Value::as_str) == Some("namespace") {
            lift_namespace_tool(&child, &path, owners, seen, flattened);
            continue;
        }
        if child.get("type").and_then(Value::as_str) != Some("function") {
            continue;
        }
        let Some(child_name) = child.get("name").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        let child_name = child_name.to_string();
        let flat = namespaced_upstream_tool_name(&path, &child_name);
        if !owners.contains_key(&flat) || !seen.insert(flat.clone()) {
            continue;
        }
        let mut lifted = child;
        if let Some(object) = lifted.as_object_mut() {
            object.insert("name".to_string(), Value::String(flat));
            object.remove("namespace");
        }
        if is_automation_update_tool(&child_name) {
            rewrite_tool_parameters(&mut lifted, xai_empty_object_schema());
            disable_strict(&mut lifted);
        }
        flattened.push(lifted);
    }
}

fn namespace_children(tool: &Value) -> Vec<Value> {
    tool.get("tools")
        .or_else(|| tool.get("children"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn rewrite_namespace_calls(value: &mut Value, owners: &HashMap<String, XaiToolOrigin>) {
    match value {
        Value::Array(items) => {
            for item in items {
                rewrite_namespace_calls(item, owners);
            }
        }
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("function_call") {
                rewrite_namespace_call(value, owners);
                return;
            }
            for child in object.values_mut() {
                rewrite_namespace_calls(child, owners);
            }
        }
        _ => {}
    }
}

fn rewrite_namespace_call(item: &mut Value, owners: &HashMap<String, XaiToolOrigin>) -> bool {
    let Some(object) = item.as_object_mut() else {
        return false;
    };
    let namespace = object
        .get("namespace")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if namespace.is_empty() || name.is_empty() {
        return false;
    }
    let path: Vec<String> = namespace.split('.').map(str::to_string).collect();
    let flat = namespaced_upstream_tool_name(&path, &name);
    let Some(origin) = owners.get(&flat) else {
        return false;
    };
    if origin.namespace != namespace || origin.name != name {
        return false;
    }
    object.insert("name".to_string(), Value::String(flat));
    object.remove("namespace");
    true
}

fn restore_function_names(value: &mut Value, names: &HashMap<String, XaiToolOrigin>) -> bool {
    if names.is_empty() {
        return false;
    }
    match value {
        Value::Array(items) => items.iter_mut().fold(false, |changed, item| {
            changed | restore_function_names(item, names)
        }),
        Value::Object(object) => {
            let mut changed = false;
            if object.get("type").and_then(Value::as_str) == Some("function_call")
                && let Some(flat) = object.get("name").and_then(Value::as_str)
                && let Some(origin) = names.get(flat)
            {
                object.insert("name".to_string(), Value::String(origin.name.clone()));
                object.insert(
                    "namespace".to_string(),
                    Value::String(origin.namespace.clone()),
                );
                changed = true;
            }
            for child in object.values_mut() {
                changed |= restore_function_names(child, names);
            }
            changed
        }
        _ => false,
    }
}

fn remove_unsupported_fields(body: &mut Value) -> bool {
    let grok_45 = request_targets_grok_45(body);
    let mut changed = false;
    if let Some(object) = body.as_object_mut() {
        for field in XAI_TOP_LEVEL_UNSUPPORTED_FIELDS {
            changed |= object.remove(*field).is_some();
        }
        if grok_45 {
            for field in GROK_45_UNSUPPORTED_FIELDS {
                changed |= object.remove(*field).is_some();
            }
        }
    }
    changed | remove_field_recursive(body, "external_web_access")
}

fn request_targets_grok_45(body: &Value) -> bool {
    let Some(model) = body.get("model").and_then(Value::as_str) else {
        return false;
    };
    let model = model.trim().rsplit('/').next().unwrap_or("").trim();
    model.eq_ignore_ascii_case("grok-4.5")
}

fn remove_field_recursive(value: &mut Value, field: &str) -> bool {
    match value {
        Value::Object(object) => {
            let mut changed = object.remove(field).is_some();
            for child in object.values_mut() {
                changed |= remove_field_recursive(child, field);
            }
            changed
        }
        Value::Array(items) => items.iter_mut().fold(false, |changed, item| {
            changed | remove_field_recursive(item, field)
        }),
        _ => false,
    }
}

fn strip_null_reasoning_content(body: &mut Value) -> bool {
    let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return false;
    };
    let mut changed = false;
    for item in input {
        if item.get("type").and_then(Value::as_str) != Some("reasoning") {
            continue;
        }
        if let Some(object) = item.as_object_mut()
            && matches!(object.get("content"), Some(Value::Null))
        {
            object.remove("content");
            changed = true;
        }
    }
    changed
}

fn filter_unsupported_tools(body: &mut Value) -> bool {
    let Some((original_len, filtered)) = body.get("tools").and_then(Value::as_array).map(|tools| {
        let filtered = tools
            .iter()
            .filter(|tool| {
                XAI_SUPPORTED_TOOL_TYPES.contains(
                    &tool
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim(),
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        (tools.len(), filtered)
    }) else {
        return false;
    };
    let drop_choice = body.get("tool_choice").is_some() && should_drop_tool_choice(body, &filtered);
    let mut changed = false;
    if let Some(object) = body.as_object_mut() {
        if filtered.len() != original_len {
            if filtered.is_empty() {
                object.remove("tools");
            } else {
                object.insert("tools".to_string(), Value::Array(filtered));
            }
            changed = true;
        }
        if drop_choice {
            object.remove("tool_choice");
            changed = true;
        }
    }
    changed
}

fn should_drop_tool_choice(body: &Value, tools: &[Value]) -> bool {
    let Some(choice) = body.get("tool_choice") else {
        return false;
    };
    if tools.is_empty() {
        return true;
    }
    let Some(choice) = choice.as_object() else {
        return false;
    };
    let choice_type = choice
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if choice_type.is_empty() || !XAI_SUPPORTED_TOOL_TYPES.contains(&choice_type) {
        return !choice_type.is_empty();
    }
    if choice_type != "function" {
        return false;
    }
    let Some(name) = choice
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| {
            choice
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|name| !name.is_empty())
    else {
        return false;
    };
    !tools.iter().any(|tool| {
        tool.get("type").and_then(Value::as_str) == Some("function")
            && tool.get("name").and_then(Value::as_str).map(str::trim) == Some(name)
    })
}

fn normalize_function_tool_schemas(body: &mut Value) -> bool {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return false;
    };
    let mut changed = false;
    for tool in tools {
        changed |= normalize_function_tool_schema(tool);
    }
    changed
}

fn normalize_function_tool_schema(tool: &mut Value) -> bool {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return false;
    }
    let name = tool
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if is_automation_update_tool(name) {
        return rewrite_tool_parameters(tool, xai_empty_object_schema()) | disable_strict(tool);
    }
    let Some(parameters) = tool.get("parameters").cloned() else {
        return rewrite_tool_parameters(tool, xai_empty_object_schema());
    };
    let Some(simplified) = simplify_parameter_root(&parameters) else {
        return false;
    };
    rewrite_tool_parameters(tool, simplified)
}

fn is_automation_update_tool(name: &str) -> bool {
    name == "automation_update"
        || name.ends_with("__automation_update")
        || name.split("__").any(|part| part == "automation_update")
}

fn xai_empty_object_schema() -> Value {
    json!({"type":"object","properties":{},"additionalProperties":true})
}

fn simplify_parameter_root(parameters: &Value) -> Option<Value> {
    let object = parameters.as_object()?;
    let union = object
        .get("anyOf")
        .or_else(|| object.get("oneOf"))
        .and_then(Value::as_array)?;
    Some(flatten_union_branches(union))
}

fn flatten_union_branches(branches: &[Value]) -> Value {
    let objects: Vec<&Value> = branches
        .iter()
        .filter(|branch| branch.get("type").and_then(Value::as_str) == Some("object"))
        .collect();
    if objects.len() == 1 {
        let mut result = objects[0].clone();
        if let Some(object) = result.as_object_mut() {
            object.insert("type".to_string(), json!("object"));
            object.remove("anyOf");
            object.remove("oneOf");
            object
                .entry("properties".to_string())
                .or_insert_with(|| json!({}));
        }
        return result;
    }
    if objects.is_empty() {
        return xai_empty_object_schema();
    }
    let mut properties = serde_json::Map::new();
    let mut required: Option<Vec<Value>> = None;
    for branch in objects {
        if let Some(branch_properties) = branch.get("properties").and_then(Value::as_object) {
            for (key, value) in branch_properties {
                properties
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
            }
        }
        let branch_required = branch
            .get("required")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        required = Some(match required {
            None => branch_required,
            Some(existing) => existing
                .into_iter()
                .filter(|item| branch_required.contains(item))
                .collect(),
        });
    }
    let mut result = json!({
        "type": "object",
        "properties": Value::Object(properties),
    });
    let required = required.unwrap_or_default();
    if !required.is_empty() {
        result["required"] = Value::Array(required);
    }
    result
}

fn rewrite_tool_parameters(tool: &mut Value, parameters: Value) -> bool {
    let Some(object) = tool.as_object_mut() else {
        return false;
    };
    if object.get("parameters") == Some(&parameters) {
        return false;
    }
    object.insert("parameters".to_string(), parameters);
    true
}

fn disable_strict(tool: &mut Value) -> bool {
    let Some(object) = tool.as_object_mut() else {
        return false;
    };
    if object.get("strict") == Some(&Value::Bool(true)) {
        object.insert("strict".to_string(), Value::Bool(false));
        true
    } else {
        false
    }
}

fn prepare_agent_messages(body: &mut Value) -> Result<bool> {
    let normalized = normalize_encrypted_agent_payloads(body);
    reject_sealed_agent_messages(body)?;
    Ok(normalized | rewrite_agent_messages(body))
}

fn reject_sealed_agent_messages(value: &Value) -> Result<()> {
    match value {
        Value::Array(items) => {
            for item in items {
                reject_sealed_agent_messages(item)?;
            }
        }
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("agent_message")
                && object
                    .get("content")
                    .is_some_and(contains_encrypted_content)
            {
                anyhow::bail!("xAI 不接受带加密任务正文的 agent_message，请重新提供明文任务");
            }
            for child in object.values() {
                reject_sealed_agent_messages(child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn contains_encrypted_content(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(contains_encrypted_content),
        Value::Object(object) => {
            object.get("type").and_then(Value::as_str) == Some("encrypted_content")
                || object.values().any(contains_encrypted_content)
        }
        _ => false,
    }
}

fn rewrite_agent_messages(value: &mut Value) -> bool {
    match value {
        Value::Array(items) => items.iter_mut().fold(false, |changed, item| {
            changed | rewrite_agent_messages(item)
        }),
        Value::Object(object) => {
            let mut changed = false;
            if object.get("type").and_then(Value::as_str) == Some("agent_message") {
                object.insert("type".to_string(), json!("message"));
                object.insert("role".to_string(), json!("user"));
                changed = true;
            }
            for child in object.values_mut() {
                changed |= rewrite_agent_messages(child);
            }
            changed
        }
        _ => false,
    }
}

fn rewrite_completed_integer_arguments(value: &mut Value) -> bool {
    match value {
        Value::Array(items) => items.iter_mut().fold(false, |changed, item| {
            changed | rewrite_completed_integer_arguments(item)
        }),
        Value::Object(object) => {
            let event_type = object
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string);
            if event_type.as_deref() == Some("response.function_call_arguments.delta") {
                return false;
            }
            let mut changed = false;
            if matches!(
                event_type.as_deref(),
                Some("response.function_call_arguments.done" | "function_call")
            ) {
                changed |= rewrite_arguments_field(object);
            }
            for child in object.values_mut() {
                changed |= rewrite_completed_integer_arguments(child);
            }
            changed
        }
        _ => false,
    }
}

fn rewrite_arguments_field(object: &mut serde_json::Map<String, Value>) -> bool {
    match object.get_mut("arguments") {
        Some(Value::String(arguments)) => {
            let Ok(mut parsed) = serde_json::from_str::<Value>(arguments) else {
                return false;
            };
            if !rewrite_whole_number_floats(&mut parsed) {
                return false;
            }
            let Ok(encoded) = serde_json::to_string(&parsed) else {
                return false;
            };
            *arguments = encoded;
            true
        }
        Some(other) => rewrite_whole_number_floats(other),
        None => false,
    }
}

fn rewrite_whole_number_floats(value: &mut Value) -> bool {
    match value {
        Value::Number(number) => {
            if let Some(integer) = whole_float_to_json_int(number) {
                *number = integer;
                true
            } else {
                false
            }
        }
        Value::Array(items) => items.iter_mut().fold(false, |changed, item| {
            changed | rewrite_whole_number_floats(item)
        }),
        Value::Object(object) => object.values_mut().fold(false, |changed, child| {
            changed | rewrite_whole_number_floats(child)
        }),
        _ => false,
    }
}

fn whole_float_to_json_int(number: &serde_json::Number) -> Option<serde_json::Number> {
    if number.is_i64() || number.is_u64() {
        return None;
    }
    let float = number.as_f64()?;
    if !float.is_finite() || float.fract() != 0.0 {
        return None;
    }
    if float >= 0.0 {
        if float >= u64::MAX as f64 {
            return None;
        }
        let integer = float as u64;
        if integer as f64 != float {
            return None;
        }
        Some(serde_json::Number::from(integer))
    } else {
        if float < i64::MIN as f64 {
            return None;
        }
        let integer = float as i64;
        if integer as f64 != float {
            return None;
        }
        Some(serde_json::Number::from(integer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn sealed_task() -> String {
        let mut token = vec![0x80];
        token.extend_from_slice(&1_700_000_000u64.to_be_bytes());
        token.extend_from_slice(&[0x11; 16]);
        token.extend_from_slice(&[0x33; 16]);
        token.extend_from_slice(&[0x22; 32]);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token)
    }

    #[test]
    fn xai_host_matches_api_and_subdomains_only() {
        assert!(upstream_is_xai("https://api.x.ai/v1/responses"));
        assert!(upstream_is_xai("https://x.ai/v1/responses"));
        assert!(upstream_is_xai("https://API.X.AI/v1"));
        assert!(!upstream_is_xai("https://api.openai.com/v1/responses"));
        assert!(!upstream_is_xai("https://x.ai.example.com/v1"));
        assert!(!upstream_is_xai("not a url"));
        assert!(native_upstream_needs_xai_compat(
            "https://relay.example.com/v1/responses",
            "x-ai/grok-4"
        ));
        assert!(native_upstream_needs_xai_compat(
            "https://api.x.ai/v1/responses",
            "custom-model"
        ));
        assert!(model_is_grok("grok-4.5-fast"));
        assert!(model_is_grok("org/grok_code"));
        assert!(!model_is_grok("gpt-5"));
        assert!(!model_is_grok("grokking-bot"));
        assert!(!native_upstream_needs_xai_compat(
            "https://relay.example.com/v1/responses",
            "gpt-5"
        ));
    }

    #[test]
    fn namespace_tools_round_trip_and_history_calls_are_flattened() {
        let mut body = json!({
            "model": "grok-4",
            "tools": [
                {"type":"function","name":"plain","parameters":{"type":"object","properties":{}}},
                {
                    "type":"namespace",
                    "name":"mcp__codex",
                    "tools":[{
                        "type":"function",
                        "name":"read_file",
                        "parameters":{"type":"object","properties":{"path":{"type":"string"}}}
                    }]
                },
                {"type":"tool_search","name":"tool_search"}
            ],
            "input":[{
                "type":"function_call",
                "name":"read_file",
                "namespace":"mcp__codex",
                "arguments":"{}"
            }],
            "tool_choice":{"type":"namespace","name":"mcp__codex"},
            "safety_identifier":"sid"
        });
        let prepared = prepare_xai_native_request(&mut body).unwrap();
        assert!(prepared.request_changed);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "plain");
        let flat = tools[1]["name"].as_str().unwrap();
        assert_eq!(body["input"][0]["name"], flat);
        assert!(body["input"][0].get("namespace").is_none());
        assert_eq!(body["tool_choice"], "auto");
        assert!(body.get("safety_identifier").is_none());

        let mut event = json!({
            "type":"response.output_item.done",
            "item":{
                "type":"function_call",
                "name": flat,
                "arguments":"{\"line\":92116.0,\"ratio\":1.5}"
            }
        });
        assert!(prepared.response.apply(&mut event));
        assert_eq!(event["item"]["name"], "read_file");
        assert_eq!(event["item"]["namespace"], "mcp__codex");
        assert_eq!(event["item"]["arguments"], "{\"line\":92116,\"ratio\":1.5}");
    }

    #[test]
    fn colliding_namespace_tool_is_rejected() {
        let flat = namespaced_upstream_tool_name(&["mcp__codex".to_string()], "read_file");
        let mut body = json!({
            "model":"grok-4",
            "tools":[
                {"type":"function","name":flat,"parameters":{"type":"object"}},
                {"type":"namespace","name":"mcp__codex","tools":[
                    {"type":"function","name":"read_file","parameters":{"type":"object"}}
                ]}
            ]
        });
        let error = prepare_xai_native_request(&mut body).unwrap_err();
        assert!(error.to_string().contains("重名"), "{error}");
    }

    #[test]
    fn grok_45_drops_sampling_fields_and_agent_message_becomes_user_text() {
        let mut body = json!({
            "model":"org/grok-4.5",
            "stop":["END"],
            "presence_penalty":0.1,
            "tools":[{
                "type":"function",
                "name":"mcp__codex__automation_update",
                "strict":true,
                "parameters":{"anyOf":[{"type":"object","properties":{"mode":{"type":"string"}}},{"type":"null"}]}
            }],
            "input":[{
                "type":"agent_message",
                "content":[{"type":"encrypted_content","encrypted_content":"run the tests"}]
            },{
                "type":"reasoning",
                "content":null
            }]
        });
        assert!(
            prepare_xai_native_request(&mut body)
                .unwrap()
                .request_changed
        );
        assert!(body.get("stop").is_none());
        assert!(body.get("presence_penalty").is_none());
        assert_eq!(body["tools"][0]["strict"], false);
        assert_eq!(body["tools"][0]["parameters"]["type"], "object");
        assert!(body["tools"][0]["parameters"].get("anyOf").is_none());
        assert_eq!(body["input"][0]["type"], "message");
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["input"][0]["content"][0]["text"], "run the tests");
        assert!(body["input"][1].get("content").is_none());

        let mut kept = json!({"model":"grok-4","stop":["END"]});
        prepare_xai_native_request(&mut kept).unwrap();
        assert_eq!(kept["stop"][0], "END");
    }

    #[test]
    fn sealed_agent_message_is_rejected() {
        let mut body = json!({
            "model":"grok-4",
            "input":[{
                "type":"agent_message",
                "content":[{"type":"encrypted_content","encrypted_content":sealed_task()}]
            }]
        });
        let error = prepare_xai_native_request(&mut body).unwrap_err();
        assert!(error.to_string().contains("加密任务正文"), "{error}");
    }

    #[test]
    fn argument_deltas_and_fractional_numbers_stay_unchanged() {
        let fix = XaiResponseFix::default();
        let mut delta = json!({
            "type":"response.function_call_arguments.delta",
            "delta":"92116.0"
        });
        assert!(!fix.apply(&mut delta));
        assert_eq!(delta["delta"], "92116.0");
        let original = br#"{"type":"response.created","response":{"id":"resp"}}"#;
        assert!(matches!(
            NativeResponseFix::from(fix).rewrite_json_bytes(original),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn sse_frames_restore_names_across_split_chunks() {
        let mut body = json!({
            "model":"grok-4",
            "tools":[{"type":"namespace","name":"pkg","tools":[
                {"type":"function","name":"lookup","parameters":{"type":"object","properties":{}}}
            ]}]
        });
        let prepared = prepare_xai_native_request(&mut body).unwrap();
        let flat = body["tools"][0]["name"].as_str().unwrap();
        let frame = format!(
            "event: response.output_item.done\ndata: {{\"type\":\"function_call\",\"name\":{flat:?},\"arguments\":\"{{\\\"n\\\":1.0}}\"}}\n\n"
        );
        let fix = NativeResponseFix::from(prepared.response);
        let mut rewriter = ResponsesSseRewriter::new(Some(&fix), true, None);
        let split = frame.len() / 2;
        assert!(
            rewriter
                .push(&frame.as_bytes()[..split])
                .unwrap()
                .is_empty()
        );
        let output = rewriter.push(&frame.as_bytes()[split..]).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("event: response.output_item.done"), "{text}");
        assert!(text.contains("\"name\":\"lookup\""), "{text}");
        assert!(text.contains("\"namespace\":\"pkg\""), "{text}");
        assert!(text.contains("\"arguments\":\"{\\\"n\\\":1}\""), "{text}");
    }
}
