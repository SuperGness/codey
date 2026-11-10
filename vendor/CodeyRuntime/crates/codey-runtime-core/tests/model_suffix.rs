use codey_runtime_core::model_suffix::{
    bundled_model_catalog, remove_model_prompt_fields, sanitize_generic_model_metadata,
};
use serde_json::{Value, json};

#[test]
fn bundled_catalog_contains_compatibility_metadata_without_prompt_assets() {
    let catalog = bundled_model_catalog().expect("bundled model metadata");
    assert_prompt_fields_absent(&catalog);
    assert!(
        catalog["models"]
            .as_array()
            .is_some_and(|models| !models.is_empty())
    );
}

#[test]
fn remove_model_prompt_fields_strips_prompt_fields_recursively() {
    let mut template = json!({
        "slug": "template",
        "base_instructions": "private base prompt",
        "model_messages": {
            "instructions_template": "private instructions template",
            "personality_default": "private personality"
        },
        "compatibility": {
            "instructions_template": "private nested template",
            "personality_default": "private nested personality",
            "instructions_variables": {
                "private": "nested prompt variable"
            }
        }
    });

    remove_model_prompt_fields(&mut template);
    assert_prompt_fields_absent(&template);
    let serialized = serde_json::to_string(&template).unwrap();
    assert!(!serialized.contains("private"));
}

#[test]
fn sanitize_generic_model_metadata_strips_model_specific_runtime_metadata() {
    let mut model = json!({
        "slug": "gpt-template",
        "use_responses_lite": true,
        "tool_mode": "code_mode_only",
        "multi_agent_version": "v2",
        "multi_agent_reasoning_effort": "xhigh",
        "comp_hash": "3000",
        "default_service_tier": "priority",
        "prefer_websockets": true,
        "reasoning_summary_format": "experimental",
        "auto_review_model_override": "review-template",
        "node_repl_auto_review_required": true,
        "node_repl_disabled": true,
        "include_skills_usage_instructions": false,
        "include_plugin_usage_instructions": false,
        "include_apps_usage_instructions": false,
        "experimental_supported_tools": ["template-only-tool"],
        "auto_compact_token_limit": 12345
    });

    sanitize_generic_model_metadata(&mut model);

    assert_eq!(model["use_responses_lite"], false);
    for field in [
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
    ] {
        assert!(
            model.get(field).is_none(),
            "found model-specific field {field}"
        );
    }
    assert_eq!(model["include_skills_usage_instructions"], true);
    assert_eq!(model["include_plugin_usage_instructions"], true);
    assert_eq!(model["include_apps_usage_instructions"], true);
    assert_eq!(model["experimental_supported_tools"], json!([]));
    assert!(model["auto_compact_token_limit"].is_null());
}

fn assert_prompt_fields_absent(value: &Value) {
    match value {
        Value::Object(object) => {
            for field in [
                "base_instructions",
                "instructions_template",
                "model_messages",
                "instructions_variables",
                "personality_default",
                "personality_friendly",
                "personality_pragmatic",
            ] {
                assert!(!object.contains_key(field), "found prompt field {field}");
            }
            for child in object.values() {
                assert_prompt_fields_absent(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                assert_prompt_fields_absent(child);
            }
        }
        _ => {}
    }
}
