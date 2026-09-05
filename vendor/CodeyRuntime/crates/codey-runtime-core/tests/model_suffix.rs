use std::collections::HashMap;

use codey_runtime_core::model_suffix::{
    build_model_catalog_json, build_model_catalog_json_with_template, bundled_model_catalog,
    collect_catalog_entries, parse_model_suffix,
};
use serde_json::{Value, json};

#[test]
fn parse_suffix_extracts_k_and_m_units() {
    assert_eq!(
        parse_model_suffix("deepseek-v4-pro[1M]"),
        ("deepseek-v4-pro".to_string(), Some(1_000_000))
    );
    assert_eq!(
        parse_model_suffix("claude-sonnet-4[200K]"),
        ("claude-sonnet-4".to_string(), Some(200_000))
    );
    assert_eq!(
        parse_model_suffix("gpt-5.5[512k]"),
        ("gpt-5.5".to_string(), Some(512_000))
    );
    assert_eq!(
        parse_model_suffix("gpt-5.5[1000000]"),
        ("gpt-5.5".to_string(), Some(1_000_000))
    );
}

#[test]
fn parse_suffix_returns_none_without_bracket() {
    assert_eq!(parse_model_suffix("gpt-5.5"), ("gpt-5.5".to_string(), None));
    assert_eq!(
        parse_model_suffix("  qwen3-coder  "),
        ("qwen3-coder".to_string(), None)
    );
}

#[test]
fn parse_suffix_keeps_original_slug_when_bracket_invalid() {
    // 括号内非合法窗口 token 时，整串（含括号）作为 slug，window=None
    let (slug, window) = parse_model_suffix("foo[bar]");
    assert_eq!(slug, "foo[bar]");
    assert_eq!(window, None);

    // 括号未闭合：不剥离
    let (slug2, window2) = parse_model_suffix("foo[1M");
    assert_eq!(slug2, "foo[1M");
    assert_eq!(window2, None);
}

#[test]
fn parse_suffix_rejects_zero_and_negative() {
    assert_eq!(parse_model_suffix("foo[0K]"), ("foo[0K]".to_string(), None));
}

#[test]
fn collect_entries_includes_current_model_and_strips_suffix() {
    let mut windows = HashMap::new();
    windows.insert("deepseek-v4-pro".to_string(), "1M".to_string());
    let entries =
        collect_catalog_entries("deepseek-v4-pro\nqwen3-coder", &windows, "deepseek-v4-pro");
    // 当前 model 与列表去重后共 2 条
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].slug, "deepseek-v4-pro");
    assert_eq!(entries[0].suffix_window, Some(1_000_000));
    assert_eq!(entries[1].slug, "qwen3-coder");
    assert_eq!(entries[1].suffix_window, None);
}

#[test]
fn collect_entries_deduplicates() {
    let entries =
        collect_catalog_entries("qwen3-coder\nqwen3-coder", &HashMap::new(), "qwen3-coder");
    assert_eq!(entries.len(), 1);
}

#[test]
fn build_catalog_json_writes_context_window_and_strips_suffix() {
    let mut windows = HashMap::new();
    windows.insert("deepseek-v4-pro".to_string(), "1M".to_string());
    windows.insert("claude-sonnet-4".to_string(), "200K".to_string());
    let entries = collect_catalog_entries("deepseek-v4-pro\nclaude-sonnet-4", &windows, "");
    let catalog = build_model_catalog_json(&entries, None);
    assert!(catalog.contains(r#""slug": "deepseek-v4-pro""#));
    assert!(catalog.contains(r#""context_window": 1000000"#));
    assert!(catalog.contains(r#""max_context_window": 1000000"#));
    assert!(catalog.contains(r#""slug": "claude-sonnet-4""#));
    assert!(catalog.contains(r#""context_window": 200000"#));
    // 后缀不得进入 catalog
    assert!(!catalog.contains("[1M]"));
    assert!(!catalog.contains("[200K]"));
    // auto_compact 留 null（codex 按比例算）
    assert!(catalog.contains(r#""auto_compact_token_limit": null"#));
}

#[test]
fn build_catalog_json_uses_fallback_for_no_suffix_entries() {
    let entries = collect_catalog_entries("qwen3-coder", &HashMap::new(), "");
    let catalog = build_model_catalog_json(&entries, Some(272_000));
    assert!(catalog.contains(r#""slug": "qwen3-coder""#));
    assert!(catalog.contains(r#""context_window": 272000"#));
}

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
fn build_catalog_strips_prompt_fields_from_an_external_template() {
    let entries = collect_catalog_entries("third-party-model", &HashMap::new(), "");
    let template = json!({
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

    let catalog = build_model_catalog_json_with_template(&entries, None, Some(&template));
    let catalog: Value = serde_json::from_str(&catalog).unwrap();
    assert_prompt_fields_absent(&catalog);
    let serialized = serde_json::to_string(&catalog).unwrap();
    assert!(!serialized.contains("private"));
}

#[test]
fn build_catalog_sanitizes_model_specific_runtime_metadata_from_external_template() {
    let entries = collect_catalog_entries("third-party-model", &HashMap::new(), "");
    let template = json!({
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

    let catalog = build_model_catalog_json_with_template(&entries, None, Some(&template));
    let catalog: Value = serde_json::from_str(&catalog).unwrap();
    let model = &catalog["models"][0];

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

#[test]
fn collect_entries_adopts_suffix_for_current_model_from_list() {
    // 当前 model 本身无后缀，但 model_list 中靠后位置有同名带后缀条目。
    let mut windows = HashMap::new();
    windows.insert("deepseek-v4-pro".to_string(), "1M".to_string());
    let entries =
        collect_catalog_entries("qwen3-coder\ndeepseek-v4-pro", &windows, "deepseek-v4-pro");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].slug, "deepseek-v4-pro");
    assert_eq!(entries[0].suffix_window, Some(1_000_000));
}

#[test]
fn collect_entries_prefers_later_suffix_for_duplicate_slug() {
    // 同一 slug 先出现无后缀条目，后出现带后缀条目，应采纳后者窗口。
    let mut windows = HashMap::new();
    windows.insert("deepseek/deepseek-v4-flash".to_string(), "1M".to_string());
    let entries = collect_catalog_entries(
        "deepseek/deepseek-v4-flash\ndeepseek/deepseek-v4-flash",
        &windows,
        "",
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].slug, "deepseek/deepseek-v4-flash");
    assert_eq!(entries[0].suffix_window, Some(1_000_000));
}

#[test]
fn collect_entries_prefers_later_suffix_when_reversed() {
    // 同一 slug 先出现 [1M]，后出现 [200K]，后者应覆盖前者。
    let mut windows = HashMap::new();
    windows.insert("deepseek/deepseek-v4-flash".to_string(), "200K".to_string());
    let entries = collect_catalog_entries(
        "deepseek/deepseek-v4-flash\ndeepseek/deepseek-v4-flash",
        &windows,
        "",
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].slug, "deepseek/deepseek-v4-flash");
    assert_eq!(entries[0].suffix_window, Some(200_000));
}

#[test]
fn migrate_model_list_with_suffixes_splits_slug_and_window() {
    let input = "deepseek-v4-flash[1M]\ndeepseek-v4-pro\nnvidia/...:free[200K]";
    let (clean_list, windows) =
        codey_runtime_core::model_suffix::migrate_model_list_with_suffixes(input);
    assert_eq!(
        clean_list,
        "deepseek-v4-flash\ndeepseek-v4-pro\nnvidia/...:free"
    );
    assert_eq!(
        windows.get("deepseek-v4-flash"),
        Some(&"1000000".to_string())
    );
    assert_eq!(windows.get("deepseek-v4-pro"), None);
    assert_eq!(windows.get("nvidia/...:free"), Some(&"200000".to_string()));
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
