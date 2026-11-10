//! 内置 Codex 模型兼容元数据加载与通用模型字段清洗。

use serde_json::{Value, json};

/// 内置 Codex 模型兼容元数据，不包含 system/developer prompt。
///
/// 这里只保留 Codex 识别模型条目所需的能力字段，避免把上游私有提示资产
/// 编入 CodeyRuntime 二进制。
const BUNDLED_TEMPLATE_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/model-catalog-metadata.json"
));

const MODEL_PROMPT_FIELDS: [&str; 7] = [
    "base_instructions",
    "instructions_template",
    "model_messages",
    "instructions_variables",
    "personality_default",
    "personality_friendly",
    "personality_pragmatic",
];

const GENERIC_MODEL_SPECIFIC_FIELDS: [&str; 12] = [
    "tool_mode",
    "multi_agent_version",
    "multi_agent_reasoning_effort",
    "comp_hash",
    "default_service_tier",
    "prefer_websockets",
    "reasoning_summary_format",
    "auto_review_model_override",
    "node_repl_auto_review_required",
    "node_repl_disabled",
    "supports_search_tool",
    "web_search_tool_type",
];

/// Loads prompt-free model compatibility metadata bundled with CodeyRuntime.
///
/// Callers should prefer a live Codex cache and use this only as a cold-start
/// fallback when an API-only installation has not created models_cache.json.
pub fn bundled_model_catalog() -> Option<Value> {
    let mut catalog = serde_json::from_str(BUNDLED_TEMPLATE_JSON).ok()?;
    remove_model_prompt_fields(&mut catalog);
    Some(catalog)
}

/// Recursively removes Codex prompt-bearing fields from model catalog values.
///
/// Live `models_cache.json` entries can contain the same fields as the bundled
/// upstream catalog. Callers must sanitize cloned entries before writing a
/// Codey-owned catalog.
pub fn remove_model_prompt_fields(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for field in MODEL_PROMPT_FIELDS {
                object.remove(field);
            }
            for child in object.values_mut() {
                remove_model_prompt_fields(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                remove_model_prompt_fields(child);
            }
        }
        _ => {}
    }
}

/// Removes runtime and transport capabilities that belong to one concrete
/// Codex model and therefore cannot safely be inferred for a generic provider
/// model cloned from that model's catalog entry.
pub fn sanitize_generic_model_metadata(value: &mut Value) {
    let Some(model) = value.as_object_mut() else {
        return;
    };
    for field in GENERIC_MODEL_SPECIFIC_FIELDS {
        model.remove(field);
    }

    // Unknown provider models use the conservative full Responses transport.
    // Usage instructions stay enabled because the generic model has no native
    // model-specific tool orchestration contract to replace them.
    model.insert("use_responses_lite".to_string(), json!(false));
    model.insert("include_skills_usage_instructions".to_string(), json!(true));
    model.insert("include_plugin_usage_instructions".to_string(), json!(true));
    model.insert("include_apps_usage_instructions".to_string(), json!(true));
    model.insert("experimental_supported_tools".to_string(), json!([]));
    model.insert("auto_compact_token_limit".to_string(), Value::Null);
}
