// Convert OpenAI Responses requests to the Antigravity Gemini wire format.
// Port of proxy/lib/convert.js + the schema/boundary logic of upstream
// pi-antigravity 0.8.0 stream.ts. Pure functions; no I/O.
use crate::catalog::{self, Effort};
use crate::config::CONTINUATION_TEXT;
use crate::security::stable_uuid;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Mutex;

pub const GEMINI_ROLE_USER: &str = "user";
pub const GEMINI_ROLE_MODEL: &str = "model";

// --- JSON helpers ------------------------------------------------------------

fn is_obj(v: &Value) -> bool {
    v.is_object()
}

// --- Schema: $ref dereferencing (upstream stream.ts) --------------------------

const MAX_SCHEMA_DEREF_DEPTH: usize = 64;
const MAX_SCHEMA_DEREF_NODES: usize = 10_000;

const SCHEMA_MAP_KEYWORDS: &[&str] = &[
    "properties",
    "patternProperties",
    "dependentSchemas",
    "dependencies",
];
const SCHEMA_VALUE_KEYWORDS: &[&str] = &[
    "additionalItems",
    "additionalProperties",
    "contains",
    "contentSchema",
    "else",
    "if",
    "items",
    "not",
    "propertyNames",
    "then",
    "unevaluatedItems",
    "unevaluatedProperties",
];
const SCHEMA_ARRAY_KEYWORDS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems"];

fn resolve_local_json_pointer(r#ref: &str, root: &Value) -> Option<Value> {
    if r#ref == "#" {
        return Some(root.clone());
    }
    let rest = r#ref.strip_prefix("#/")?;
    let mut current = root.clone();
    for token in rest.split('/') {
        let key = token.replace("~1", "/").replace("~0", "~");
        if current.is_array() {
            let idx: usize = key.parse().ok()?;
            current = current.get(idx)?.clone();
            continue;
        }
        current = current.get(&key)?.clone();
    }
    Some(current)
}

struct DerefResult {
    schema: Value,
    issues: Vec<String>,
}

#[derive(Default)]
struct DerefState {
    nodes: usize,
}

fn dereference_schema(
    schema: &Value,
    root: &Value,
    ref_stack: &mut std::collections::HashSet<String>,
    object_stack: &mut Vec<usize>,
    state: &mut DerefState,
    path: &str,
    depth: usize,
) -> DerefResult {
    if depth > MAX_SCHEMA_DEREF_DEPTH {
        return DerefResult {
            schema: json!({}),
            issues: vec![format!(
                "{path}: schema expansion exceeded {MAX_SCHEMA_DEREF_DEPTH} levels"
            )],
        };
    }
    state.nodes += 1;
    if state.nodes > MAX_SCHEMA_DEREF_NODES {
        return DerefResult {
            schema: json!({}),
            issues: vec![format!(
                "{path}: schema expansion exceeded {MAX_SCHEMA_DEREF_NODES} nodes"
            )],
        };
    }
    if !schema.is_object() && !schema.is_array() {
        return DerefResult {
            schema: schema.clone(),
            issues: vec![],
        };
    }

    if let Some(arr) = schema.as_array() {
        let mut out = Vec::new();
        let mut issues = Vec::new();
        for (i, item) in arr.iter().enumerate() {
            let r = dereference_schema(
                item,
                root,
                ref_stack,
                object_stack,
                state,
                &format!("{path}[{i}]"),
                depth + 1,
            );
            out.push(r.schema);
            issues.extend(r.issues);
        }
        return DerefResult {
            schema: Value::Array(out),
            issues,
        };
    }

    let obj = schema.as_object().unwrap();
    let ptr = std::ptr::from_ref(obj) as usize;
    if object_stack.contains(&ptr) {
        return DerefResult {
            schema: json!({}),
            issues: vec![format!("{path}: circular schema object")],
        };
    }
    object_stack.push(ptr);

    let result = if let Some(r#ref) = obj.get("$ref").and_then(|v| v.as_str()) {
        if ref_stack.contains(r#ref) {
            let detail = format!("{}: circular local reference {}", path, r#ref);
            DerefResult {
                schema: json!({}),
                issues: vec![detail],
            }
        } else if let Some(target) = resolve_local_json_pointer(r#ref, root) {
            ref_stack.insert(r#ref.to_string());
            let resolved = dereference_schema(
                &target,
                root,
                ref_stack,
                object_stack,
                state,
                path,
                depth + 1,
            );
            let mut siblings = obj.clone();
            siblings.remove("$ref");
            let sibling = dereference_schema(
                &Value::Object(siblings),
                root,
                ref_stack,
                object_stack,
                state,
                path,
                depth + 1,
            );
            ref_stack.remove(r#ref);
            if resolved.schema.is_object() && sibling.schema.is_object() {
                let mut merged = resolved.schema.as_object().unwrap().clone();
                merged.extend(sibling.schema.as_object().unwrap().clone());
                DerefResult {
                    schema: Value::Object(merged),
                    issues: [resolved.issues, sibling.issues].concat(),
                }
            } else {
                DerefResult {
                    schema: resolved.schema,
                    issues: [resolved.issues, sibling.issues].concat(),
                }
            }
        } else {
            let detail = format!(
                "{}: target of {} is not present in the root schema",
                path, r#ref
            );
            DerefResult {
                schema: json!({}),
                issues: vec![detail],
            }
        }
    } else {
        let mut out = Map::new();
        let mut issues = Vec::new();
        for (key, value) in obj {
            // $defs/definitions resolve through the root schema but must not be
            // emitted: the backend requires self-contained schemas.
            if key == "$defs" || key == "definitions" {
                continue;
            }
            if SCHEMA_MAP_KEYWORDS.contains(&key.as_str()) {
                // Map keywords hold user-defined property names; only the VALUES
                // are dereferenced (upstream dereferenceSchemaMap).
                if let Some(map) = value.as_object() {
                    let mut m = Map::new();
                    for (pk, pv) in map {
                        let r = dereference_schema(
                            pv,
                            root,
                            ref_stack,
                            object_stack,
                            state,
                            &format!("{path}.{key}.{pk}"),
                            depth + 1,
                        );
                        m.insert(pk.clone(), r.schema);
                        issues.extend(r.issues);
                    }
                    out.insert(key.clone(), Value::Object(m));
                } else if let Some(arr) = value.as_array() {
                    let mut a = Vec::new();
                    for (i, pv) in arr.iter().enumerate() {
                        let r = dereference_schema(
                            pv,
                            root,
                            ref_stack,
                            object_stack,
                            state,
                            &format!("{path}.{key}[{i}]"),
                            depth + 1,
                        );
                        a.push(r.schema);
                        issues.extend(r.issues);
                    }
                    out.insert(key.clone(), Value::Array(a));
                } else {
                    let r = dereference_schema(
                        value,
                        root,
                        ref_stack,
                        object_stack,
                        state,
                        &format!("{path}.{key}"),
                        depth + 1,
                    );
                    out.insert(key.clone(), r.schema);
                    issues.extend(r.issues);
                }
            } else if SCHEMA_VALUE_KEYWORDS.contains(&key.as_str())
                || SCHEMA_ARRAY_KEYWORDS.contains(&key.as_str())
            {
                let r = dereference_schema(
                    value,
                    root,
                    ref_stack,
                    object_stack,
                    state,
                    &format!("{path}.{key}"),
                    depth + 1,
                );
                out.insert(key.clone(), r.schema);
                issues.extend(r.issues);
            } else {
                out.insert(key.clone(), value.clone());
            }
        }
        DerefResult {
            schema: Value::Object(out),
            issues,
        }
    };

    object_stack.pop();
    result
}

fn ensure_root_object_schema(schema: Value) -> Value {
    match &schema {
        Value::Object(o) if o.get("type").is_none() => {
            let mut out = o.clone();
            out.insert("type".into(), json!("object"));
            if !out.contains_key("properties") {
                out.insert("properties".into(), json!({}));
            }
            Value::Object(out)
        }
        Value::Object(_) => schema,
        _ => json!({"type": "object", "properties": {}}),
    }
}

const META_SCHEMA_KEYWORDS: &[&str] = &[
    "$schema",
    "$id",
    "$anchor",
    "$dynamicAnchor",
    "$vocabulary",
    "$comment",
    "$defs",
    "definitions",
];

fn strip_meta_schema(schema: &Value) -> Value {
    match schema {
        Value::Array(items) => Value::Array(items.iter().map(strip_meta_schema).collect()),
        Value::Object(o) => {
            let mut out = Map::new();
            for (key, value) in o {
                if META_SCHEMA_KEYWORDS.contains(&key.as_str()) {
                    continue;
                }
                if SCHEMA_MAP_KEYWORDS.contains(&key.as_str())
                    || SCHEMA_VALUE_KEYWORDS.contains(&key.as_str())
                    || SCHEMA_ARRAY_KEYWORDS.contains(&key.as_str())
                {
                    // Map keys are user-defined property names, not keywords.
                    out.insert(key.clone(), strip_meta_schema(value));
                } else {
                    out.insert(key.clone(), value.clone());
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

// --- Legacy custom-tool schema (Claude / GPT-OSS bridge) ----------------------

const CUSTOM_TOOL_SCHEMA_ALLOW: &[&str] = &[
    "type",
    "description",
    "properties",
    "required",
    "items",
    "enum",
];

fn normalize_custom_tool_type(value: &Value) -> Option<Value> {
    match value {
        Value::String(_) => Some(value.clone()),
        Value::Array(entries) => entries
            .iter()
            .find(|e| e.is_string() && e.as_str() != Some("null"))
            .cloned(),
        _ => None,
    }
}

fn normalize_custom_tool_schema(schema: &Value) -> Value {
    match schema {
        Value::Array(items) => {
            Value::Array(items.iter().map(normalize_custom_tool_schema).collect())
        }
        Value::Object(o) => {
            let mut out = Map::new();
            for (key, value) in o {
                if !CUSTOM_TOOL_SCHEMA_ALLOW.contains(&key.as_str()) {
                    continue;
                }
                if key == "type" {
                    if let Some(t) = normalize_custom_tool_type(value) {
                        out.insert("type".into(), t);
                    }
                    continue;
                }
                if key == "properties" {
                    if let Some(props) = value.as_object() {
                        let mut p = Map::new();
                        for (name, sub) in props {
                            p.insert(name.clone(), normalize_custom_tool_schema(sub));
                        }
                        out.insert("properties".into(), Value::Object(p));
                    }
                    continue;
                }
                if key == "enum" {
                    if let Some(arr) = value.as_array() {
                        if arr.iter().all(|e| e.is_string()) {
                            out.insert("enum".into(), value.clone());
                        }
                    }
                    continue;
                }
                out.insert(key.clone(), normalize_custom_tool_schema(value));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Convert one tool's JSON Schema parameters for the wire.
/// Gemini runtimes take full JSON Schema via `parametersJsonSchema`; Claude and
/// GPT-OSS use the Cloud Code Assist custom-tool bridge, which requires the
/// legacy `parameters` field with a Draft-2020-12 subset allowlist.
pub fn convert_tool_parameters(schema: &Value, legacy: bool) -> Result<Value, String> {
    let mut ref_stack = std::collections::HashSet::new();
    let mut object_stack = Vec::new();
    let mut state = DerefState::default();
    let dereferenced = dereference_schema(
        schema,
        schema,
        &mut ref_stack,
        &mut object_stack,
        &mut state,
        "$",
        0,
    );
    if !dereferenced.issues.is_empty() {
        return Err(dereferenced.issues.join(", "));
    }
    let root = ensure_root_object_schema(dereferenced.schema);
    let stripped = strip_meta_schema(&root);
    if legacy {
        Ok(normalize_custom_tool_schema(&stripped))
    } else {
        Ok(stripped)
    }
}

// --- Tools mapping -------------------------------------------------------------

pub struct ConvertedTools {
    pub tools: Vec<Value>,
    pub native_search: bool,
    pub warnings: Vec<String>,
}

/// Map Responses tools to Gemini tools. A pure web_search request maps to the
/// google_search built-in; when function tools are also present the backend
/// rejects the mix, so a synthetic `web_search` function is declared instead
/// (the proxy intercepts and grounds it itself). `image_tool` adds a
/// `generate_image` function declaration (intercepted like web_search).
pub fn responses_tools_to_gemini_tools(
    tools: &Value,
    runtime_model: &str,
    image_tool: bool,
) -> ConvertedTools {
    let mut result: Vec<Value> = Vec::new();
    let mut native_search = false;
    let mut has_functions = false;
    let legacy = runtime_model.starts_with("claude-") || runtime_model.starts_with("gpt-oss-");
    let mut warnings: Vec<String> = Vec::new();

    if let Some(list) = tools.as_array() {
        for t in list {
            let ttype = t.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if ttype.starts_with("web_search") && crate::config::extra_tool_enabled("SEARCH") {
                native_search = true;
            }
            if ttype != "function" {
                continue;
            }
            let Some(name) = t.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            if (name == "google_search" && !crate::config::extra_tool_enabled("SEARCH"))
                || (name == "generate_image" && !crate::config::extra_tool_enabled("IMAGE"))
            {
                continue;
            }
            has_functions = true;
            let mut declaration = Map::new();
            declaration.insert("name".into(), json!(name));
            declaration.insert(
                "description".into(),
                json!(t.get("description").and_then(|v| v.as_str()).unwrap_or("")),
            );
            if let Some(params) = t.get("parameters").filter(|p| p.is_object()) {
                match convert_tool_parameters(params, legacy) {
                    Ok(p) if p.as_object().map(|o| !o.is_empty()).unwrap_or(false) => {
                        let key = if legacy {
                            "parameters"
                        } else {
                            "parametersJsonSchema"
                        };
                        declaration.insert(key.into(), p);
                    }
                    Ok(_) => {}
                    Err(detail) => {
                        warnings.push(format!(
                            "Skipped tool '{name}' due to unresolved schema reference: {detail}"
                        ));
                        continue;
                    }
                }
            }
            result.push(json!({"functionDeclarations": [Value::Object(declaration)]}));
        }
    }
    if image_tool && has_functions && crate::config::extra_tool_enabled("IMAGE") {
        result.push(json!({
            "functionDeclarations": [{
                "name": "generate_image",
                "description": "Generate an image from a text description. Returns the saved file path and a short caption.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "prompt": {"type": "string", "description": "Description of the image to generate"},
                        "aspect_ratio": {"type": "string", "description": "Optional aspect ratio such as 16:9 or 1:1"}
                    },
                    "required": ["prompt"]
                }
            }]
        }));
    }
    if native_search {
        if has_functions {
            result.push(json!({
                "functionDeclarations": [{
                    "name": "web_search",
                    "description": "Search the web for current information. Returns a digest of the top results with sources.",
                    "parameters": {
                        "type": "object",
                        "properties": {"query": {"type": "string", "description": "The search query"}},
                        "required": ["query"]
                    }
                }]
            }));
        } else {
            result.push(json!({"google_search": {}}));
        }
    }
    ConvertedTools {
        tools: result,
        native_search,
        warnings,
    }
}

// --- Thought signatures ---------------------------------------------------------

const BASE64_SIG_RE: &str = r"^[A-Za-z0-9+/]+={0,2}$";

pub fn is_valid_thought_signature(signature: Option<&str>) -> bool {
    let Some(sig) = signature else { return false };
    if sig.is_empty() || sig.len() % 4 != 0 {
        return false;
    }
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(BASE64_SIG_RE).unwrap())
        .is_match(sig)
}

/// Gemini >= 3 requires the model's own thoughtSignature echoed on later turns.
pub fn gemini_requires_thought_signature(runtime_model: &str) -> bool {
    if !runtime_model.starts_with("gemini-") {
        return false;
    }
    match runtime_model
        .strip_prefix("gemini-")
        .and_then(|s| s.split('.').next())
        .and_then(|s| s.split('-').next())
        .and_then(|s| s.parse::<i64>().ok())
    {
        Some(major) => major >= 3,
        None => true,
    }
}

// --- Tool call id sanitization (Claude / GPT-OSS bridges) ------------------------

pub fn sanitize_tool_call_id(id: &str, fallback_name: Option<&str>) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let capped: String = cleaned.chars().take(64).collect();
    if capped.is_empty() {
        format!("{}_1", fallback_name.unwrap_or("tool"))
    } else {
        capped
    }
}

pub fn tool_call_id_needed(requested_model: &str, runtime_model: &str) -> bool {
    requested_model.starts_with("claude-")
        || requested_model.starts_with("gpt-oss-")
        || runtime_model.starts_with("claude-")
        || runtime_model.starts_with("gpt-oss-")
}

// --- Item conversion --------------------------------------------------------------

fn add_turn(contents: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = contents.last_mut() {
        if last.get("role").and_then(|r| r.as_str()) == Some(role) {
            if let Some(arr) = last.get_mut("parts").and_then(|p| p.as_array_mut()) {
                arr.extend(parts);
                return;
            }
        }
    }
    contents.push(json!({"role": role, "parts": parts}));
}

fn image_part_from_block(c: &Value) -> Option<Value> {
    let raw = if let Some(u) = c.get("image_url") {
        match u {
            Value::String(s) => Some(s.clone()),
            Value::Object(_) => u.get("url").and_then(|v| v.as_str()).map(String::from),
            _ => None,
        }
    } else {
        c.pointer("/image/data")
            .and_then(|v| v.as_str())
            .map(String::from)
    }?;
    if raw.is_empty() {
        return None;
    }
    // Remote URLs are resolved+inlined before conversion (lib images); a bare
    // URL that got this far is skipped rather than sent as invalid base64.
    if raw.to_lowercase().starts_with("http://") || raw.to_lowercase().starts_with("https://") {
        return None;
    }
    let mut mime = c
        .pointer("/image_url/mime_type")
        .or_else(|| c.pointer("/image/mime_type"))
        .or_else(|| c.get("mime_type"))
        .and_then(|v| v.as_str())
        .unwrap_or("image/png")
        .to_string();
    let mut data = raw;
    if let Some(rest) = data.strip_prefix("data:") {
        if let Some(comma) = rest.find(',') {
            let prefix = &rest[..comma];
            if !prefix.is_empty() {
                mime = prefix.split(';').next().unwrap_or("image/png").to_string();
            }
            data = rest[comma + 1..].to_string();
        }
    }
    let clean: String = data.chars().filter(|ch| !ch.is_whitespace()).collect();
    Some(json!({"inlineData": {"mimeType": mime, "data": clean}}))
}

fn convert_content_parts(content: &Value, role_model: bool, parts: &mut Vec<Value>) {
    let arr: Vec<Value> = match content {
        Value::Array(a) => a.clone(),
        Value::String(s) => vec![json!({"type": "input_text", "text": s})],
        _ => vec![],
    };
    for c in &arr {
        match c {
            Value::String(s) => {
                let text = s.trim();
                if !text.is_empty() {
                    parts.push(json!({"text": text}));
                }
            }
            Value::Object(o) => {
                let ctype = o.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match ctype {
                    "input_text" | "output_text" => {
                        let text = o.get("text").and_then(|v| v.as_str()).unwrap_or("").trim();
                        if !text.is_empty() {
                            parts.push(json!({"text": text}));
                        }
                    }
                    "input_image" => {
                        if o.get("image_url").is_some() || o.get("image").is_some() {
                            if let Some(p) = image_part_from_block(c) {
                                parts.push(p);
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    let _ = role_model;
}

/// Convert one Responses input item into Gemini parts appended via `add`.
fn response_item_to_gemini_parts(
    item: &Value,
    add: &mut dyn FnMut(&str, Vec<Value>),
    call_names: &mut HashMap<String, String>,
    dropped: &mut HashMap<String, String>,
    ctx: &ItemCtx,
) {
    let Some(obj) = item.as_object() else { return };
    let item_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

    if item_type == "message"
        || (item_type.is_empty() && obj.get("content").map(|c| c.is_array()).unwrap_or(false))
    {
        let role = if obj.get("role").and_then(|r| r.as_str()) == Some("assistant") {
            GEMINI_ROLE_MODEL
        } else {
            GEMINI_ROLE_USER
        };
        let mut parts = Vec::new();
        let content = obj.get("content").cloned().unwrap_or(Value::Null);
        convert_content_parts(&content, role == GEMINI_ROLE_MODEL, &mut parts);
        add(role, parts);
    } else if item_type == "web_search_call" {
        let query = item
            .pointer("/action/query")
            .and_then(|q| q.as_str())
            .unwrap_or("");
        let text = if !query.trim().is_empty() {
            format!("Searched the web for: {query}")
        } else {
            "Searched the web.".to_string()
        };
        add(GEMINI_ROLE_MODEL, vec![json!({"text": text})]);
    } else if item_type == "function_call" {
        let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let call_id = obj.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
        let args_raw = match obj.get("arguments") {
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => "{}".to_string(),
        };
        let args: Value =
            serde_json::from_str(&args_raw).unwrap_or_else(|_| json!({"raw": args_raw}));
        let mut sig = obj.get("thought_signature").and_then(|v| v.as_str());
        if !is_valid_thought_signature(sig) {
            sig = Some(crate::config::PLACEHOLDER_THOUGHT_SIGNATURE);
        }
        let attach_sig = sig.is_some()
            && (gemini_requires_thought_signature(&ctx.runtime_model)
                || is_valid_thought_signature(
                    obj.get("thought_signature").and_then(|v| v.as_str()),
                ));
        let part_id = if ctx.sanitize_ids {
            sanitize_tool_call_id(call_id, Some(name))
        } else {
            call_id.to_string()
        };
        let function_call = json!({"name": name, "args": args, "id": part_id});
        let mut part = json!({"functionCall": function_call});
        if attach_sig {
            if let Some(s) = sig {
                part["thoughtSignature"] = json!(s);
            }
        }
        add(GEMINI_ROLE_MODEL, vec![part]);
        if !call_id.is_empty() && !name.is_empty() {
            call_names.insert(call_id.to_string(), name.to_string());
        }
    } else if item_type == "function_call_output" {
        let call_id = obj.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
        let out_name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| call_names.get(call_id).cloned())
            .unwrap_or_else(|| {
                if call_id.is_empty() {
                    "tool".to_string()
                } else {
                    call_id.to_string()
                }
            });
        let output = match obj.get("output") {
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        };
        let response_id = if ctx.sanitize_ids {
            sanitize_tool_call_id(call_id, Some(&out_name))
        } else if call_id.is_empty() {
            out_name.clone()
        } else {
            call_id.to_string()
        };
        // A dropped (unsigned) call is replayed as a text observation instead of
        // a functionCall part, matching upstream; its tool result then becomes
        // a user text note.
        if ctx.requires_sig {
            if let Some(args_text) = dropped.get(call_id).cloned().or_else(|| {
                if call_id.is_empty() {
                    dropped.get("").cloned()
                } else {
                    None
                }
            }) {
                let label = if args_text == "{}" {
                    format!("`{out_name}`")
                } else {
                    format!("`{out_name}` ({args_text})")
                };
                add(
                    GEMINI_ROLE_USER,
                    vec![json!({"text": format!("[Observation from {label}:\n{output}]")})],
                );
                return;
            }
        }
        add(
            GEMINI_ROLE_USER,
            vec![json!({
                "functionResponse": {
                    "name": out_name,
                    "response": {"output": output},
                    "id": response_id
                }
            })],
        );
    }
}

struct ItemCtx {
    runtime_model: String,
    sanitize_ids: bool,
    requires_sig: bool,
}

// --- Session trajectory (stable across turns) -----------------------------------

fn trajectory_map() -> &'static Mutex<HashMap<String, (String, String)>> {
    static MAP: std::sync::OnceLock<Mutex<HashMap<String, (String, String)>>> =
        std::sync::OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Stable conversationId / trajectoryId for the first user message of a
/// conversation (seeded by its first 64 chars, which stay constant across
/// turns of the same session).
pub fn resolve_session_trajectory(first_user_text: Option<&str>) -> (String, String) {
    let Some(text) = first_user_text.map(str::trim).filter(|t| !t.is_empty()) else {
        let c = format!(
            "conv-{}",
            crate::security::stable_uuid(&format!("r:{}", rand_u64()))
        );
        let t = format!(
            "traj-{}",
            crate::security::stable_uuid(&format!("r:{}", rand_u64()))
        );
        return (c, t);
    };
    let seed = text.chars().take(64).collect::<String>();
    let key = format!("user:{seed}");
    if let Ok(mut map) = trajectory_map().lock() {
        if map.len() > 64 {
            // drop an arbitrary old entry (insertion-order map)
            if let Some(first) = map.keys().next().cloned() {
                map.remove(&first);
            }
        }
        if let Some(hit) = map.get(&key) {
            return hit.clone();
        }
        let entry = (
            stable_uuid(&format!("antigravity:conv:{key}")),
            stable_uuid(&format!("antigravity:traj:{key}")),
        );
        map.insert(key, entry.clone());
        return entry;
    }
    (
        stable_uuid(&format!("antigravity:conv:{key}")),
        stable_uuid(&format!("antigravity:traj:{key}")),
    )
}

pub fn rand_u64() -> u64 {
    use rand::RngCore;
    rand::thread_rng().next_u64()
}

/// Build the wire envelope labels + request/session ids
/// (port of upstream utils/util.ts antigravityRequestEnvelope).
pub fn antigravity_request_envelope(
    runtime_model: &str,
    is_claude: bool,
    is_non_gemini: bool,
    step: usize,
    last_step_index: String,
    request_index: usize,
    conversation_id: Option<String>,
    trajectory_id: Option<String>,
) -> (String, String, Map<String, Value>) {
    let step = step.max(1);
    let agent_id = conversation_id
        .unwrap_or_else(|| crate::security::stable_uuid(&format!("a:{}", rand_u64())));
    let trajectory_id =
        trajectory_id.unwrap_or_else(|| crate::security::stable_uuid(&format!("t:{}", rand_u64())));
    let session_id = rand_u64().to_string();

    let claude_label = if is_claude { "true" } else { "false" };
    let non_gemini_label = if is_non_gemini || is_claude {
        "true"
    } else {
        "false"
    };

    let mut labels = Map::new();
    labels.insert("last_step_index".into(), json!(last_step_index));
    labels.insert(
        "request_id".into(),
        json!(format!("{trajectory_id}-{request_index}")),
    );
    labels.insert("trajectory_id".into(), json!(trajectory_id));
    labels.insert("used_claude".into(), json!(claude_label));
    labels.insert("used_claude_conservative".into(), json!(claude_label));
    labels.insert("used_non_gemini_model".into(), json!(non_gemini_label));
    if let Some(model_enum) = catalog::get_model_enum(runtime_model) {
        labels.insert("model_enum".into(), json!(model_enum));
    }
    if step > 1 {
        labels.insert(
            "last_execution_id".into(),
            json!(stable_uuid(&format!(
                "antigravity:exec:{trajectory_id}:{}",
                step - 1
            ))),
        );
    }

    let request_id = format!(
        "agent/{agent_id}/{}{}/{trajectory_id}/{step}",
        chrono::Utc::now().timestamp_millis(),
        ""
    );
    (request_id, session_id, labels)
}

fn first_user_text(body: &Value) -> Option<String> {
    if let Some(s) = body.get("input").and_then(|v| v.as_str()) {
        return Some(s.chars().take(64).collect());
    }
    let items = body.get("input")?.as_array()?;
    for item in items {
        let role = item.get("role").and_then(|r| r.as_str());
        if role != Some("user") {
            continue;
        }
        if let Some(text) = item.get("content") {
            if let Some(s) = text.as_str() {
                return Some(s.chars().take(64).collect());
            }
            if let Some(arr) = text.as_array() {
                for part in arr {
                    if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                        if !t.trim().is_empty() {
                            return Some(t.chars().take(64).collect());
                        }
                    }
                }
            }
        }
    }
    None
}

// --- Main conversion ---------------------------------------------------------------

pub struct ConvertOptions {
    /// requested (public or runtime) model id from the client
    pub requested_model: String,
    pub effort: Option<Effort>,
    pub prompt: String,
    pub image_tool: bool,
    /// pre-injected generate_image function_call/items interception happens in
    /// the stream layer; this only declares the tool.
    pub inject_image_tool: bool,
}

/// Convert a Responses request body into the serialized Antigravity request.
/// Err = unrecoverable boundary violation (missing tool results).
pub fn responses_to_gemini_body(
    body: &Value,
    project: &str,
    runtime_model: &str,
    opts: &ConvertOptions,
) -> Result<String, String> {
    let requested = opts.requested_model.as_str();
    let sanitize_ids = tool_call_id_needed(requested, runtime_model);
    let requires_sig = gemini_requires_thought_signature(runtime_model);

    // System instruction: top-level instructions + system.
    let mut system_texts: Vec<String> = Vec::new();
    for key in ["instructions", "system"] {
        if let Some(s) = body.get(key).and_then(|v| v.as_str()) {
            if !s.trim().is_empty() {
                system_texts.push(s.to_string());
            }
        }
    }
    let instructions = if system_texts.is_empty() {
        "You are a helpful coding assistant.".to_string()
    } else {
        system_texts.join("\n\n")
    };

    let mut contents: Vec<Value> = Vec::new();
    let mut call_names: HashMap<String, String> = HashMap::new();
    let mut dropped: HashMap<String, String> = HashMap::new();
    let ctx = ItemCtx {
        runtime_model: runtime_model.to_string(),
        sanitize_ids,
        requires_sig,
    };
    let mut add = |role: &str, parts: Vec<Value>| add_turn(&mut contents, role, parts);
    match body.get("input") {
        Some(Value::Array(items)) => {
            for item in items {
                response_item_to_gemini_parts(item, &mut add, &mut call_names, &mut dropped, &ctx);
            }
        }
        Some(Value::String(s)) => {
            add(GEMINI_ROLE_USER, vec![json!({"text": s})]);
        }
        Some(other)
            if other
                .get("content")
                .map(|c| c.is_array() || c.is_string())
                .unwrap_or(false) =>
        {
            response_item_to_gemini_parts(other, &mut add, &mut call_names, &mut dropped, &ctx);
        }
        _ => {}
    }
    drop(add);

    // Boundary fix #1 (upstream #48): a function-call model turn is only valid
    // immediately after a user turn; compacted history can lose that boundary.
    {
        let mut insertions: Vec<usize> = Vec::new();
        for (i, turn) in contents.iter().enumerate() {
            let is_model_fn_call = turn.get("role").and_then(|r| r.as_str())
                == Some(GEMINI_ROLE_MODEL)
                && turn
                    .get("parts")
                    .and_then(|p| p.as_array())
                    .map(|parts| parts.iter().any(|p| p.get("functionCall").is_some()))
                    .unwrap_or(false);
            if is_model_fn_call {
                let prev_user = if i == 0 {
                    false
                } else {
                    contents[i - 1].get("role").and_then(|r| r.as_str()) == Some(GEMINI_ROLE_USER)
                };
                if !prev_user {
                    insertions.push(i);
                }
            }
        }
        for (offset, i) in insertions.into_iter().enumerate() {
            contents.insert(
                i + offset,
                json!({"role": GEMINI_ROLE_USER, "parts": [{"text": CONTINUATION_TEXT}]}),
            );
        }
    }

    // Boundary fix #2: Antigravity requires a natural-language user part.
    let has_user_text = contents.iter().any(|turn| {
        turn.get("role").and_then(|r| r.as_str()) == Some(GEMINI_ROLE_USER)
            && turn
                .get("parts")
                .and_then(|p| p.as_array())
                .map(|parts| {
                    parts.iter().any(|p| {
                        p.get("text")
                            .and_then(|t| t.as_str())
                            .map(|t| !t.trim().is_empty())
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
    });
    if !has_user_text && !contents.is_empty() {
        let bridge = json!({"text": CONTINUATION_TEXT});
        match contents
            .iter_mut()
            .find(|t| t.get("role").and_then(|r| r.as_str()) == Some(GEMINI_ROLE_USER))
        {
            Some(user_turn) => {
                if let Some(parts) = user_turn.get_mut("parts").and_then(|p| p.as_array_mut()) {
                    parts.push(bridge);
                }
            }
            None => contents.insert(0, json!({"role": GEMINI_ROLE_USER, "parts": [bridge]})),
        }
    }

    // Boundary fix #3: trailing model turn. With a trailing functionCall the
    // tool result is missing (upstream throws); otherwise append a user turn.
    if let Some(last) = contents.last() {
        if last.get("role").and_then(|r| r.as_str()) == Some(GEMINI_ROLE_MODEL) {
            let has_fn_call = last
                .get("parts")
                .and_then(|p| p.as_array())
                .map(|parts| parts.iter().any(|p| p.get("functionCall").is_some()))
                .unwrap_or(false);
            if has_fn_call {
                return Err(
                    "Antigravity request is missing tool result(s) for the final assistant tool call. Provide the corresponding tool result before continuing."
                        .to_string(),
                );
            }
            add_turn(
                &mut contents,
                GEMINI_ROLE_USER,
                vec![json!({"text": CONTINUATION_TEXT})],
            );
        }
    }

    // Thinking config + max output tokens (upstream per-model tables).
    let effort = opts
        .effort
        .or_else(|| catalog::suffix_effort(runtime_model));
    let mut generation_config = Map::new();
    generation_config.insert(
        "maxOutputTokens".into(),
        json!(catalog::get_max_output_tokens(requested, runtime_model)),
    );
    if let Some(temp) = body.get("temperature").and_then(|v| v.as_f64()) {
        generation_config.insert("temperature".into(), json!(temp));
    }
    if let Some((include_thoughts, budget)) = catalog::get_thinking_config(runtime_model, effort) {
        generation_config.insert(
            "thinkingConfig".into(),
            json!({"includeThoughts": include_thoughts, "thinkingBudget": budget}),
        );
    }

    let converted = responses_tools_to_gemini_tools(
        &body.get("tools").cloned().unwrap_or(Value::Null),
        runtime_model,
        opts.inject_image_tool,
    );

    // Envelope: step = contents length, request_index = completed assistant turns.
    let step = contents.len().max(1);
    let last_step_index = (contents.len().saturating_sub(1)).to_string();
    let request_index = body
        .get("input")
        .and_then(|i| i.as_array())
        .map(|items| {
            items
                .iter()
                .filter(|i| {
                    i.get("type").and_then(|t| t.as_str()) == Some("message")
                        && i.get("role").and_then(|r| r.as_str()) == Some("assistant")
                })
                .count()
        })
        .unwrap_or(0);
    let (conversation_id, trajectory_id) = match body
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(seed) => (
            stable_uuid(&format!("antigravity:conv:session:{seed}")),
            stable_uuid(&format!("antigravity:traj:session:{seed}")),
        ),
        None => resolve_session_trajectory(first_user_text(body).as_deref()),
    };
    let (request_id, session_id, mut labels) = antigravity_request_envelope(
        runtime_model,
        requested.starts_with("claude-") || runtime_model.starts_with("claude-"),
        requested.starts_with("claude-")
            || requested.starts_with("gpt-oss-")
            || runtime_model.starts_with("claude-")
            || runtime_model.starts_with("gpt-oss-")
            || (!requested.starts_with("gemini-") && !runtime_model.starts_with("gemini-")),
        step,
        last_step_index,
        request_index,
        Some(conversation_id),
        Some(trajectory_id),
    );
    if step > 1 {
        if let Some(last) = body.get("last_execution_id").and_then(Value::as_str) {
            labels.insert("last_execution_id".into(), json!(last));
        }
    }

    let mut request = Map::new();
    request.insert("contents".into(), Value::Array(contents));
    request.insert(
        "systemInstruction".into(),
        json!({"role": GEMINI_ROLE_USER, "parts": [{"text": instructions}]}),
    );
    let has_google_search = converted
        .tools
        .iter()
        .any(|t| t.get("google_search").is_some());
    if !converted.tools.is_empty() {
        request.insert("tools".into(), Value::Array(converted.tools));
    }
    if has_google_search {
        // Mixing the google_search built-in with server-side invocations needs
        // this opt-in flag, or Antigravity 400s (pure-search requests only).
        request.insert(
            "toolConfig".into(),
            json!({"includeServerSideToolInvocations": true}),
        );
    }
    request.insert("generationConfig".into(), Value::Object(generation_config));
    request.insert("sessionId".into(), json!(session_id));
    request.insert("labels".into(), Value::Object(labels));

    let envelope = json!({
        "project": project,
        "model": runtime_model,
        "request": Value::Object(request),
        "requestType": "agent",
        "userAgent": "antigravity",
        "requestId": request_id
    });
    Ok(envelope.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::stable_uuid;

    fn body_of(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn instructions_merge_and_string_input() {
        let body = body_of(r#"{"instructions":"Be terse.","system":"Legacy.","input":"Reply OK"}"#);
        let opts = ConvertOptions {
            requested_model: "gemini-3.8-flash".into(),
            effort: Some(Effort::Medium),
            prompt: String::new(),
            image_tool: false,
            inject_image_tool: false,
        };
        let out = responses_to_gemini_body(&body, "p", "gemini-3.8-flash-medium", &opts).unwrap();
        let v: Value = body_of(&out);
        assert_eq!(
            v["request"]["systemInstruction"]["parts"][0]["text"],
            "Be terse.\n\nLegacy."
        );
        assert_eq!(v["request"]["contents"][0]["role"], "user");
        assert_eq!(v["request"]["contents"][0]["parts"][0]["text"], "Reply OK");
    }

    #[test]
    fn envelope_has_fingerprint_labels() {
        let body = body_of(r#"{"input":"hi"}"#);
        let opts = ConvertOptions {
            requested_model: "gemini-3.8-flash".into(),
            effort: None,
            prompt: String::new(),
            image_tool: false,
            inject_image_tool: false,
        };
        let out = responses_to_gemini_body(&body, "p", "gemini-3.8-flash-medium", &opts).unwrap();
        let v: Value = body_of(&out);
        let labels = &v["request"]["labels"];
        assert_eq!(labels["used_claude"], "false");
        assert_eq!(labels["model_enum"], "MODEL_PLACEHOLDER_M319");
        assert!(labels["request_id"].as_str().unwrap().contains('-'));
        assert!(v["requestId"].as_str().unwrap().starts_with("agent/"));
        assert_eq!(
            v["request"]["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            4000
        );
    }

    #[test]
    fn parallel_tool_calls_and_outputs() {
        let body = body_of(
            r#"{"input":[
            {"type":"message","role":"user","content":[{"type":"input_text","text":"weather?"}]},
            {"type":"function_call","call_id":"call_1","name":"get_weather","arguments":"{\"city\":\"Beijing\"}","thought_signature":"c2lnMQ=="},
            {"type":"function_call","call_id":"call_2","name":"get_time","arguments":"{}"},
            {"type":"function_call_output","call_id":"call_1","output":"{\"temp\":23}"},
            {"type":"function_call_output","call_id":"call_2","output":"12:00"}
        ]}"#,
        );
        let opts = ConvertOptions {
            requested_model: "gemini-3.8-flash".into(),
            effort: None,
            prompt: String::new(),
            image_tool: false,
            inject_image_tool: false,
        };
        let out = responses_to_gemini_body(&body, "p", "gemini-3.8-flash-medium", &opts).unwrap();
        let v: Value = body_of(&out);
        let contents = v["request"]["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 3, "user, model, user");
        assert_eq!(contents[1]["role"], "model");
        let calls = contents[1]["parts"].as_array().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["functionCall"]["name"], "get_weather");
        assert_eq!(calls[0]["thoughtSignature"], "c2lnMQ==");
        assert!(
            calls[1].get("thoughtSignature").is_none()
                || calls[1]["thoughtSignature"]
                    == json!(crate::config::PLACEHOLDER_THOUGHT_SIGNATURE),
            "missing sig backfilled"
        );
        let responses_ = contents[2]["parts"].as_array().unwrap();
        assert_eq!(
            responses_[0]["functionResponse"]["name"], "get_weather",
            "name backfilled from callNames"
        );
        assert_eq!(responses_[0]["functionResponse"]["id"], "call_1");
    }

    #[test]
    fn boundary_injects_user_turn_between_model_fn_calls() {
        let body = body_of(
            r#"{"input":[
            {"type":"message","role":"user","content":[{"type":"input_text","text":"go"}]},
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"working"}]},
            {"type":"function_call","call_id":"c1","name":"f","arguments":"{}"},
            {"type":"function_call_output","call_id":"c1","output":"ok"},
            {"type":"function_call","call_id":"c2","name":"g","arguments":"{}"},
            {"type":"function_call_output","call_id":"c2","output":"ok2"}
        ]}"#,
        );
        let opts = ConvertOptions {
            requested_model: "gemini-3.8-flash".into(),
            effort: None,
            prompt: String::new(),
            image_tool: false,
            inject_image_tool: false,
        };
        let out = responses_to_gemini_body(&body, "p", "gemini-3.8-flash-medium", &opts).unwrap();
        let v: Value = body_of(&out);
        let contents = v["request"]["contents"].as_array().unwrap();
        for (i, turn) in contents.iter().enumerate() {
            let model_fn = turn.get("role").and_then(|r| r.as_str()) == Some("model")
                && turn["parts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| p.get("functionCall").is_some());
            if model_fn && i > 0 {
                assert_eq!(
                    contents[i - 1]["role"],
                    "user",
                    "fn-call turn must follow a user turn"
                );
            }
        }
    }

    #[test]
    fn trailing_model_text_turn_gets_continuation() {
        let body = body_of(
            r#"{"input":[
            {"type":"message","role":"user","content":[{"type":"input_text","text":"go"}]},
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}
        ]}"#,
        );
        let opts = ConvertOptions {
            requested_model: "gemini-3.8-flash".into(),
            effort: None,
            prompt: String::new(),
            image_tool: false,
            inject_image_tool: false,
        };
        let out = responses_to_gemini_body(&body, "p", "gemini-3.8-flash-medium", &opts).unwrap();
        let v: Value = body_of(&out);
        let contents = v["request"]["contents"].as_array().unwrap();
        let last = contents.last().unwrap();
        assert_eq!(
            last["role"], "user",
            "trailing model turn gets a continuation user turn"
        );
    }

    #[test]
    fn missing_tool_result_is_error() {
        let body = body_of(
            r#"{"input":[
            {"type":"message","role":"user","content":[{"type":"input_text","text":"go"}]},
            {"type":"function_call","call_id":"c1","name":"f","arguments":"{}"}
        ]}"#,
        );
        let opts = ConvertOptions {
            requested_model: "gemini-3.8-flash".into(),
            effort: None,
            prompt: String::new(),
            image_tool: false,
            inject_image_tool: false,
        };
        assert!(responses_to_gemini_body(&body, "p", "gemini-3.8-flash-medium", &opts).is_err());
    }

    #[test]
    fn schema_dereference_and_legacy_params() {
        let schema = body_of(
            r##"{"type":"object","$defs":{"name":{"type":"string"}},"properties":{"who":{"$ref":"#/$defs/name"},"n":{"anyOf":[{"type":"string"},{"type":"null"}]},"const_field":{"const":"fixed","type":"string"}}}"##,
        );
        // Gemini path keeps richer JSON Schema.
        let gem = convert_tool_parameters(&schema, false).unwrap();
        assert!(gem.is_object());
        assert!(
            gem["properties"]["who"]["type"] == json!("string"),
            "deref resolved"
        );
        assert!(gem.get("$defs").is_none(), "defs stripped");
        assert!(
            gem["properties"]["n"]["anyOf"].is_array(),
            "gemini keeps anyOf"
        );
        // Claude/GPT path: legacy allowlist; union type collapses; const dropped.
        let legacy = convert_tool_parameters(&schema, true).unwrap();
        assert_eq!(legacy["properties"]["who"]["type"], json!("string"));
        assert!(legacy["properties"]["n"].get("anyOf").is_none());
        assert!(legacy["properties"]["const_field"].get("const").is_none());
    }

    #[test]
    fn stable_trajectory_survives_repeats() {
        let a = resolve_session_trajectory(Some("hello world"));
        let b = resolve_session_trajectory(Some("hello world"));
        assert_eq!(a, b);
        assert_ne!(a.0, a.1);
        assert_eq!(a.0, stable_uuid("antigravity:conv:user:hello world"));
    }

    #[test]
    fn web_search_synthetic_declaration() {
        let tools = json!([
            {"type": "web_search"},
            {"type": "function", "name": "read_file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}
        ]);
        let converted = responses_tools_to_gemini_tools(&tools, "gemini-3.8-flash-medium", false);
        assert!(!converted.native_search_used_pure());
        assert!(converted
            .tools
            .iter()
            .any(|t| { t["functionDeclarations"][0]["name"] == json!("web_search") }));
    }

    impl ConvertedTools {
        fn native_search_used_pure(&self) -> bool {
            self.tools.iter().any(|t| t.get("google_search").is_some())
        }
    }
}
#[test]
fn session_seed_and_previous_execution_survive_multiturn() {
    let opts = ConvertOptions {
        requested_model: "gemini-3-flash".into(),
        effort: None,
        prompt: String::new(),
        image_tool: false,
        inject_image_tool: false,
    };
    let single = serde_json::json!({"input":"q","session_id":"isolated-session"});
    let multi = serde_json::json!({"session_id":"isolated-session","last_execution_id":"previous-execution","input":[{"role":"user","content":"different"},{"role":"assistant","type":"message","content":"answer"},{"role":"user","content":"followup"}]});
    let first: Value = serde_json::from_str(
        &responses_to_gemini_body(&single, "p", "gemini-3-flash", &opts).unwrap(),
    )
    .unwrap();
    let next: Value = serde_json::from_str(
        &responses_to_gemini_body(&multi, "p", "gemini-3-flash", &opts).unwrap(),
    )
    .unwrap();
    assert!(first["request"]["labels"]
        .get("last_execution_id")
        .is_none());
    assert_eq!(
        first["request"]["labels"]["trajectory_id"],
        next["request"]["labels"]["trajectory_id"]
    );
    assert_eq!(
        next["request"]["labels"]["last_execution_id"],
        "previous-execution"
    );
}
