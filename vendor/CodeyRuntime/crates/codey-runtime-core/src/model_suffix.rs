//! 通用模型字段与提示词字段清洗。

use serde_json::{Value, json};

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

/// Recursively removes Codex prompt-bearing fields from model catalog values.
///
/// Live catalog entries can contain prompt fields. Callers must sanitize cloned
/// entries before writing a Codey-owned catalog.
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
