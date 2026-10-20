use super::*;

fn official_cache() -> Value {
    let mut cache = json!({
        "client_version": "test-client",
        "models": [
            {
                "slug": "gpt-5.6-sol",
                "display_name": "GPT-5.6-Sol",
                "visibility": "list",
                "priority": 1,
                "default_reasoning_level": "low",
                "supported_reasoning_levels": [
                    {"effort": "low"}, {"effort": "medium"}, {"effort": "high"},
                    {"effort": "xhigh"}, {"effort": "max"}, {"effort": "ultra"}
                ],
                "use_responses_lite": true,
                "tool_mode": "code_mode_only",
                "comp_hash": "3000",
                "default_service_tier": "priority",
                "prefer_websockets": true,
                "include_skills_usage_instructions": false,
                "include_plugin_usage_instructions": true,
                "include_apps_usage_instructions": true,
                "experimental_supported_tools": ["gpt-5.6-only-tool"],
                "node_repl_auto_review_required": false,
                "node_repl_disabled": false,
                "service_tiers": [{"id": "priority"}],
                "additional_speed_tiers": ["fast"]
            },
            {
                "slug": "gpt-5.5",
                "display_name": "GPT-5.5",
                "visibility": "list",
                "priority": 7,
                "default_reasoning_level": "medium",
                "supported_reasoning_levels": [{"effort": "low"}, {"effort": "xhigh"}],
                "use_responses_lite": false,
                "comp_hash": "2911",
                "include_skills_usage_instructions": true,
                "include_plugin_usage_instructions": true,
                "include_apps_usage_instructions": true,
                "experimental_supported_tools": [],
                "node_repl_auto_review_required": false,
                "node_repl_disabled": false,
                "additional_speed_tiers": ["fast"],
                "upgrade": {"model": "gpt-5.6-sol"}
            },
            // An older cache can still carry a model the official list has
            // retired; the generated catalog must not expose it again.
            {
                "slug": "gpt-5.4",
                "display_name": "GPT-5.4",
                "visibility": "hide",
                "priority": 16,
                "default_reasoning_level": "medium",
                "supported_reasoning_levels": [
                    {"effort": "low"}, {"effort": "medium"}, {"effort": "high"},
                    {"effort": "xhigh"}, {"effort": "max"}
                ],
                "service_tiers": [{"id": "priority"}],
                "additional_speed_tiers": ["fast"]
            },
            // Upstream retired this slug and the fixed official list no
            // longer carries it; an old cache that still holds it must not
            // bring it back into the generated catalog.
            {
                "slug": "gpt-5.3-codex-spark",
                "display_name": "GPT-5.3-Codex-Spark",
                "visibility": "list",
                "priority": 30,
                "supported_in_api": true,
                "default_reasoning_level": "high",
                "supported_reasoning_levels": [
                    {"effort": "low"}, {"effort": "medium"},
                    {"effort": "high"}, {"effort": "xhigh"}
                ],
                "service_tiers": [],
                "additional_speed_tiers": []
            },
            {"slug": "codex-auto-review", "visibility": "hide", "priority": 43}
        ]
    });
    let bundled = codey_runtime_core::model_suffix::bundled_model_catalog().unwrap();
    for (slug, _) in OFFICIAL_MODELS {
        let exists = cache["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| model["slug"] == slug);
        if exists {
            continue;
        }
        let model = bundled["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["slug"] == slug)
            .unwrap()
            .clone();
        cache["models"].as_array_mut().unwrap().push(model);
    }
    for model in cache["models"].as_array_mut().unwrap() {
        let slug = model["slug"].as_str().unwrap_or("test-model").to_string();
        match slug.as_str() {
            "gpt-6-astra" | "gpt-5.6-sol" | "gpt-5.6-terra" => {
                model["multi_agent_version"] = json!("v2");
            }
            "gpt-5.6-luna" => {
                model["multi_agent_version"] = json!("v1");
            }
            "gpt-5.3-codex-spark" => {
                model["multi_agent_version"] = json!("disabled");
            }
            _ => {
                model.as_object_mut().unwrap().remove("multi_agent_version");
            }
        }
        model["base_instructions"] = json!(format!("test-only instructions for {slug}"));
        model["model_messages"] = json!({
            "instructions_template": "test-only template"
        });
    }
    cache
}

fn write_cache(home: &Path) {
    fs::write(
        home.join("models_cache.json"),
        serde_json::to_vec(&official_cache()).unwrap(),
    )
    .unwrap();
}

#[test]
fn catalog_operation_reuses_runtime_resolution_across_refresh_and_state_reads() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let context = CatalogContext::new("");
    let before = catalog_operation_counts();
    let read_state = || {
        selection_state_in_context(home.path(), true, None, &[], &[], None, None, &context).unwrap()
    };
    assert!(!read_state().official_models.is_empty());
    refresh_for_provider_in_context(
        home.path(),
        true,
        None,
        &[],
        CapabilityLists::default(),
        CatalogOverrides {
            plugin_contexts: &BTreeMap::new(),
            contexts: &BTreeMap::new(),
            reasoning_efforts: &BTreeMap::new(),
            display_names: &BTreeMap::new(),
        },
        &context,
    )
    .unwrap();
    assert!(!read_state().official_models.is_empty());
    assert!(prepare_cached_catalog_in_context(home.path(), &[], &[], &[], &context).unwrap());
    let after = catalog_operation_counts();
    assert_eq!(after.0 - before.0, 1);
}

#[test]
fn selection_reads_missing_or_outdated_snapshots_without_starting_cli() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let runtime = home.path().join("codex");
    fs::write(&runtime, "runtime v1").unwrap();
    let runtime_path = runtime.to_str().unwrap();
    let before = catalog_operation_counts();
    for has_snapshot in [false, true] {
        if has_snapshot {
            let source = catalog_source_for_app(runtime_path).unwrap();
            let mut snapshot = official_cache();
            snapshot["codey_runtime_source"] = serde_json::to_value(source).unwrap();
            snapshot["codey_account_snapshot"] = json!(true);
            atomic_write(
                &home.path().join(DEBUG_CATALOG_RELATIVE_PATH),
                &serde_json::to_vec(&snapshot).unwrap(),
            )
            .unwrap();
            fs::write(&runtime, "replacement runtime v2").unwrap();
        }
        let state = selection_state_with_manual_models(
            home.path(),
            true,
            None,
            &[],
            &[],
            None,
            None,
            runtime_path,
        )
        .unwrap();
        assert!(!state.official_models.is_empty());
    }
    assert_eq!(catalog_operation_counts().1 - before.1, 0);
}

#[test]
fn new_catalog_operation_rechecks_runtime_identity_and_rejects_previous_snapshot() {
    let home = tempfile::tempdir().unwrap();
    let first_runtime = home.path().join("codex-first");
    let second_runtime = home.path().join("codex-second");
    fs::write(&first_runtime, "same runtime bytes").unwrap();
    fs::write(&second_runtime, "same runtime bytes").unwrap();
    let first_path = first_runtime.to_str().unwrap();
    let first = CatalogContext::new(first_path);
    let snapshot = json!({
        "models": [{
            "slug": "account-only-model",
            "description": "Account model",
            "base_instructions": "Runtime-specific instructions"
        }],
        "codey_account_snapshot": true,
        "codey_runtime_source": first.source(),
    });
    atomic_write(
        &home.path().join(DEBUG_CATALOG_RELATIVE_PATH),
        &serde_json::to_vec(&snapshot).unwrap(),
    )
    .unwrap();
    let has_account_model = |context: &CatalogContext<'_>| {
        read_official_entries_in_context(home.path(), context)
            .unwrap()
            .iter()
            .any(|model| model["slug"] == "account-only-model")
    };
    assert!(has_account_model(&first));
    let second_path = second_runtime.to_str().unwrap();
    assert!(!has_account_model(&CatalogContext::new(second_path)));
    fs::write(&first_runtime, "replacement runtime with a new identity").unwrap();
    assert!(!has_account_model(&CatalogContext::new(first_path)));
}

#[test]
fn account_snapshot_exposes_models_unknown_to_the_fallback_list() {
    let home = tempfile::tempdir().unwrap();
    let snapshot = json!({
        "codey_account_snapshot": true,
        "codey_runtime_source": catalog_source_for_app(""),
        "models": [
            {
                "slug": "gpt-6-sol",
                "display_name": "GPT-6-Sol",
                "visibility": "list",
                "description": "GPT-6-Sol",
                "base_instructions": "account instructions",
                "supported_reasoning_levels": [{"effort": "low"}]
            },
            {
                "slug": "gpt-6-luna",
                "display_name": "GPT-6-Luna",
                "visibility": "list",
                "description": "GPT-6-Luna",
                "base_instructions": "account instructions",
                "supported_reasoning_levels": [{"effort": "low"}]
            }
        ]
    });
    fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
    fs::write(
        home.path().join(DEBUG_CATALOG_RELATIVE_PATH),
        serde_json::to_vec(&snapshot).unwrap(),
    )
    .unwrap();

    let state = selection_state(home.path(), true, None, &[], None).unwrap();
    assert_eq!(state.official_model_ids, ["gpt-6-sol", "gpt-6-luna"]);
}

#[test]
fn mismatched_and_unmarked_codey_catalogs_never_supply_runtime_templates() {
    let home = tempfile::tempdir().unwrap();
    let first = home.path().join("first-cli");
    let second = home.path().join("second-cli");
    fs::write(&first, "first runtime").unwrap();
    fs::write(&second, "second runtime").unwrap();
    fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
    for marked in [true, false] {
        let mut snapshot = official_cache();
        snapshot["codey_account_snapshot"] = json!(true);
        if marked {
            snapshot["codey_runtime_source"] =
                json!(catalog_source_for_app(first.to_str().unwrap()));
        }
        for relative in [DEBUG_CATALOG_RELATIVE_PATH, MODEL_CATALOG_RELATIVE_PATH] {
            fs::write(
                home.path().join(relative),
                serde_json::to_vec(&snapshot).unwrap(),
            )
            .unwrap();
        }
        let before = fs::read(home.path().join(DEBUG_CATALOG_RELATIVE_PATH)).unwrap();
        let entries = read_official_entries(home.path(), second.to_str().unwrap()).unwrap();
        assert!(
            entries
                .iter()
                .all(|model| model_instruction_source(model).is_none())
        );
        let error = refresh_for_provider_with_capabilities(
            home.path(),
            true,
            None,
            &[],
            CapabilityLists::default(),
            second.to_str().unwrap(),
        )
        .unwrap_err();
        assert!(is_runtime_model_cache_unavailable(&error));
        assert!(
            !prepare_cached_catalog_for_current_capabilities(
                home.path(),
                &[],
                &[],
                &[],
                second.to_str().unwrap(),
            )
            .unwrap()
        );
        assert_eq!(
            before,
            fs::read(home.path().join(DEBUG_CATALOG_RELATIVE_PATH)).unwrap()
        );
    }
}

#[test]
fn matching_source_remains_usable_offline_and_cache_keys_include_runtime_source() {
    let home = tempfile::tempdir().unwrap();
    let first = home.path().join("first-cli");
    let second = home.path().join("second-cli");
    fs::write(&first, "runtime one").unwrap();
    fs::write(&second, "runtime two").unwrap();
    let mut snapshot = official_cache();
    snapshot["codey_account_snapshot"] = json!(true);
    snapshot["codey_runtime_source"] = json!(catalog_source_for_app(first.to_str().unwrap()));
    fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
    fs::write(
        home.path().join(DEBUG_CATALOG_RELATIVE_PATH),
        serde_json::to_vec(&snapshot).unwrap(),
    )
    .unwrap();
    let first_entries = read_official_entries(home.path(), first.to_str().unwrap()).unwrap();
    assert!(
        first_entries
            .iter()
            .any(|model| model_instruction_source(model).is_some())
    );
    let second_entries = read_official_entries(home.path(), second.to_str().unwrap()).unwrap();
    assert!(
        second_entries
            .iter()
            .all(|model| model_instruction_source(model).is_none())
    );
    refresh_for_provider_with_capabilities(
        home.path(),
        true,
        None,
        &[],
        CapabilityLists::default(),
        first.to_str().unwrap(),
    )
    .unwrap();
    assert!(
        prepare_cached_catalog_for_current_capabilities(
            home.path(),
            &[],
            &[],
            &[],
            first.to_str().unwrap(),
        )
        .unwrap()
    );
    assert!(
        !prepare_cached_catalog_for_current_capabilities(
            home.path(),
            &[],
            &[],
            &[],
            second.to_str().unwrap(),
        )
        .unwrap()
    );
}

#[test]
fn account_merge_excludes_old_runtime_templates_but_preserves_official_cache() {
    let home = tempfile::tempdir().unwrap();
    let first = home.path().join("first-cli");
    let second = home.path().join("second-cli");
    fs::write(&first, "runtime one").unwrap();
    fs::write(&second, "runtime two").unwrap();
    write_cache(home.path());
    let cache_before = fs::read(home.path().join("models_cache.json")).unwrap();
    let mut snapshot = official_cache();
    for model in snapshot["models"].as_array_mut().unwrap() {
        model["base_instructions"] = json!("old installation instructions");
    }
    snapshot["codey_runtime_source"] = json!(catalog_source_for_app(first.to_str().unwrap()));
    fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
    fs::write(
        home.path().join(DEBUG_CATALOG_RELATIVE_PATH),
        serde_json::to_vec(&snapshot).unwrap(),
    )
    .unwrap();
    merge_account_runtime_models(
        home.path(),
        &[json!({"slug": "gpt-6-sol", "visibility": "list"})],
        second.to_str().unwrap(),
    )
    .unwrap();
    let saved = read_catalog_value(&home.path().join(DEBUG_CATALOG_RELATIVE_PATH)).unwrap();
    assert!(snapshot_matches_runtime_source(
        &saved,
        catalog_source_for_app(second.to_str().unwrap()).as_ref()
    ));
    assert_eq!(saved["models"].as_array().unwrap().len(), 1);
    assert_ne!(
        saved["models"][0]["base_instructions"],
        "old installation instructions"
    );
    assert!(model_instruction_source(&saved["models"][0]).is_some());
    assert_eq!(
        cache_before,
        fs::read(home.path().join("models_cache.json")).unwrap()
    );
}

#[test]
fn account_snapshot_ultrafast_follows_the_two_x_fast_family() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let snapshot = json!({
        "codey_account_snapshot": true,
        "codey_runtime_source": catalog_source_for_app(""),
        "models": [
            {
                "slug": "gpt-6.1-sol",
                "display_name": "GPT-6.1-Sol",
                "visibility": "list",
                "description": "GPT-6.1-Sol"
            },
            {
                "slug": "gpt-6-astra",
                "display_name": "GPT-6-Astra",
                "visibility": "list",
                "description": "GPT-6-Astra"
            },
            {
                "slug": "gpt-5.6-sol",
                "display_name": "GPT-5.6-Sol",
                "visibility": "list",
                "description": "GPT-5.6-Sol"
            }
        ]
    });
    fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
    fs::write(
        home.path().join(DEBUG_CATALOG_RELATIVE_PATH),
        serde_json::to_vec(&snapshot).unwrap(),
    )
    .unwrap();

    refresh_for_provider(home.path(), true, None, &[]).unwrap();

    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let models = catalog["models"].as_array().unwrap();
    for slug in ["gpt-6.1-sol", "gpt-6-astra"] {
        let Some(model) = models.iter().find(|model| model["slug"] == slug) else {
            panic!("目录里缺少 {slug}");
        };
        // Fast 仍是第一档，Ultrafast 追加在它后面，选择器的默认顺序不变。
        assert_eq!(model["service_tiers"][0]["id"], FAST_SERVICE_TIER_ID);
        assert_native_fast(model);
        assert_native_ultrafast(model);
    }
    // 上游只按 1.5x 声明 Fast 的模型不会被顺带升级。
    let sol56 = models
        .iter()
        .find(|model| model["slug"] == "gpt-5.6-sol")
        .unwrap();
    assert!(
        !sol56["service_tiers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tier| tier["id"] == ULTRAFAST_SERVICE_TIER_ID)
    );
}

#[test]
fn route_alias_for_six_one_sol_keeps_ultrafast_speed() {
    let home = tempfile::tempdir().unwrap();
    let mut cache = official_cache();
    cache["models"].as_array_mut().unwrap().push(json!({
        "slug": "gpt-6.1-sol",
        "display_name": "GPT-6.1-Sol",
        "visibility": "list",
        "priority": 0,
        "default_reasoning_level": "low",
        "supported_reasoning_levels": [{"effort": "low"}, {"effort": "medium"}],
        "service_tiers": [{"id": "priority"}],
        "additional_speed_tiers": ["fast"]
    }));
    fs::write(
        home.path().join("models_cache.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();

    let selection = vec!["route-x/gpt-6.1-sol".to_string()];
    refresh_for_provider(home.path(), false, Some(&selection), &selection).unwrap();

    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let alias = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["slug"] == "route-x/gpt-6.1-sol")
        .unwrap();
    assert_native_fast(alias);
    assert_native_ultrafast(alias);
}

#[test]
fn ultrafast_speed_support_is_limited_to_the_two_x_fast_family() {
    for slug in [
        "gpt-6.1-sol",
        "gpt-6.1-sol-wm",
        "gpt-6.1-sol-priority",
        "route-mu5dql1a/gpt-6.1-sol",
        "my%20route/gpt-6.1-sol-wm",
        "gpt-6-astra",
        "gpt-6-astra-wm",
        "codey-official-account-x/gpt-6-astra",
    ] {
        let model = json!({ "slug": slug });
        assert!(supports_ultrafast_speed(&model), "{slug} 应支持 Ultrafast");
    }
    for slug in [
        "gpt-5.6-sol",
        "gpt-5.6-luna",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-6.1-sol2",
        "gpt-6-astra2",
        "route-x/gpt-6-astra-max",
    ] {
        let model = json!({ "slug": slug });
        assert!(
            !supports_ultrafast_speed(&model),
            "{slug} 不应支持 Ultrafast"
        );
    }
}

#[test]
fn ultrafast_speed_controls_keep_fast_first_and_stay_idempotent() {
    let mut model = json!({
        "slug": "gpt-6.1-sol",
        "service_tiers": [{ "id": FAST_SERVICE_TIER_ID, "name": "Fast" }],
        "additional_speed_tiers": [FAST_SPEED_TIER_ID]
    });
    add_ultrafast_speed_controls(&mut model);
    add_ultrafast_speed_controls(&mut model);

    let tiers = model["service_tiers"].as_array().unwrap();
    assert_eq!(tiers.len(), 2);
    assert_eq!(tiers[0]["id"], FAST_SERVICE_TIER_ID);
    assert_eq!(tiers[1]["id"], ULTRAFAST_SERVICE_TIER_ID);
    assert_eq!(
        model["additional_speed_tiers"],
        json!([FAST_SPEED_TIER_ID, ULTRAFAST_SPEED_TIER_ID])
    );

    remove_ultrafast_speed_controls(&mut model);
    assert_eq!(model["service_tiers"].as_array().unwrap().len(), 1);
    assert_eq!(model["additional_speed_tiers"], json!([FAST_SPEED_TIER_ID]));
}

#[test]
fn account_snapshot_borrows_a_local_template_for_slugs_the_fallback_list_omits() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let snapshot = json!({
        "codey_account_snapshot": true,
        "codey_runtime_source": catalog_source_for_app(""),
        "models": [
            {
                "slug": "gpt-6-sol",
                "display_name": "GPT-6-Sol",
                "visibility": "list",
                "description": "GPT-6-Sol"
            },
            {
                "slug": "gpt-6-luna",
                "display_name": "GPT-6-Luna",
                "visibility": "list"
            },
            {"slug": "codex-auto-review", "visibility": "hide"}
        ]
    });
    fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
    fs::write(
        home.path().join(DEBUG_CATALOG_RELATIVE_PATH),
        serde_json::to_vec(&snapshot).unwrap(),
    )
    .unwrap();

    let state = selection_state(home.path(), true, None, &[], None).unwrap();
    assert_eq!(state.official_model_ids, ["gpt-6-sol", "gpt-6-luna"]);
    let sol = state
        .official_models
        .iter()
        .find(|model| model.slug == "gpt-6-sol")
        .unwrap();
    assert!(
        sol.supported_reasoning_efforts
            .iter()
            .any(|effort| effort == "ultra")
    );
    assert_eq!(
        refresh_for_provider(home.path(), true, None, &[]).unwrap(),
        2
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let models = catalog["models"].as_array().unwrap();
    assert!(
        models
            .iter()
            .all(|model| model_instruction_source(model).is_some())
    );
    let luna = models
        .iter()
        .find(|model| model["slug"] == "gpt-6-luna")
        .unwrap();
    assert_eq!(luna["description"], "GPT-6-Luna");
    assert_eq!(luna["multi_agent_version"], "v1");
    let sol = models
        .iter()
        .find(|model| model["slug"] == "gpt-6-sol")
        .unwrap();
    assert_eq!(sol["multi_agent_version"], "v2");
    assert_eq!(sol["description"], "GPT-6-Sol");
}

#[test]
fn merging_account_models_preserves_runtime_templates_for_new_slugs() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
    let snapshot = json!({
        "codey_account_snapshot": true,
        "codey_runtime_source": catalog_source_for_app(""),
        "models": [
            {
                "slug": "gpt-5.6-sol",
                "display_name": "GPT-5.6-Sol",
                "visibility": "list",
                "description": "Local Sol",
                "base_instructions": "sol instructions",
                "multi_agent_version": "v2"
            },
            {
                "slug": "gpt-5.6-luna",
                "display_name": "GPT-5.6-Luna",
                "visibility": "list",
                "description": "Local Luna",
                "base_instructions": "luna instructions",
                "multi_agent_version": "v1"
            }
        ]
    });
    fs::write(
        home.path().join(DEBUG_CATALOG_RELATIVE_PATH),
        serde_json::to_vec(&snapshot).unwrap(),
    )
    .unwrap();

    merge_account_runtime_models(
        home.path(),
        &[
            json!({
                "slug": "gpt-5.6-sol",
                "display_name": "GPT-5.6-Sol",
                "visibility": "list",
                "description": "Updated Sol"
            }),
            json!({
                "slug": "gpt-6-sol",
                "display_name": "GPT-6-Sol",
                "visibility": "list",
                "description": "GPT-6-Sol"
            }),
            json!({
                "slug": "gpt-6-luna",
                "display_name": "GPT-6-Luna",
                "visibility": "list"
            }),
        ],
        "",
    )
    .unwrap();

    let saved: Value =
        serde_json::from_slice(&fs::read(home.path().join(DEBUG_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let models = saved["models"].as_array().unwrap();
    let sol = models
        .iter()
        .find(|model| model["slug"] == "gpt-5.6-sol")
        .unwrap();
    assert_eq!(sol["base_instructions"], "sol instructions");
    assert_eq!(sol["description"], "Updated Sol");
    let gpt6_sol = models
        .iter()
        .find(|model| model["slug"] == "gpt-6-sol")
        .unwrap();
    assert_eq!(gpt6_sol["base_instructions"], "sol instructions");
    assert_eq!(gpt6_sol["description"], "GPT-6-Sol");
    assert_eq!(gpt6_sol["multi_agent_version"], "v2");
    let gpt6_luna = models
        .iter()
        .find(|model| model["slug"] == "gpt-6-luna")
        .unwrap();
    assert_eq!(gpt6_luna["base_instructions"], "luna instructions");
    assert_eq!(gpt6_luna["description"], "GPT-6-Luna");
    assert_eq!(gpt6_luna["multi_agent_version"], "v1");
}

fn staged_catalog_refresh(home: &Path, selected: &[String], overrides: CatalogOverrides<'_>) {
    refresh_for_provider_with_capabilities(
        home,
        false,
        Some(selected),
        selected,
        CapabilityLists::default(),
        "",
    )
    .unwrap();
    let mut models = read_runtime_catalog_models(home).unwrap();
    for model in &mut models {
        let policy = model["slug"].as_str().and_then(|slug| {
            overrides
                .contexts
                .iter()
                .find(|(key, _)| model_id::equal(key, slug))
                .map(|(_, policy)| policy)
        });
        apply_model_context(model, policy).unwrap();
    }
    write_verified_catalog(home, &models).unwrap();
    apply_catalog_reasoning_efforts(home, overrides.reasoning_efforts).unwrap();
}

#[test]
fn combined_catalog_refresh_preserves_staged_context_and_reasoning_results() {
    let staged = tempfile::tempdir().unwrap();
    let combined = tempfile::tempdir().unwrap();
    for home in [staged.path(), combined.path()] {
        write_cache(home);
    }
    let selected = vec![
        "route/gpt-5.6-sol".to_string(),
        "route/custom-model".to_string(),
    ];
    let contexts = std::collections::BTreeMap::from([(
        "ROUTE/GPT-5.6-SOL".to_string(),
        crate::config::ModelContextConfig {
            context_window_tokens: 256_000,
            auto_compact_token_limit: Some(200_000),
            reserve_output_tokens: Some(32_000),
        },
    )]);
    let efforts = std::collections::BTreeMap::from([(
        "route/gpt-5.6-sol".to_string(),
        vec![crate::config::ModelReasoningEffort {
            level: "high".into(),
            value: "high".into(),
        }],
    )]);
    let overrides = CatalogOverrides {
        plugin_contexts: &Default::default(),
        contexts: &contexts,
        reasoning_efforts: &efforts,
        display_names: &Default::default(),
    };
    staged_catalog_refresh(staged.path(), &selected, overrides);
    refresh_for_provider_with_contexts(
        combined.path(),
        false,
        Some(&selected),
        &selected,
        CapabilityLists::default(),
        overrides,
        "",
    )
    .unwrap();
    assert_eq!(
        fs::read(staged.path().join(relative_path())).unwrap(),
        fs::read(combined.path().join(relative_path())).unwrap()
    );

    // Removing a declaration from a cached catalog must restore its template.
    let no_efforts = std::collections::BTreeMap::new();
    apply_catalog_reasoning_efforts(staged.path(), &no_efforts).unwrap();
    apply_catalog_overrides(
        combined.path(),
        CatalogOverrides {
            plugin_contexts: &Default::default(),
            contexts: &contexts,
            reasoning_efforts: &no_efforts,
            display_names: &Default::default(),
        },
    )
    .unwrap();
    assert_eq!(
        fs::read(staged.path().join(relative_path())).unwrap(),
        fs::read(combined.path().join(relative_path())).unwrap()
    );
}

#[test]
fn route_display_names_replace_raw_route_ids_in_the_generated_catalog() {
    // Codex renders this field in the model picker and the model-change
    // divider; a route alias there reads as a long internal id.
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let selected = vec![
        "route/gpt-5.6-sol".to_string(),
        "route/custom-model".to_string(),
    ];
    let display_names = std::collections::BTreeMap::from([
        (
            "route/gpt-5.6-sol".to_string(),
            "[ro] gpt-5.6-sol".to_string(),
        ),
        (
            "route/custom-model".to_string(),
            "[ro] custom-model".to_string(),
        ),
    ]);
    refresh_for_provider_with_contexts(
        home.path(),
        false,
        Some(&selected),
        &selected,
        CapabilityLists::default(),
        CatalogOverrides {
            plugin_contexts: &Default::default(),
            contexts: &Default::default(),
            reasoning_efforts: &Default::default(),
            display_names: &display_names,
        },
        "",
    )
    .unwrap();
    for model in read_runtime_catalog_models(home.path()).unwrap() {
        let slug = model["slug"].as_str().unwrap();
        assert_eq!(
            model["display_name"].as_str().unwrap(),
            display_names[slug].as_str(),
            "unexpected display name for {slug}"
        );
    }
    // Reusing the cached catalog must keep the same display names instead
    // of restoring the raw route ids.
    apply_catalog_overrides(
        home.path(),
        CatalogOverrides {
            plugin_contexts: &Default::default(),
            contexts: &Default::default(),
            reasoning_efforts: &Default::default(),
            display_names: &display_names,
        },
    )
    .unwrap();
    for model in read_runtime_catalog_models(home.path()).unwrap() {
        let slug = model["slug"].as_str().unwrap();
        assert_eq!(
            model["display_name"].as_str().unwrap(),
            display_names[slug].as_str()
        );
    }
}

#[test]
fn invalid_override_keeps_previous_catalog_for_generation_and_cached_updates() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let selected = vec!["route/gpt-5.6-sol".to_string()];
    refresh_for_provider_with_capabilities(
        home.path(),
        false,
        Some(&selected),
        &selected,
        CapabilityLists::default(),
        "",
    )
    .unwrap();
    let path = home.path().join(relative_path());
    let original = fs::read(&path).unwrap();
    let contexts = std::collections::BTreeMap::from([(
        selected[0].clone(),
        crate::config::ModelContextConfig {
            context_window_tokens: 0,
            auto_compact_token_limit: None,
            reserve_output_tokens: None,
        },
    )]);
    let efforts = std::collections::BTreeMap::new();
    let overrides = CatalogOverrides {
        plugin_contexts: &Default::default(),
        contexts: &contexts,
        reasoning_efforts: &efforts,
        display_names: &Default::default(),
    };
    assert!(
        refresh_for_provider_with_contexts(
            home.path(),
            false,
            Some(&selected),
            &selected,
            CapabilityLists::default(),
            overrides,
            ""
        )
        .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(apply_catalog_overrides(home.path(), overrides).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
#[ignore = "使用合成模型目录测量保存耗时，按需运行"]
fn benchmark_combined_catalog_refresh() {
    let staged = tempfile::tempdir().unwrap();
    let combined = tempfile::tempdir().unwrap();
    let mut cache = official_cache();
    for model in cache["models"].as_array_mut().unwrap() {
        model["base_instructions"] =
            json!("synthetic instructions for catalog benchmark\n".repeat(1_000));
    }
    let cache = serde_json::to_vec(&cache).unwrap();
    for home in [staged.path(), combined.path()] {
        fs::write(home.join("models_cache.json"), &cache).unwrap();
    }
    let selected = (0..24)
        .map(|i| format!("route-{i}/gpt-5.6-sol"))
        .collect::<Vec<_>>();
    let contexts = selected
        .iter()
        .map(|slug| {
            (
                slug.clone(),
                crate::config::ModelContextConfig {
                    context_window_tokens: 256_000,
                    auto_compact_token_limit: None,
                    reserve_output_tokens: None,
                },
            )
        })
        .collect();
    let efforts = std::collections::BTreeMap::new();
    let overrides = CatalogOverrides {
        plugin_contexts: &Default::default(),
        contexts: &contexts,
        reasoning_efforts: &efforts,
        display_names: &Default::default(),
    };
    let mut staged_us = Vec::new();
    let mut combined_us = Vec::new();
    for _ in 0..7 {
        let started = std::time::Instant::now();
        staged_catalog_refresh(staged.path(), &selected, overrides);
        staged_us.push(started.elapsed().as_micros());
        let started = std::time::Instant::now();
        refresh_for_provider_with_contexts(
            combined.path(),
            false,
            Some(&selected),
            &selected,
            CapabilityLists::default(),
            overrides,
            "",
        )
        .unwrap();
        combined_us.push(started.elapsed().as_micros());
    }
    let bytes = fs::read(staged.path().join(relative_path())).unwrap();
    assert_eq!(
        bytes,
        fs::read(combined.path().join(relative_path())).unwrap()
    );
    staged_us.sort_unstable();
    combined_us.sort_unstable();
    eprintln!(
        "catalogBytes={} stagedMedianUs={} combinedMedianUs={}",
        bytes.len(),
        staged_us[3],
        combined_us[3]
    );
}

fn write_cache_with_native_web_search(home: &Path) {
    let mut cache = official_cache();
    let sol = cache["models"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|model| model["slug"] == "gpt-5.6-sol")
        .unwrap();
    sol["supports_search_tool"] = json!(true);
    sol["web_search_tool_type"] = json!("text_and_image");
    fs::write(
        home.join("models_cache.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();
}

fn write_cache_with_image_detail_original(home: &Path) {
    let mut cache = official_cache();
    let sol = cache["models"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|model| model["slug"] == "gpt-5.6-sol")
        .unwrap();
    sol["supports_image_detail_original"] = json!(true);
    fs::write(
        home.join("models_cache.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();
}

fn write_cache_with_template_only(home: &Path) {
    let mut cache = official_cache();
    for model in cache["models"].as_array_mut().unwrap() {
        let slug = model["slug"].as_str().unwrap_or("test-model").to_owned();
        model.as_object_mut().unwrap().remove("base_instructions");
        model["model_messages"] = json!({
            "instructions_template": "test-only prefix {{ personality }} suffix",
            "instructions_variables": {
                "personality_default": format!("test-only default personality for {slug}"),
                "personality_friendly": "test-only friendly personality",
                "personality_pragmatic": "test-only pragmatic personality"
            }
        });
    }
    fs::write(
        home.join("models_cache.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();
}

fn write_cache_without_fast_metadata(home: &Path) {
    let mut cache = official_cache();
    for model in cache["models"].as_array_mut().unwrap() {
        let object = model.as_object_mut().unwrap();
        object.remove("service_tiers");
        object.remove("additional_speed_tiers");
    }
    fs::write(
        home.join("models_cache.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();
}

fn write_cache_with_prompt_fields(home: &Path) {
    let mut cache = official_cache();
    let model = &mut cache["models"][0];
    model["base_instructions"] = json!("runtime-cache-only base instructions");
    model["model_messages"] = json!({
        "instructions_template": "runtime-cache-only template",
        "instructions_variables": {
            "developer": "runtime-cache-only variable"
        }
    });
    model["compatibility"] = json!({
        "instructions_template": "runtime-cache-only nested template",
        "instructions_variables": {
            "nested": "runtime-cache-only nested variable"
        }
    });
    fs::write(
        home.join("models_cache.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();
}

fn write_cache_with_sol_reasoning_metadata(
    home: &Path,
    supported_reasoning_levels: Option<Value>,
    default_reasoning_level: Option<&str>,
) {
    let mut cache = official_cache();
    let sol = cache["models"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|model| model["slug"] == "gpt-5.6-sol")
        .unwrap();
    let object = sol.as_object_mut().unwrap();
    match supported_reasoning_levels {
        Some(levels) => {
            object.insert("supported_reasoning_levels".to_string(), levels);
        }
        None => {
            object.remove("supported_reasoning_levels");
        }
    }
    match default_reasoning_level {
        Some(default) => {
            object.insert("default_reasoning_level".to_string(), json!(default));
        }
        None => {
            object.remove("default_reasoning_level");
        }
    }
    fs::write(
        home.join("models_cache.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();
}

fn assert_native_fast(model: &Value) {
    assert!(
        model["service_tiers"]
            .as_array()
            .is_some_and(|tiers| tiers.iter().any(|tier| tier["id"] == FAST_SERVICE_TIER_ID))
    );
    assert!(
        model["additional_speed_tiers"]
            .as_array()
            .is_some_and(|tiers| tiers.iter().any(|tier| tier == FAST_SPEED_TIER_ID))
    );
}

fn assert_native_ultrafast(model: &Value) {
    assert!(
        model["service_tiers"].as_array().is_some_and(|tiers| tiers
            .iter()
            .any(|tier| tier["id"] == ULTRAFAST_SERVICE_TIER_ID)),
        "{} 的 service_tiers 缺少 Ultrafast：{}",
        model["slug"],
        model["service_tiers"]
    );
    assert!(
        model["additional_speed_tiers"]
            .as_array()
            .is_some_and(|tiers| tiers.iter().any(|tier| tier == ULTRAFAST_SPEED_TIER_ID)),
        "{} 的 additional_speed_tiers 缺少 Ultrafast：{}",
        model["slug"],
        model["additional_speed_tiers"]
    );
}

#[test]
fn official_catalog_keeps_the_fixed_order_and_native_fast_metadata() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());

    assert_eq!(
        refresh_for_provider(home.path(), true, None, &[]).unwrap(),
        OFFICIAL_MODELS.len()
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let models = catalog["models"].as_array().unwrap();
    assert_eq!(
        models
            .iter()
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>(),
        OFFICIAL_MODELS
            .iter()
            .map(|(slug, _)| *slug)
            .collect::<Vec<_>>()
    );
    assert!(models.iter().all(|model| model["visibility"] == "list"));
    let sol = models
        .iter()
        .find(|model| model["slug"] == "gpt-5.6-sol")
        .unwrap();
    assert_eq!(sol["multi_agent_version"], "v2");
    let efforts = sol["supported_reasoning_levels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|level| level["effort"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(efforts, ["low", "medium", "high", "xhigh", "max", "ultra"]);
    assert_eq!(sol["service_tiers"][0]["id"], "priority");
    assert_eq!(sol["supports_reasoning_summaries"], true);
    let gpt_55 = models
        .iter()
        .find(|model| model["slug"] == "gpt-5.5")
        .unwrap();
    assert_eq!(gpt_55["service_tiers"][0]["id"], "priority");
    assert!(gpt_55.get("multi_agent_version").is_none());
    let luna = models
        .iter()
        .find(|model| model["slug"] == "gpt-5.6-luna")
        .unwrap();
    assert_eq!(luna["multi_agent_version"], "v1");
    // Upstream retired this slug, so a cache that still carries it must not
    // put it back into the generated catalog.
    assert!(
        !models
            .iter()
            .any(|model| model["slug"] == "gpt-5.3-codex-spark")
    );
    assert_eq!(
        models
            .iter()
            .filter(|model| declares_fast_speed_support(model))
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "gpt-6-astra",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
        ]
    );
}

#[test]
fn generated_catalog_follows_the_configured_model_order() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let catalog_slugs = || {
        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| model["slug"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };

    let official_selection = vec![
        "gpt-5.6-luna".to_string(),
        "gpt-5.5".to_string(),
        "gpt-6-astra".to_string(),
    ];
    assert_eq!(
        refresh_for_provider(home.path(), true, None, &official_selection).unwrap(),
        3
    );
    assert_eq!(catalog_slugs(), official_selection);

    // 本地路由目录里官方原生 ID 与第三方别名混排，也按线路配置的顺序输出。
    let mixed_selection = vec![
        "kimi/k3".to_string(),
        "gpt-5.6-sol".to_string(),
        "kimi/kimi-for-coding".to_string(),
    ];
    assert_eq!(
        refresh_for_provider(home.path(), false, Some(&mixed_selection), &mixed_selection).unwrap(),
        3
    );
    assert_eq!(catalog_slugs(), mixed_selection);
}

#[test]
fn generated_catalog_preserves_official_multi_agent_markers() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());

    refresh_for_provider(home.path(), true, None, &[]).unwrap();

    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let models = catalog["models"].as_array().unwrap();
    let marker = |slug: &str| {
        models
            .iter()
            .find(|model| model["slug"] == slug)
            .and_then(|model| model.get("multi_agent_version"))
            .and_then(Value::as_str)
    };

    assert_eq!(marker("gpt-6-astra"), Some("v2"));
    assert_eq!(marker("gpt-5.6-sol"), Some("v2"));
    assert_eq!(marker("gpt-5.6-terra"), Some("v2"));
    assert_eq!(marker("gpt-5.6-luna"), Some("v1"));
    assert_eq!(marker("gpt-5.3-codex-spark"), None);
    assert_eq!(marker("gpt-5.5"), None);
}

#[test]
fn generated_catalog_keeps_leaf_models_without_v2_coordinator_markers() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let upstream = vec!["gpt-5.6-luna".into(), "provider-custom-model".into()];

    refresh_for_provider(home.path(), false, Some(&upstream), &upstream).unwrap();

    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let models = catalog["models"].as_array().unwrap();
    let luna = models
        .iter()
        .find(|model| model["slug"] == "gpt-5.6-luna")
        .unwrap();
    assert_eq!(luna["multi_agent_version"], "v1");
    // A retired upstream slug is no longer part of the fixed official list,
    // so a route that still advertises it must not resurrect the entry.
    assert!(
        !models
            .iter()
            .any(|model| model["slug"] == "gpt-5.3-codex-spark")
    );
    let custom = models
        .iter()
        .find(|model| model["slug"] == "provider-custom-model")
        .unwrap();
    assert!(custom.get("multi_agent_version").is_none());
}

#[test]
fn generated_catalog_preserves_required_fields_from_the_local_native_cache() {
    let home = tempfile::tempdir().unwrap();
    write_cache_with_prompt_fields(home.path());

    refresh_for_provider(home.path(), true, None, &[]).unwrap();

    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let model = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["slug"] == "gpt-5.6-sol")
        .unwrap();
    assert_eq!(
        model["base_instructions"],
        "runtime-cache-only base instructions"
    );
    assert_eq!(
        model["model_messages"]["instructions_template"],
        "runtime-cache-only template"
    );
    assert_eq!(
        model["compatibility"]["instructions_variables"]["nested"],
        "runtime-cache-only nested variable"
    );
    assert!(is_available(home.path()));
}

#[test]
fn generated_catalog_derives_base_instructions_from_the_local_template() {
    let home = tempfile::tempdir().unwrap();
    write_cache_with_template_only(home.path());

    refresh_for_provider(home.path(), true, None, &[]).unwrap();

    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let models = catalog["models"].as_array().unwrap();
    assert!(models.iter().all(|model| {
        let template = model["model_messages"]["instructions_template"]
            .as_str()
            .unwrap();
        let personality = model["model_messages"]["instructions_variables"]["personality_default"]
            .as_str()
            .unwrap();
        model["base_instructions"] == template.replace(PERSONALITY_PLACEHOLDER, personality)
    }));
    assert!(is_available(home.path()));
}

#[test]
fn generated_catalog_fills_missing_or_empty_official_descriptions() {
    let home = tempfile::tempdir().unwrap();
    let mut cache = official_cache();
    let models = cache["models"].as_array_mut().unwrap();
    models
        .iter_mut()
        .find(|model| model["slug"] == "gpt-5.6-sol")
        .unwrap()["description"] = json!("Local Sol description");
    models
        .iter_mut()
        .find(|model| model["slug"] == "gpt-5.5")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("description");
    models
        .iter_mut()
        .find(|model| model["slug"] == "gpt-5.6-luna")
        .unwrap()["description"] = json!("   ");
    fs::write(
        home.path().join("models_cache.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();

    refresh_for_provider(home.path(), true, None, &[]).unwrap();

    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let models = catalog["models"].as_array().unwrap();
    assert!(models.iter().all(|model| {
        model
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .is_some_and(|description| !description.is_empty())
    }));
    let sol = models
        .iter()
        .find(|model| model["slug"] == "gpt-5.6-sol")
        .unwrap();
    assert_eq!(sol["description"], "Local Sol description");
    for slug in ["gpt-5.5", "gpt-5.6-luna"] {
        let model = models.iter().find(|model| model["slug"] == slug).unwrap();
        assert_eq!(model["description"], model["display_name"]);
    }
}

#[test]
fn instruction_bearing_snapshot_entry_repairs_an_incomplete_cache_entry() {
    let home = tempfile::tempdir().unwrap();
    // Codex 26.908+ may keep a cache file that lacks runtime fields while a
    // `codex debug models` snapshot captured by Codey carries them. The
    // merge must prefer the instruction-bearing entry per slug.
    let mut incomplete = official_cache();
    for model in incomplete["models"].as_array_mut().unwrap() {
        let object = model.as_object_mut().unwrap();
        object.remove("base_instructions");
        object.remove("model_messages");
    }
    fs::write(
        home.path().join("models_cache.json"),
        serde_json::to_vec(&incomplete).unwrap(),
    )
    .unwrap();
    fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
    fs::write(
        home.path().join(DEBUG_CATALOG_RELATIVE_PATH),
        serde_json::to_vec(&{
            let mut cache = official_cache();
            cache["codey_runtime_source"] = json!(catalog_source_for_app(""));
            cache
        })
        .unwrap(),
    )
    .unwrap();

    assert_eq!(
        refresh_for_provider(home.path(), true, None, &[]).unwrap(),
        OFFICIAL_MODELS.len()
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let sol = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["slug"] == "gpt-5.6-sol")
        .unwrap();
    assert!(
        sol["base_instructions"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
}

#[test]
fn incomplete_local_reasoning_metadata_is_completed_from_fallback_catalog() {
    let cases = [
        (None, None, "low"),
        (None, Some("xhigh"), "xhigh"),
        (
            Some(json!([{"effort": "low", "description": "local low"}])),
            Some("low"),
            "low",
        ),
    ];

    for (supported_reasoning_levels, default_reasoning_level, expected_default) in cases {
        let home = tempfile::tempdir().unwrap();
        write_cache_with_sol_reasoning_metadata(
            home.path(),
            supported_reasoning_levels,
            default_reasoning_level,
        );

        let state = selection_state(home.path(), true, None, &[], None).unwrap();
        let sol_state = state
            .official_models
            .iter()
            .find(|model| model.slug == "gpt-5.6-sol")
            .unwrap();
        assert_eq!(
            sol_state.supported_reasoning_efforts,
            ["low", "medium", "high", "xhigh", "max", "ultra"]
        );
        assert_eq!(sol_state.default_reasoning_effort, expected_default);

        refresh_for_provider(home.path(), true, None, &[]).unwrap();
        let catalog: Value = serde_json::from_slice(
            &fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap(),
        )
        .unwrap();
        let sol = catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["slug"] == "gpt-5.6-sol")
            .unwrap();
        assert_eq!(
            reasoning_efforts_from_value(sol),
            ["low", "medium", "high", "xhigh", "max", "ultra"]
        );
        assert_eq!(sol["default_reasoning_level"], expected_default);
        assert_eq!(
            sol["base_instructions"],
            "test-only instructions for gpt-5.6-sol"
        );
        assert_eq!(
            sol["model_messages"]["instructions_template"],
            "test-only template"
        );
    }
}

#[test]
fn explicit_nontrivial_local_reasoning_metadata_remains_authoritative() {
    let home = tempfile::tempdir().unwrap();
    write_cache_with_sol_reasoning_metadata(
        home.path(),
        Some(json!([
            {"effort": "low", "description": "local low"},
            {"effort": "xhigh", "description": "local xhigh"}
        ])),
        Some("xhigh"),
    );

    let state = selection_state(home.path(), true, None, &[], None).unwrap();
    let sol = state
        .official_models
        .iter()
        .find(|model| model.slug == "gpt-5.6-sol")
        .unwrap();
    assert_eq!(sol.supported_reasoning_efforts, ["low", "xhigh"]);
    assert_eq!(sol.default_reasoning_effort, "xhigh");
}

#[cfg(unix)]
#[test]
fn generated_catalog_is_private_on_unix() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());

    refresh_for_provider(home.path(), true, None, &[]).unwrap();

    let mode = fs::metadata(home.path().join(MODEL_CATALOG_RELATIVE_PATH))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn native_cache_without_fast_metadata_inherits_bundled_official_capabilities() {
    let home = tempfile::tempdir().unwrap();
    write_cache_without_fast_metadata(home.path());

    refresh_for_provider(home.path(), true, None, &[]).unwrap();

    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let models = catalog["models"].as_array().unwrap();
    assert_eq!(
        models
            .iter()
            .filter(|model| declares_fast_speed_support(model))
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "gpt-6-astra",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
        ]
    );
    // The retired slug must not reappear just because an old cache carried
    // native fast metadata for it.
    assert!(
        !models
            .iter()
            .any(|model| model["slug"] == "gpt-5.3-codex-spark")
    );
}

#[test]
fn third_party_catalog_keeps_supported_official_models_before_configured_models() {
    let home = tempfile::tempdir().unwrap();
    write_cache_with_template_only(home.path());
    let upstream = vec![
        "gpt-5.6-sol".into(),
        "gpt-5.5".into(),
        "claude-sonnet".into(),
    ];
    let selected = upstream.clone();

    assert_eq!(
        refresh_for_provider(home.path(), false, Some(&upstream), &selected,).unwrap(),
        3
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let models = catalog["models"].as_array().unwrap();
    assert_eq!(
        models
            .iter()
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["gpt-5.6-sol", "gpt-5.5", "claude-sonnet",]
    );
    assert_eq!(
        models[0]["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|level| level["effort"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["low", "medium", "high", "xhigh", "max", "ultra"]
    );
    let gpt_55 = models
        .iter()
        .find(|model| model["slug"] == "gpt-5.5")
        .unwrap();
    assert_eq!(gpt_55["visibility"], "list");
    assert!(gpt_55.get("upgrade").is_none());
    let custom = models.last().unwrap();
    assert_eq!(custom["slug"], "claude-sonnet");
    assert_eq!(custom["codey_source"], "third_party");
    assert!(custom.get("multi_agent_version").is_none());
    assert_eq!(
        custom["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|level| level["effort"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["low", "medium", "high", "xhigh"]
    );
    assert_eq!(custom["supports_reasoning_summaries"], true);
    let custom_template = custom["model_messages"]["instructions_template"]
        .as_str()
        .unwrap();
    let custom_personality =
        custom["model_messages"]["instructions_variables"]["personality_default"]
            .as_str()
            .unwrap();
    assert_eq!(
        custom["base_instructions"],
        custom_template.replace(PERSONALITY_PLACEHOLDER, custom_personality)
    );
    assert_native_fast(custom);
    assert_native_fast(
        models
            .iter()
            .find(|model| model["slug"] == "gpt-5.6-sol")
            .unwrap(),
    );
    assert_native_fast(
        models
            .iter()
            .find(|model| model["slug"] == "gpt-5.5")
            .unwrap(),
    );
}

#[test]
fn mixed_catalog_drops_prompt_free_official_stubs_without_blocking_supported_models() {
    let home = tempfile::tempdir().unwrap();
    let mut cache = official_cache();
    let stub = cache["models"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|model| model["slug"] == "gpt-5.5")
        .unwrap()
        .as_object_mut()
        .unwrap();
    stub.remove("base_instructions");
    stub.remove("model_messages");
    fs::write(
        home.path().join("models_cache.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();
    let selected = vec![
        "gpt-5.6-sol".into(),
        "gpt-5.6-luna".into(),
        "gpt-6-astra".into(),
        "gpt-5.5".into(),
        "route-oc/deepseek-flash".into(),
    ];

    assert_eq!(
        refresh_for_provider(home.path(), false, Some(&selected), &selected).unwrap(),
        4
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let models = catalog["models"].as_array().unwrap();
    assert_eq!(
        models
            .iter()
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "gpt-5.6-sol",
            "gpt-5.6-luna",
            "gpt-6-astra",
            "route-oc/deepseek-flash",
        ]
    );
    for slug in ["gpt-6-astra", "gpt-5.6-sol", "gpt-5.6-luna"] {
        let model = models.iter().find(|model| model["slug"] == slug).unwrap();
        assert_ne!(
            model.get("codey_source").and_then(Value::as_str),
            Some("third_party")
        );
        assert!(model_instruction_source(model).is_some());
        assert!(model_has_runtime_description(model));
    }
    let custom = models.last().unwrap();
    assert_eq!(custom["slug"], "route-oc/deepseek-flash");
    assert_eq!(custom["codey_source"], "third_party");
    assert!(model_instruction_source(custom).is_some());
}

#[test]
fn mixed_catalog_keeps_selected_official_slugs_missing_from_upstream() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let upstream = vec!["route-oc/deepseek-flash".into()];
    let selected = vec![
        "gpt-5.6-sol".into(),
        "gpt-5.6-luna".into(),
        "gpt-6-astra".into(),
        "route-oc/deepseek-flash".into(),
    ];

    assert_eq!(
        refresh_for_provider(home.path(), false, Some(&upstream), &selected).unwrap(),
        4
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let models = catalog["models"].as_array().unwrap();
    assert_eq!(
        models
            .iter()
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "gpt-5.6-sol",
            "gpt-5.6-luna",
            "gpt-6-astra",
            "route-oc/deepseek-flash",
        ]
    );
}

#[test]
fn route_aliases_use_matching_official_runtime_metadata_and_sanitize_unknown_models() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let selected = vec![
        "openai/gpt-6-astra".into(),
        "openai/gpt-5.6-sol".into(),
        "openai/gpt-5.5".into(),
        "provider/custom-model".into(),
        "gpt-5.6-sol".into(),
        "codey-official-account-test/gpt-5.5".into(),
    ];

    assert_eq!(
        refresh_for_provider(home.path(), false, Some(&selected), &selected).unwrap(),
        6
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let models = catalog["models"].as_array().unwrap();

    let astra = models
        .iter()
        .find(|model| model["slug"] == "openai/gpt-6-astra")
        .unwrap();
    assert_eq!(astra["use_responses_lite"], true);
    assert_eq!(astra["tool_mode"], "code_mode_only");
    assert_eq!(astra["multi_agent_version"], "v2");
    assert_eq!(astra["multi_agent_reasoning_effort"], "xhigh");
    assert_eq!(astra["node_repl_auto_review_required"], true);
    assert_eq!(
        astra["experimental_supported_tools"],
        json!(["send_user_message_async", "clock"])
    );
    assert_eq!(
        reasoning_efforts_from_value(astra),
        ["low", "medium", "high", "xhigh", "max", "ultra"]
    );
    assert_eq!(
        astra["base_instructions"],
        "test-only instructions for gpt-6-astra"
    );

    let gpt_56 = models
        .iter()
        .find(|model| model["slug"] == "openai/gpt-5.6-sol")
        .unwrap();
    assert_eq!(gpt_56["use_responses_lite"], true);
    assert_eq!(gpt_56["tool_mode"], "code_mode_only");
    assert!(gpt_56.get("comp_hash").is_none());
    assert_eq!(gpt_56["default_service_tier"], "priority");
    assert_eq!(gpt_56["prefer_websockets"], true);
    assert_eq!(gpt_56["include_skills_usage_instructions"], false);
    assert_eq!(
        gpt_56["experimental_supported_tools"],
        json!(["gpt-5.6-only-tool"])
    );
    assert_eq!(
        reasoning_efforts_from_value(gpt_56),
        ["low", "medium", "high", "xhigh", "max", "ultra"]
    );
    assert_eq!(gpt_56["multi_agent_version"], "v2");
    assert_eq!(
        gpt_56["base_instructions"],
        "test-only instructions for gpt-5.6-sol"
    );

    let gpt_55 = models
        .iter()
        .find(|model| model["slug"] == "openai/gpt-5.5")
        .unwrap();
    assert_eq!(gpt_55["use_responses_lite"], false);
    assert!(gpt_55.get("tool_mode").is_none());
    assert!(gpt_55.get("comp_hash").is_none());
    assert_eq!(gpt_55["include_skills_usage_instructions"], true);
    assert_eq!(gpt_55["experimental_supported_tools"], json!([]));
    assert_eq!(
        reasoning_efforts_from_value(gpt_55),
        ["low", "medium", "high", "xhigh"]
    );
    assert!(gpt_55.get("multi_agent_version").is_none());
    assert_eq!(
        gpt_55["base_instructions"],
        "test-only instructions for gpt-5.5"
    );

    let custom = models
        .iter()
        .find(|model| model["slug"] == "provider/custom-model")
        .unwrap();
    assert_eq!(custom["use_responses_lite"], false);
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
        "supports_search_tool",
        "web_search_tool_type",
    ] {
        assert!(
            custom.get(field).is_none(),
            "unknown model inherited model-specific field {field}"
        );
    }
    assert_eq!(custom["include_skills_usage_instructions"], true);
    assert_eq!(custom["include_plugin_usage_instructions"], true);
    assert_eq!(custom["include_apps_usage_instructions"], true);
    assert_eq!(custom["experimental_supported_tools"], json!([]));
    assert_eq!(
        reasoning_efforts_from_value(custom),
        ["low", "medium", "high", "xhigh"]
    );
    assert!(custom["auto_compact_token_limit"].is_null());

    for (slug, comp_hash) in [
        ("gpt-5.6-sol", "3000"),
        ("codey-official-account-test/gpt-5.5", "2911"),
    ] {
        let official = models.iter().find(|model| model["slug"] == slug).unwrap();
        assert_eq!(official["comp_hash"], comp_hash);
    }
}

#[test]
fn cached_catalog_removes_third_party_compaction_identity_and_is_idempotent() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let selected = vec![
        "route-a/gpt-5.6-sol".to_string(),
        "route-b/gpt-5.5".to_string(),
        "route-c/vendor/custom-model".to_string(),
        "gpt-5.6-sol".to_string(),
        "codey-official-account-test/gpt-5.5".to_string(),
    ];
    refresh_for_provider_with_capabilities(
        home.path(),
        false,
        Some(&selected),
        &selected,
        CapabilityLists {
            image_detail_original_models: Some(&[]),
            remote_compaction_models: Some(&selected[..1]),
            ..CapabilityLists::default()
        },
        "",
    )
    .unwrap();
    let mut stale = read_runtime_catalog_models(home.path()).unwrap();
    for (index, model) in stale.iter_mut().enumerate() {
        model["comp_hash"] = json!(format!("legacy-{index}"));
        // Older catalogs can mislabel both raw and derived official entries.
        model["codey_source"] = json!("third_party");
    }
    write_catalog(home.path(), &stale).unwrap();
    let mut expected = stale;
    for model in &mut expected[..3] {
        model.as_object_mut().unwrap().remove("comp_hash");
    }
    assert!(
        prepare_cached_catalog_for_current_capabilities(home.path(), &[], &[], &selected[..1], "")
            .unwrap()
    );
    let actual = read_runtime_catalog_models(home.path()).unwrap();
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(&expected) {
        for (field, value) in expected.as_object().unwrap() {
            assert_eq!(
                actual.get(field),
                Some(value),
                "{}.{field}",
                expected["slug"]
            );
        }
        assert_eq!(
            actual.as_object().unwrap().len(),
            expected.as_object().unwrap().len()
        );
    }
    let path = home.path().join(relative_path());
    let first = fs::read(&path).unwrap();
    assert!(
        prepare_cached_catalog_for_current_capabilities(home.path(), &[], &[], &selected[..1], "")
            .unwrap()
    );
    assert_eq!(fs::read(path).unwrap(), first);
}

#[test]
fn catalog_overrides_do_not_restore_third_party_compaction_identity() {
    let mut models = vec![json!({
        "slug": "route/gpt-5.6-sol",
        "codey_source": "third_party",
        "comp_hash": "3000",
        "context_window": 272_000,
        "auto_compact_token_limit": 240_000,
        "codey_context_source": "official_catalog",
        "codey_remote_compaction": true,
        "supported_reasoning_levels": [{"effort": "high"}],
        "default_reasoning_level": "high"
    })];
    let contexts = BTreeMap::from([(
        "route/gpt-5.6-sol".into(),
        crate::config::ModelContextConfig {
            context_window_tokens: 128_000,
            auto_compact_token_limit: Some(100_000),
            reserve_output_tokens: Some(16_000),
        },
    )]);
    let empty_contexts = BTreeMap::new();
    let empty_efforts = BTreeMap::new();
    let display_names = BTreeMap::from([("route/gpt-5.6-sol".into(), "Route Sol".into())]);
    let overrides = CatalogOverrides {
        plugin_contexts: &empty_contexts,
        contexts: &contexts,
        reasoning_efforts: &empty_efforts,
        display_names: &display_names,
    };
    apply_overrides_to_models(&mut models, overrides).unwrap();
    assert!(models[0].get("comp_hash").is_none());
    assert_eq!(models[0]["context_window"], 128_000);
    assert_eq!(models[0]["auto_compact_token_limit"], 100_000);
    assert_eq!(models[0]["codey_remote_compaction"], true);
    assert_eq!(models[0]["display_name"], "Route Sol");
    apply_overrides_to_models(
        &mut models,
        CatalogOverrides {
            contexts: &empty_contexts,
            ..overrides
        },
    )
    .unwrap();
    assert!(models[0].get("comp_hash").is_none());
    assert_eq!(models[0]["context_window"], 272_000);
    assert_eq!(models[0]["auto_compact_token_limit"], 240_000);
    assert_eq!(models[0]["codey_remote_compaction"], true);
}

#[test]
fn cached_catalog_migrates_the_legacy_1m_context_window() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let selected = vec!["route/gpt-5.6-sol".to_string(), "route/gpt-5.5".to_string()];

    refresh_for_provider_with_capabilities(
        home.path(),
        false,
        Some(&selected),
        &selected,
        CapabilityLists::default(),
        "",
    )
    .unwrap();
    let path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
    let mut catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for model in catalog["models"].as_array_mut().unwrap() {
        model["context_window"] = json!(LEGACY_1M_CONTEXT_WINDOW);
        model["max_context_window"] = json!(LEGACY_1M_CONTEXT_WINDOW);
        model["effective_context_window_percent"] = json!(100);
        model["auto_compact_token_limit"] = Value::Null;
        model["codey_context_source"] = json!("legacy_1m");
        model.as_object_mut().unwrap().remove("codey_context_base");
    }
    fs::write(&path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();

    assert!(
        prepare_cached_catalog_for_current_capabilities(home.path(), &[], &[], &[], "").unwrap(),
        "the legacy window must be rewritten"
    );
    let catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for model in catalog["models"].as_array().unwrap() {
        assert_eq!(model["context_window"], DEFAULT_CONTEXT_WINDOW);
        assert_eq!(model["max_context_window"], DEFAULT_CONTEXT_WINDOW);
        assert_eq!(
            model["effective_context_window_percent"],
            DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT
        );
        assert!(model["auto_compact_token_limit"].is_null());
    }
}

#[test]
fn image_detail_original_is_gated_by_route_protocol() {
    let home = tempfile::tempdir().unwrap();
    write_cache_with_image_detail_original(home.path());
    let selected = vec![
        "route-native/gpt-5.6-sol".to_string(),
        "route-chat/gpt-5.6-sol".to_string(),
        "route-anthropic/gpt-5.6-sol".to_string(),
    ];
    let image_detail_original_models = vec!["route-native/gpt-5.6-sol".to_string()];

    assert_eq!(
        refresh_for_provider_with_capabilities(
            home.path(),
            false,
            Some(&selected),
            &selected,
            CapabilityLists {
                image_detail_original_models: Some(&image_detail_original_models),
                ..CapabilityLists::default()
            },
            "",
        )
        .unwrap(),
        selected.len()
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let models = catalog["models"].as_array().unwrap();
    let native = models
        .iter()
        .find(|model| model["slug"] == "route-native/gpt-5.6-sol")
        .unwrap();
    assert_eq!(native["supports_image_detail_original"], true);
    for slug in ["route-chat/gpt-5.6-sol", "route-anthropic/gpt-5.6-sol"] {
        let adapted = models.iter().find(|model| model["slug"] == slug).unwrap();
        assert!(
            adapted.get("supports_image_detail_original").is_none(),
            "{slug} 声明的能力必须随线路协议移除"
        );
    }
}

#[test]
fn remote_compaction_catalog_and_cached_refresh_keep_routes_independent() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let selected = vec![
        "route-a/gpt-5.6-sol".to_string(),
        "route-b/gpt-5.6-sol".to_string(),
    ];
    refresh_for_provider_with_capabilities(
        home.path(),
        false,
        Some(&selected),
        &selected,
        CapabilityLists {
            remote_compaction_models: Some(&selected[..1]),
            ..CapabilityLists::default()
        },
        "",
    )
    .unwrap();
    let path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
    assert_eq!(
        remote_compaction_models_from_catalog(&path),
        HashSet::from([selected[0].clone()])
    );
    assert!(
        prepare_cached_catalog_for_current_capabilities(home.path(), &[], &[], &selected[1..], "")
            .unwrap()
    );
    assert_eq!(
        remote_compaction_models_from_catalog(&path),
        HashSet::from([selected[1].clone()])
    );
    let models = read_runtime_catalog_models(home.path()).unwrap();
    assert_eq!(models.len(), 2);
    assert!(
        models
            .iter()
            .all(|model| model["base_instructions"].as_str().is_some())
    );
}

#[test]
fn cached_catalog_fallback_removes_stale_image_detail_original_metadata() {
    let home = tempfile::tempdir().unwrap();
    write_cache_with_image_detail_original(home.path());
    let selected = vec!["route-chat/gpt-5.6-sol".to_string()];

    refresh_for_provider_with_capabilities(
        home.path(),
        false,
        Some(&selected),
        &selected,
        CapabilityLists {
            image_detail_original_models: Some(&selected),
            ..CapabilityLists::default()
        },
        "",
    )
    .unwrap();
    let path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
    let stale: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(stale["models"][0]["supports_image_detail_original"], true);

    assert!(
        prepare_cached_catalog_for_current_capabilities(home.path(), &[], &[], &[], "").unwrap()
    );
    let sanitized: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(
        sanitized["models"][0]
            .get("supports_image_detail_original")
            .is_none()
    );
}

#[test]
fn model_reasoning_effort_override_applies_and_restores_the_template() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let selected = vec!["route/gpt-5.6-sol".to_string()];
    refresh_for_provider_with_capabilities(
        home.path(),
        false,
        Some(&selected),
        &selected,
        CapabilityLists::default(),
        "",
    )
    .unwrap();
    let path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
    let find_model = |catalog: &Value| {
        catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["slug"] == "route/gpt-5.6-sol")
            .unwrap()
            .clone()
    };
    let catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let baseline = find_model(&catalog);

    let overrides = std::collections::BTreeMap::from([(
        "route/gpt-5.6-sol".to_string(),
        vec![
            crate::config::ModelReasoningEffort {
                level: "low".into(),
                value: "low".into(),
            },
            crate::config::ModelReasoningEffort {
                level: "high".into(),
                value: "high".into(),
            },
        ],
    )]);
    apply_catalog_reasoning_efforts(home.path(), &overrides).unwrap();
    let catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let declared = find_model(&catalog);
    assert_eq!(reasoning_efforts_from_value(&declared), ["low", "high"]);
    assert_eq!(declared["default_reasoning_level"], "low");
    assert_eq!(declared["supports_reasoning_summaries"], true);
    assert!(declared.get(REASONING_BASE_FIELD).is_some());

    apply_catalog_reasoning_efforts(home.path(), &std::collections::BTreeMap::new()).unwrap();
    let catalog: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let restored = find_model(&catalog);
    assert_eq!(
        restored.get("supported_reasoning_levels"),
        baseline.get("supported_reasoning_levels")
    );
    assert_eq!(
        restored.get("default_reasoning_level"),
        baseline.get("default_reasoning_level")
    );
    assert!(restored.get(REASONING_BASE_FIELD).is_none());
}

#[test]
fn plugin_reasoning_capability_stays_bounded_after_template_metadata() {
    let mut model = json!({
        "slug": "plugin/gpt-6-astra",
        "supported_reasoning_levels": [
            {"effort": "low"},
            {"effort": "medium"},
            {"effort": "high"},
            {"effort": "xhigh"},
            {"effort": "max"},
            {"effort": "ultra"}
        ],
        "default_reasoning_level": "ultra"
    });
    let declaration = vec![
        crate::config::ModelReasoningEffort {
            level: "low".into(),
            value: "low".into(),
        },
        crate::config::ModelReasoningEffort {
            level: "medium".into(),
            value: "medium".into(),
        },
        crate::config::ModelReasoningEffort {
            level: "high".into(),
            value: "high".into(),
        },
        crate::config::ModelReasoningEffort {
            level: "xhigh".into(),
            value: "xhigh".into(),
        },
    ];
    apply_model_reasoning_efforts(&mut model, Some(&declaration));
    assert_eq!(
        reasoning_efforts_from_value(&model),
        ["low", "medium", "high", "xhigh"]
    );
    assert_eq!(model["default_reasoning_level"], "low");
}

#[test]
fn websocket_preference_is_isolated_per_route_model_alias() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let selected = vec![
        "route-ws/gpt-5.6-sol".to_string(),
        "route-http/gpt-5.6-sol".to_string(),
    ];
    let websocket_models = vec!["route-ws/gpt-5.6-sol".to_string()];

    refresh_for_provider_with_websocket_models(
        home.path(),
        false,
        Some(&selected),
        &selected,
        &websocket_models,
    )
    .unwrap();
    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let models = catalog["models"].as_array().unwrap();
    let websocket = models
        .iter()
        .find(|model| model["slug"] == "route-ws/gpt-5.6-sol")
        .unwrap();
    let http = models
        .iter()
        .find(|model| model["slug"] == "route-http/gpt-5.6-sol")
        .unwrap();

    assert_eq!(websocket["prefer_websockets"], true);
    assert_eq!(http["prefer_websockets"], false);
}

#[test]
fn native_web_search_is_gated_by_route_and_source_model_metadata() {
    let home = tempfile::tempdir().unwrap();
    write_cache_with_native_web_search(home.path());
    let selected = vec![
        "route-search/gpt-5.6-sol".to_string(),
        "route-off/gpt-5.6-sol".to_string(),
        "route-search/claude-opus-5".to_string(),
    ];
    let native_web_search_models = vec![
        "route-search/gpt-5.6-sol".to_string(),
        "route-search/claude-opus-5".to_string(),
    ];

    refresh_for_provider_with_capabilities(
        home.path(),
        false,
        Some(&selected),
        &selected,
        CapabilityLists {
            native_web_search_models: Some(&native_web_search_models),
            ..CapabilityLists::default()
        },
        "",
    )
    .unwrap();
    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let models = catalog["models"].as_array().unwrap();
    let search = models
        .iter()
        .find(|model| model["slug"] == "route-search/gpt-5.6-sol")
        .unwrap();
    let disabled = models
        .iter()
        .find(|model| model["slug"] == "route-off/gpt-5.6-sol")
        .unwrap();
    let unknown = models
        .iter()
        .find(|model| model["slug"] == "route-search/claude-opus-5")
        .unwrap();

    assert_eq!(search["supports_search_tool"], true);
    assert!(search["web_search_tool_type"].is_string());
    for model in [disabled, unknown] {
        assert!(model.get("supports_search_tool").is_none());
        assert!(model.get("web_search_tool_type").is_none());
    }
}

#[test]
fn cached_catalog_fallback_removes_stale_capability_metadata() {
    let home = tempfile::tempdir().unwrap();
    write_cache_with_native_web_search(home.path());
    let selected = vec!["route-search/gpt-5.6-sol".to_string()];

    refresh_for_provider_with_capabilities(
        home.path(),
        false,
        Some(&selected),
        &selected,
        CapabilityLists {
            native_web_search_models: Some(&selected),
            ..CapabilityLists::default()
        },
        "",
    )
    .unwrap();
    let path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
    let mut stale: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(stale["models"][0]["supports_search_tool"], true);
    assert!(stale["models"][0]["web_search_tool_type"].is_string());
    stale["models"][0]["context_window"] = json!(LEGACY_1M_CONTEXT_WINDOW);
    stale["models"][0]["max_context_window"] = json!(LEGACY_1M_CONTEXT_WINDOW);
    stale["models"][0]["effective_context_window_percent"] = json!(100);
    stale["models"][0]["auto_compact_token_limit"] = Value::Null;
    stale["models"][0]["codey_context_source"] = json!("legacy_1m");
    stale["models"][0]
        .as_object_mut()
        .unwrap()
        .remove("codey_context_base");
    stale["models"][0]
        .as_object_mut()
        .unwrap()
        .remove("codey_source");
    fs::write(&path, serde_json::to_vec_pretty(&stale).unwrap()).unwrap();

    assert!(
        prepare_cached_catalog_for_current_capabilities(home.path(), &[], &[], &[], "").unwrap(),
        "a valid cached catalog should remain usable after stale capabilities are removed"
    );
    let sanitized: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(sanitized["models"][0].get("supports_search_tool").is_none());
    assert!(sanitized["models"][0].get("web_search_tool_type").is_none());
    assert_eq!(
        sanitized["models"][0]["context_window"],
        DEFAULT_CONTEXT_WINDOW
    );
    let sanitized_bytes = fs::read(&path).unwrap();

    assert!(
        prepare_cached_catalog_for_current_capabilities(home.path(), &[], &[], &[], "").unwrap()
    );
    assert_eq!(fs::read(&path).unwrap(), sanitized_bytes);
}

#[test]
fn third_party_catalog_does_not_inherit_a_high_only_template() {
    let home = tempfile::tempdir().unwrap();
    write_cache_with_sol_reasoning_metadata(
        home.path(),
        Some(json!([{"effort": "high"}])),
        Some("high"),
    );
    let selected = vec!["provider-fast-coder".into()];

    assert_eq!(
        refresh_for_provider(home.path(), false, Some(&selected), &selected,).unwrap(),
        1
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let model = &catalog["models"][0];
    assert_eq!(model["slug"], "provider-fast-coder");
    assert_eq!(
        reasoning_efforts_from_value(model),
        ["low", "medium", "high", "xhigh"]
    );
    assert_eq!(
        model["default_reasoning_level"],
        THIRD_PARTY_DEFAULT_REASONING_EFFORT
    );
    for level in model["supported_reasoning_levels"].as_array().unwrap() {
        assert!(
            level_has_runtime_description(level),
            "{} level lacks the runtime-required description",
            level["effort"]
        );
    }
}

#[test]
fn configured_provider_model_survives_a_missing_upstream_snapshot() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let selected = vec!["provider-fast-coder".into()];

    assert_eq!(
        refresh_for_provider(home.path(), false, None, &selected,).unwrap(),
        OFFICIAL_MODELS.len() + 1
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    let models = catalog["models"].as_array().unwrap();
    // 已配置的模型排在最前，未同步时补入的官方模型保持固定顺序跟在后面。
    let model = models.first().unwrap();
    assert_eq!(model["slug"], "provider-fast-coder");
    assert_eq!(
        models[1..]
            .iter()
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>(),
        OFFICIAL_MODELS
            .iter()
            .map(|(slug, _)| *slug)
            .collect::<Vec<_>>()
    );
    assert_eq!(model["codey_source"], "third_party");
    assert_eq!(model["visibility"], "list");
    assert_eq!(model["supported_in_api"], true);
    assert_native_fast(model);
    assert!(
        !model["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn prompt_free_cold_start_falls_back_instead_of_writing_an_invalid_catalog() {
    let home = tempfile::tempdir().unwrap();

    let error = refresh_for_provider(home.path(), true, None, &[]).unwrap_err();

    assert!(error.to_string().contains("模型缓存缺少运行时必需字段"));
    assert!(is_runtime_model_cache_unavailable(&error));
    assert!(!home.path().join(MODEL_CATALOG_RELATIVE_PATH).exists());
    assert!(!is_available(home.path()));
    let state = selection_state(home.path(), true, None, &[], None).unwrap();
    assert_eq!(state.official_models.len(), OFFICIAL_MODELS.len());
    assert_eq!(state.available_model("gpt-6-astra"), Some("gpt-6-astra"));
}

#[test]
fn astra_selection_and_route_metadata_follow_the_native_reasoning_levels() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let selected = vec!["gpt-6-astra".into()];
    let expected_efforts = ["low", "medium", "high", "xhigh", "max", "ultra"];

    let state = selection_state(home.path(), true, None, &selected, Some("GPT-6-ASTRA")).unwrap();
    assert_eq!(state.default_model, "gpt-6-astra");
    assert_eq!(
        state.official_models[0].supported_reasoning_efforts,
        expected_efforts
    );
    assert_eq!(state.official_models[0].default_reasoning_effort, "medium");
    refresh_for_provider(home.path(), true, None, &selected).unwrap();
    let catalog = read_catalog_value(&home.path().join(relative_path())).unwrap();
    assert_eq!(
        catalog["models"][0]["service_tiers"][0]["description"],
        "2x speed, increased usage"
    );

    let route = vec!["relay/GPT-6-ASTRA".into()];
    let state = selection_state(home.path(), false, Some(&route), &route, None).unwrap();
    let metadata = state
        .third_party_model_metadata
        .iter()
        .find(|model| model.slug == route[0])
        .unwrap();
    assert_eq!(metadata.supported_reasoning_efforts, expected_efforts);

    let mut cache = official_cache();
    let astra = cache["models"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|model| model["slug"] == "gpt-6-astra")
        .unwrap();
    astra["supported_reasoning_levels"]
        .as_array_mut()
        .unwrap()
        .retain(|level| level["effort"] != "ultra");
    fs::write(
        home.path().join("models_cache.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();

    refresh_for_provider(home.path(), false, Some(&route), &route).unwrap();
    let catalog = read_catalog_value(&home.path().join(relative_path())).unwrap();
    let astra = &catalog["models"][0];
    assert_eq!(
        reasoning_efforts_from_value(astra),
        ["low", "medium", "high", "xhigh", "max"]
    );
    assert!(astra.get("multi_agent_version").is_none());
    assert!(astra.get("multi_agent_reasoning_effort").is_none());
}

#[test]
fn luna_forwards_ultra_even_when_the_template_stops_at_max() {
    let luna = json!({
        "slug": "gpt-6-luna",
        "default_reasoning_level": "medium",
        "base_instructions": "luna instructions",
        "supported_reasoning_levels": [
            {"effort": "low"}, {"effort": "medium"}, {"effort": "high"},
            {"effort": "xhigh"}, {"effort": "max"}
        ]
    });
    assert_eq!(
        third_party_reasoning_efforts_from_value(&luna),
        ["low", "medium", "high", "xhigh", "max", "ultra"]
    );
    assert!(!third_party_template_supports_coordination(&luna));

    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
    fs::write(
        home.path().join(DEBUG_CATALOG_RELATIVE_PATH),
        serde_json::to_vec(&json!({
            "codey_account_snapshot": true,
        "codey_runtime_source": catalog_source_for_app(""),
            "models": [luna.clone()]
        }))
        .unwrap(),
    )
    .unwrap();
    let route =
        vec!["codey-official-account-67fa10d5-0cf6-4653-ad8b-34664dde569e/gpt-6-luna".to_string()];
    refresh_for_provider(home.path(), false, Some(&route), &route).unwrap();
    let catalog = read_catalog_value(&home.path().join(relative_path())).unwrap();
    let written = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| {
            model["slug"]
                .as_str()
                .is_some_and(|slug| slug.ends_with("/gpt-6-luna"))
        })
        .unwrap();
    assert_eq!(
        reasoning_efforts_from_value(written),
        ["low", "medium", "high", "xhigh", "max", "ultra"]
    );

    let state = selection_state(home.path(), true, None, &["gpt-6-luna".into()], None).unwrap();
    let metadata = state
        .official_models
        .iter()
        .find(|model| model.slug == "gpt-6-luna")
        .unwrap();
    assert!(
        metadata
            .supported_reasoning_efforts
            .iter()
            .any(|effort| effort == "ultra")
    );
    assert_eq!(metadata.default_reasoning_effort, "medium");
}

#[test]
fn cache_without_astra_keeps_existing_routes_and_requires_its_own_runtime_template() {
    let home = tempfile::tempdir().unwrap();
    let mut cache = official_cache();
    cache["models"]
        .as_array_mut()
        .unwrap()
        .retain(|model| model["slug"] != "gpt-6-astra");
    fs::write(
        home.path().join("models_cache.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();

    for (official, model) in [
        (true, "gpt-5.6-sol"),
        (false, "relay/gpt-5.6-sol"),
        (false, "provider/custom-model"),
    ] {
        let selected = vec![model.into()];
        assert_eq!(
            refresh_for_provider(home.path(), official, Some(&selected), &selected).unwrap(),
            1
        );
    }

    let path = home.path().join(relative_path());
    let previous = fs::read(&path).unwrap();
    for (official, model) in [(true, "gpt-6-astra"), (false, "relay/gpt-6-astra")] {
        let selected = vec![model.into()];
        let error =
            refresh_for_provider(home.path(), official, Some(&selected), &selected).unwrap_err();
        assert!(is_runtime_model_cache_unavailable(&error));
        assert_eq!(fs::read(&path).unwrap(), previous);
    }
}

#[test]
fn mixed_catalog_preserves_official_account_alias_identity() {
    let home = tempfile::tempdir().unwrap();
    let mut cache = official_cache();
    let source = cache["models"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|model| model["slug"] == "gpt-5.6-sol")
        .unwrap();
    source["description"] = json!("Official account model description");
    let expected_instructions = model_instruction_source(source).unwrap().to_string();
    fs::write(
        home.path().join("models_cache.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();

    let first = crate::config::official_profile_id("acct-one");
    let second = crate::config::official_profile_id("acct-two");
    let selected = vec![
        format!("{first}/gpt-5.6-sol"),
        format!("{second}/gpt-5.6-sol"),
        "relay/gpt-5.6-sol".to_string(),
    ];
    assert_eq!(
        refresh_for_provider(home.path(), false, Some(&selected), &selected).unwrap(),
        3
    );
    let catalog = read_catalog_value(&home.path().join(relative_path())).unwrap();
    let models = catalog["models"].as_array().unwrap();
    assert_eq!(
        models
            .iter()
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>(),
        selected.iter().map(String::as_str).collect::<Vec<_>>()
    );
    for model in &models[..2] {
        assert_eq!(model["description"], "Official account model description");
        assert!(model.get("codey_source").is_none());
    }
    let third_party = &models[2];
    assert_eq!(third_party["description"], "Third-party API model");
    assert_eq!(third_party["codey_source"], "third_party");
    for model in models {
        assert_eq!(
            model_instruction_source(model),
            Some(expected_instructions.as_str())
        );
    }
}

#[test]
fn official_route_aliases_without_a_local_runtime_template_are_dropped() {
    let home = tempfile::tempdir().unwrap();
    // One official model has no runtime instructions in this machine's
    // sources, and an older config still selects a slug upstream retired.
    let mut cache = official_cache();
    let prompt_free = cache["models"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|model| model["slug"] == "gpt-5.5")
        .unwrap()
        .as_object_mut()
        .unwrap();
    prompt_free.remove("base_instructions");
    prompt_free.remove("model_messages");
    fs::write(
        home.path().join("models_cache.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();

    let first = crate::config::official_profile_id("acct-one");
    let second = crate::config::official_profile_id("acct-two");
    let selected = vec![
        format!("{first}/gpt-5.5"),
        format!("{first}/gpt-5.4"),
        format!("{second}/gpt-5.6-sol"),
    ];

    assert_eq!(
        refresh_for_provider(home.path(), false, Some(&selected), &selected).unwrap(),
        1
    );
    let catalog = read_catalog_value(&home.path().join(relative_path())).unwrap();
    assert_eq!(catalog["models"].as_array().unwrap().len(), 1);
    assert_eq!(catalog["models"][0]["slug"], selected[2].as_str());
    assert!(is_available(home.path()));

    // The same upstream name on a user-declared third-party route is not
    // affected by the retirement, so it keeps the generic template.
    let third_party = vec!["relay/gpt-5.4".to_string()];
    assert_eq!(
        refresh_for_provider(home.path(), false, Some(&third_party), &third_party).unwrap(),
        1
    );
    let catalog = read_catalog_value(&home.path().join(relative_path())).unwrap();
    assert_eq!(catalog["models"][0]["slug"], "relay/gpt-5.4");
    assert!(model_instruction_source(&catalog["models"][0]).is_some());
    assert!(model_has_runtime_description(&catalog["models"][0]));
    assert!(is_available(home.path()));
}

#[test]
fn prompt_free_existing_catalog_is_not_reused_as_a_runtime_fallback() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        serde_json::to_vec(&codey_runtime_core::model_suffix::bundled_model_catalog().unwrap())
            .unwrap(),
    )
    .unwrap();

    assert!(!is_available(home.path()));
}

#[test]
fn existing_catalog_without_description_is_not_reused_as_a_runtime_fallback() {
    let home = tempfile::tempdir().unwrap();
    let mut catalog = official_cache();
    for model in catalog["models"].as_array_mut().unwrap() {
        model["description"] = json!(model["display_name"].as_str().unwrap_or("Model"));
    }
    catalog["models"][0]
        .as_object_mut()
        .unwrap()
        .remove("description");
    let path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, serde_json::to_vec(&catalog).unwrap()).unwrap();

    assert!(!is_available(home.path()));
}

#[test]
fn cold_start_replaces_stale_codey_fast_metadata() {
    let home = tempfile::tempdir().unwrap();
    let mut stale_catalog = official_cache();
    stale_catalog["codey_runtime_source"] = json!(catalog_source_for_app(""));
    for model in stale_catalog["models"].as_array_mut().unwrap() {
        add_fast_speed_controls(model);
    }
    let catalog_path = home.path().join(MODEL_CATALOG_RELATIVE_PATH);
    fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
    fs::write(&catalog_path, serde_json::to_vec(&stale_catalog).unwrap()).unwrap();

    refresh_for_provider(home.path(), true, None, &[]).unwrap();

    let catalog: Value = serde_json::from_slice(&fs::read(catalog_path).unwrap()).unwrap();
    let models = catalog["models"].as_array().unwrap();
    // The stale catalog claimed native fast support for every slug; the
    // regenerated one must rebuild that metadata from the bundled catalog
    // and drop the retired slug entirely.
    assert_eq!(
        models
            .iter()
            .filter(|model| declares_fast_speed_support(model))
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "gpt-6-astra",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
        ]
    );
    assert!(
        !models
            .iter()
            .any(|model| model["slug"] == "gpt-5.3-codex-spark")
    );
}

#[test]
fn synced_empty_provider_catalog_hides_every_model() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let upstream = Vec::<String>::new();
    assert_eq!(
        refresh_for_provider(home.path(), false, Some(&upstream), &[],).unwrap(),
        0
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    assert_eq!(catalog["models"].as_array().unwrap().len(), 0);
}

#[test]
fn unsynced_third_party_provider_does_not_invent_official_models() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());

    let state = selection_state(home.path(), false, None, &[], None).unwrap();

    assert!(state.official_models.is_empty());
    assert!(state.third_party_models.is_empty());
    assert!(state.default_model.is_empty());
}

#[test]
fn synced_third_party_provider_keeps_official_looking_ids_route_scoped() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let upstream = vec!["gpt-5.6-sol".into(), "third-model".into()];
    let state = selection_state_with_manual_models(
        home.path(),
        false,
        Some(&upstream),
        &["gpt-5.6-sol".into(), "third-model".into()],
        &["third-model".into()],
        None,
        None,
        "",
    )
    .unwrap();

    assert!(state.official_models.is_empty());
    assert_eq!(state.third_party_models, ["gpt-5.6-sol", "third-model"]);
    let sol_metadata = state
        .third_party_model_metadata
        .iter()
        .find(|model| model.slug == "gpt-5.6-sol")
        .unwrap();
    assert_eq!(
        sol_metadata.supported_reasoning_efforts,
        ["low", "medium", "high", "xhigh", "max", "ultra"]
    );
    let custom_metadata = state
        .third_party_model_metadata
        .iter()
        .find(|model| model.slug == "third-model")
        .unwrap();
    assert_eq!(
        custom_metadata.supported_reasoning_efforts,
        ["low", "medium", "high", "xhigh"]
    );
    assert_eq!(state.manual_third_party_models, ["third-model"]);
    assert_eq!(state.default_model, "gpt-5.6-sol");
}

#[test]
fn synced_empty_provider_marks_official_models_and_configured_models_unavailable() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let upstream = Vec::new();

    let state = selection_state(
        home.path(),
        false,
        Some(&upstream),
        &["provider-fast-coder".into()],
        None,
    )
    .unwrap();

    assert!(state.official_models.is_empty());
    assert!(state.third_party_models.is_empty());
    assert!(state.upstream_models.is_empty());
    assert!(state.default_model.is_empty());
}

#[test]
fn selection_state_does_not_enable_unselected_upstream_models() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let upstream = vec!["gpt-5.4".into(), "codex-auto-review".into()];
    let state = selection_state(home.path(), false, Some(&upstream), &[], None).unwrap();

    assert!(state.official_models.is_empty());
    assert!(state.third_party_models.is_empty());
    assert!(state.default_model.is_empty());
}

#[test]
fn synced_provider_keeps_selected_spark_as_a_route_model() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let upstream = vec!["gpt-5.3-codex-spark".into()];

    let state = selection_state(
        home.path(),
        false,
        Some(&upstream),
        &["gpt-5.3-codex-spark".into()],
        None,
    )
    .unwrap();

    assert!(state.official_models.is_empty());
    assert_eq!(state.third_party_models, ["gpt-5.3-codex-spark"]);
    let spark_metadata = state
        .third_party_model_metadata
        .iter()
        .find(|model| model.slug == "gpt-5.3-codex-spark")
        .unwrap();
    assert_eq!(
        spark_metadata.supported_reasoning_efforts,
        ["low", "medium", "high", "xhigh"]
    );
    let sol_metadata = state
        .third_party_model_metadata
        .iter()
        .find(|model| model.slug == "gpt-5.6-sol")
        .unwrap();
    assert_eq!(
        sol_metadata.supported_reasoning_efforts,
        ["low", "medium", "high", "xhigh", "max", "ultra"]
    );
    assert_eq!(state.default_model, "gpt-5.3-codex-spark");
}

#[test]
fn selection_state_uses_requested_default_when_available() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let upstream = vec![
        "GPT-5.6-SOL".into(),
        "gpt-5.6-luna".into(),
        "Third-Model".into(),
    ];
    let state = selection_state(
        home.path(),
        false,
        Some(&upstream),
        &[
            "third-model".into(),
            "THIRD-MODEL".into(),
            "gpt-5.6-luna".into(),
        ],
        Some("THIRD-MODEL"),
    )
    .unwrap();

    assert_eq!(state.default_model, "third-model");
    assert_eq!(
        state.available_model(" GPT-5.6-LUNA "),
        Some("gpt-5.6-luna")
    );
    assert_eq!(state.available_model("third-model"), Some("third-model"));
    assert_eq!(state.available_model("gpt-5.4"), None);
    assert!(state.official_models.is_empty());
}

#[test]
fn official_selection_marks_only_enabled_models_as_supported() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let selected = vec!["gpt-5.6-sol".into()];

    let state = selection_state(home.path(), true, None, &selected, Some("gpt-5.6-luna")).unwrap();

    assert_eq!(state.default_model, "gpt-5.6-sol");
    assert!(
        state
            .official_models
            .iter()
            .find(|model| model.slug == "gpt-5.6-sol")
            .is_some_and(|model| model.supported)
    );
    assert!(
        state
            .official_models
            .iter()
            .filter(|model| model.slug != "gpt-5.6-sol")
            .all(|model| !model.supported)
    );
}

#[test]
fn synced_official_account_keeps_unchecked_models_disabled() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let upstream = vec![
        "gpt-5.6-sol".into(),
        "gpt-5.6-luna".into(),
        "gpt-5.5".into(),
    ];
    let selected = vec!["gpt-5.6-sol".into()];

    let state = selection_state(
        home.path(),
        true,
        Some(&upstream),
        &selected,
        Some("gpt-5.6-luna"),
    )
    .unwrap();

    assert_eq!(state.default_model, "gpt-5.6-sol");
    assert_eq!(
        state
            .official_models
            .iter()
            .filter(|model| model.supported)
            .map(|model| model.slug.as_str())
            .collect::<Vec<_>>(),
        ["gpt-5.6-sol"]
    );
}

#[test]
fn selection_state_falls_back_from_unavailable_default_to_first_route_model() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());
    let upstream = vec!["gpt-5.6-sol".into(), "third-model".into()];
    let state = selection_state(
        home.path(),
        false,
        Some(&upstream),
        &["third-model".into()],
        Some("missing-model"),
    )
    .unwrap();

    assert_eq!(state.default_model, "third-model");
}

#[test]
fn selection_exposes_every_model_available_on_the_current_route() {
    let home = tempfile::tempdir().unwrap();
    fs::write(
        home.path().join("models_cache.json"),
        serde_json::to_vec(&official_cache()).unwrap(),
    )
    .unwrap();
    let upstream = vec![
        "gpt-5.6-sol".into(),
        "gpt-5.6-terra".into(),
        "gpt-5.6-luna".into(),
        "third-model".into(),
    ];

    let state = selection_state(home.path(), false, Some(&upstream), &upstream, None).unwrap();

    for model in &upstream {
        assert!(state.available_model(model).is_some(), "{model}");
    }
    assert_eq!(state.available_model("gpt-5.4"), None);
}

#[test]
fn selection_ignores_a_stale_shared_cache_client_version() {
    let home = tempfile::tempdir().unwrap();
    let mut cache = official_cache();
    cache["client_version"] = json!("0.146.1");
    fs::write(
        home.path().join("models_cache.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();
    let upstream = vec![
        "gpt-5.6-sol".into(),
        "gpt-5.6-luna".into(),
        "third-model".into(),
    ];

    let state = selection_state(
        home.path(),
        false,
        Some(&upstream),
        &["gpt-5.6-luna".into(), "third-model".into()],
        None,
    )
    .unwrap();

    assert_eq!(state.available_model("gpt-5.6-luna"), Some("gpt-5.6-luna"));
    assert_eq!(state.available_model("third-model"), Some("third-model"));
}

#[test]
fn selection_does_not_depend_on_a_generated_runtime_catalog() {
    let home = tempfile::tempdir().unwrap();
    write_cache(home.path());

    let state = selection_state(home.path(), true, None, &[], None).unwrap();

    assert!(!home.path().join(relative_path()).exists());
    for model in &state.official_models {
        assert_eq!(
            state.available_model(&model.slug),
            Some(model.slug.as_str())
        );
    }
    assert!(state.first_available_model().is_some());
}

#[test]
fn catalog_snapshot_restores_existing_content_and_removes_new_content() {
    let existing_home = tempfile::tempdir().unwrap();
    let existing_path = existing_home.path().join(relative_path());
    fs::create_dir_all(existing_path.parent().unwrap()).unwrap();
    fs::write(&existing_path, b"original catalog\n").unwrap();
    let existing_snapshot = snapshot(existing_home.path()).unwrap();
    fs::write(&existing_path, b"replacement catalog\n").unwrap();

    restore_snapshot(existing_snapshot).unwrap();

    assert_eq!(fs::read(&existing_path).unwrap(), b"original catalog\n");

    let new_home = tempfile::tempdir().unwrap();
    let new_path = new_home.path().join(relative_path());
    let new_snapshot = snapshot(new_home.path()).unwrap();
    fs::create_dir_all(new_path.parent().unwrap()).unwrap();
    fs::write(&new_path, b"new catalog\n").unwrap();

    restore_snapshot(new_snapshot).unwrap();

    assert!(!new_path.exists());
}

#[test]
fn runtime_snapshot_is_stale_without_a_snapshot_or_after_a_codex_upgrade() {
    let stale = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
    let fresh = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(200);

    // No local Codex CLI means Codey cannot capture anything, so it must not
    // treat a missing snapshot as work it can do.
    assert!(!runtime_snapshot_is_stale_at(None, None));
    assert!(!runtime_snapshot_is_stale_at(Some(stale), None));
    // A missing snapshot is stale whenever a CLI is available: Codex
    // 26.908+ leaves no `models_cache.json`, so this is the only path that
    // ever creates the first capture.
    assert!(runtime_snapshot_is_stale_at(None, Some(fresh)));
    // A snapshot older than the newest local build is stale; one newer than
    // every candidate is not.
    assert!(runtime_snapshot_is_stale_at(Some(stale), Some(fresh)));
    assert!(!runtime_snapshot_is_stale_at(Some(fresh), Some(stale)));
    assert!(!runtime_snapshot_is_stale_at(Some(fresh), Some(fresh)));
}

#[test]
fn snapshot_sync_slot_is_claimed_once_per_codex_build() {
    let attempted = std::sync::Mutex::new(None);
    let first = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(10);
    let second = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(20);

    assert!(claim_snapshot_sync_slot(&attempted, Some(first)));
    // The same build is not rendered twice in one process.
    assert!(!claim_snapshot_sync_slot(&attempted, Some(first)));
    // A failed render releases the claim, so a restart can retry.
    release_snapshot_sync_slot(&attempted, Some(first));
    assert!(claim_snapshot_sync_slot(&attempted, Some(first)));
    // A Codex upgrade changes the stamp and is rendered again.
    assert!(claim_snapshot_sync_slot(&attempted, Some(second)));
    // A release that does not own the current claim leaves it in place.
    release_snapshot_sync_slot(&attempted, Some(first));
    assert!(!claim_snapshot_sync_slot(&attempted, Some(second)));
}

#[test]
fn codex_cli_stamp_uses_the_newest_candidate_and_ignores_missing_paths() {
    let home = tempfile::tempdir().unwrap();
    let older = home.path().join("older-codex");
    let newer = home.path().join("newer-codex");
    fs::write(&older, b"old").unwrap();
    fs::write(&newer, b"new").unwrap();
    let older_mtime = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
    let newer_mtime = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(200);
    std::fs::File::options()
        .write(true)
        .open(&older)
        .unwrap()
        .set_modified(older_mtime)
        .unwrap();
    std::fs::File::options()
        .write(true)
        .open(&newer)
        .unwrap()
        .set_modified(newer_mtime)
        .unwrap();

    // The newest build decides, whichever order the candidates arrive in,
    // and a path that does not exist never contributes a stamp.
    for candidates in [
        vec![older.clone(), newer.clone()],
        vec![newer.clone(), older.clone()],
    ] {
        assert_eq!(codex_cli_stamp_for(&candidates), Some(newer_mtime));
    }
    assert_eq!(codex_cli_stamp_for(&[]), None);
    assert_eq!(codex_cli_stamp_for(&[home.path().join("absent")]), None);
    assert_eq!(
        codex_cli_stamp_for(&[home.path().join("absent"), newer.clone()]),
        Some(newer_mtime)
    );
}

#[test]
fn snapshot_requires_every_fixed_official_model() {
    let mut cache = official_cache();
    let models = official_models_from_value(&cache);
    assert!(snapshot_covers_official_models(&models));

    // A render missing one fixed slug, or carrying it without runtime
    // fields, must not be persisted as a durable source.
    for slug in OFFICIAL_MODELS.iter().map(|(slug, _)| *slug) {
        let mut partial = models.clone();
        partial.retain(|model| model["slug"] != slug);
        assert!(
            !snapshot_covers_official_models(&partial),
            "{slug} should be required"
        );

        let mut stripped = models.clone();
        let entry = stripped
            .iter_mut()
            .find(|model| model["slug"] == slug)
            .unwrap()
            .as_object_mut()
            .unwrap();
        entry.remove("base_instructions");
        entry.remove("model_messages");
        assert!(
            !snapshot_covers_official_models(&stripped),
            "{slug} needs runtime fields"
        );
    }

    cache["models"] = json!([]);
    assert!(!snapshot_covers_official_models(
        &official_models_from_value(&cache)
    ));
}

#[test]
fn snapshot_accepts_the_real_codex_26_908_render() {
    // The 26.908 CLI renders these eleven slugs on a clean CODEX_HOME. The
    // render must be accepted, so no fixed entry may name a slug the
    // installed Codex no longer publishes: a single retired slug in
    // `OFFICIAL_MODELS` would reject every real render and silently leave
    // the missing cache unrepaired.
    let rendered = [
        "gpt-6-astra",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
    ]
    .into_iter()
    .chain([
        "gpt-daybreak-blue-latest",
        "gpt-daybreak-red-latest",
        "gpt-5.5",
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5.2",
        "codex-auto-review",
    ])
    .map(|slug| {
        json!({
            "slug": slug,
            "display_name": slug,
            "description": format!("{slug} description"),
            "base_instructions": format!("{slug} instructions"),
        })
    })
    .collect::<Vec<_>>();

    assert!(snapshot_covers_official_models(&rendered));
    assert!(
        !rendered
            .iter()
            .any(|model| model["slug"] == "gpt-5.3-codex-spark")
    );
}

#[test]
fn codex_26_908_snapshot_alone_generates_the_catalog() {
    // End-to-end for the bug this change set targets: Codex 26.908 writes
    // no `models_cache.json`, so the captured snapshot is the only
    // instruction-bearing source. A retired slug left in `OFFICIAL_MODELS`
    // would reject the render and force the custom-context recovery dialog
    // on every launch.
    let home = tempfile::tempdir().unwrap();
    let models = [
        "gpt-6-astra",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-daybreak-blue-latest",
        "gpt-daybreak-red-latest",
        "gpt-5.5",
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5.2",
        "codex-auto-review",
    ]
    .into_iter()
    .map(|slug| {
        json!({
            "slug": slug,
            "display_name": slug,
            "description": format!("{slug} description"),
            "base_instructions": format!("{slug} instructions"),
            "supported_reasoning_levels": [{"effort": "low"}],
        })
    })
    .collect::<Vec<_>>();
    let snapshot = json!({ "models": models, "codey_runtime_source": catalog_source_for_app("") });
    fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
    fs::write(
        home.path().join(DEBUG_CATALOG_RELATIVE_PATH),
        serde_json::to_vec(&snapshot).unwrap(),
    )
    .unwrap();
    assert!(!home.path().join("models_cache.json").exists());

    assert_eq!(
        refresh_for_provider(home.path(), true, None, &[]).unwrap(),
        OFFICIAL_MODELS.len()
    );
    let catalog: Value =
        serde_json::from_slice(&fs::read(home.path().join(MODEL_CATALOG_RELATIVE_PATH)).unwrap())
            .unwrap();
    assert_eq!(
        catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| model["slug"].as_str().unwrap())
            .collect::<Vec<_>>(),
        OFFICIAL_MODELS
            .iter()
            .map(|(slug, _)| *slug)
            .collect::<Vec<_>>()
    );
}
#[test]
fn newer_snapshot_supersedes_a_stale_cache_entry() {
    // A capture happens when the snapshot is missing or older than the
    // installed Codex, so a snapshot that outdates `models_cache.json`
    // carries the instructions the cache cannot. Reading sources in a
    // fixed order would let the stale cache win and silently waste the
    // capture, leaving upgraded installs on pre-upgrade instructions.
    let home = tempfile::tempdir().unwrap();
    let source = |label: &str| {
        json!({
            "codey_runtime_source": catalog_source_for_app(""),
            "models": OFFICIAL_MODELS
                .iter()
                .map(|(slug, _)| {
                    json!({
                        "slug": slug,
                        "display_name": slug,
                        "description": format!("{label} description"),
                        "base_instructions": format!("{label} instructions for {slug}"),
                    })
                })
                .collect::<Vec<_>>()
        })
    };
    let cache = home.path().join("models_cache.json");
    let snapshot = home.path().join(DEBUG_CATALOG_RELATIVE_PATH);
    fs::write(&cache, serde_json::to_vec(&source("CACHED")).unwrap()).unwrap();
    fs::create_dir_all(home.path().join("model-catalogs")).unwrap();
    fs::write(&snapshot, serde_json::to_vec(&source("SNAPSHOT")).unwrap()).unwrap();

    let older = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100);
    let newer = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(200);
    for (path, stamp) in [(cache, older), (snapshot, newer)] {
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
    }

    let entries = read_official_entries(home.path(), "").unwrap();
    let sol = entries
        .iter()
        .find(|model| model["slug"] == "gpt-5.6-sol")
        .unwrap();
    assert_eq!(
        sol["base_instructions"],
        "SNAPSHOT instructions for gpt-5.6-sol"
    );

    // The reverse order still prefers the cache, so an install whose Codex
    // keeps writing its own cache does not inherit Codey snapshot text.
    std::fs::File::options()
        .write(true)
        .open(home.path().join("models_cache.json"))
        .unwrap()
        .set_modified(newer + std::time::Duration::from_secs(100))
        .unwrap();
    let entries = read_official_entries(home.path(), "").unwrap();
    let sol = entries
        .iter()
        .find(|model| model["slug"] == "gpt-5.6-sol")
        .unwrap();
    assert_eq!(
        sol["base_instructions"],
        "CACHED instructions for gpt-5.6-sol"
    );
}

#[cfg(test)]
#[test]
fn removing_context_override_restores_the_original_model_budget() {
    let original = json!({
        "slug":"gpt-5.6-sol",
        "context_window":272000,
        "max_context_window":1000000,
        "effective_context_window_percent":95,
        "auto_compact_token_limit":null,
        "codey_context_source":"official_catalog"
    });
    let mut models = vec![original.clone()];
    let empty = std::collections::BTreeMap::new();
    let contexts = std::collections::BTreeMap::from([(
        "gpt-5.6-sol".to_string(),
        crate::config::ModelContextConfig {
            context_window_tokens: 128_000,
            auto_compact_token_limit: Some(100_000),
            reserve_output_tokens: Some(16_000),
        },
    )]);
    for overrides in [&contexts, &empty] {
        apply_overrides_to_models(
            &mut models,
            CatalogOverrides {
                plugin_contexts: &empty,
                contexts: overrides,
                reasoning_efforts: &Default::default(),
                display_names: &Default::default(),
            },
        )
        .unwrap();
        if !overrides.is_empty() {
            assert_eq!(models[0]["context_window"], 128_000);
            assert_eq!(models[0]["auto_compact_token_limit"], 100_000);
        }
    }
    models[0]
        .as_object_mut()
        .unwrap()
        .remove("codey_context_base");
    assert_eq!(models[0], original);
}

#[cfg(test)]
#[test]
fn custom_context_overlay_preserves_source_and_uses_immutable_versions() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("custom.json");
    let original = json!({
        "revision": "user-owned",
        "models": [
            {"slug": "GPT-5.6-SOL", "context_window": 1000000,
             "base_instructions": "Keep user instructions", "custom_field": [1, 2]},
            {"slug": "unmodified", "context_window": 1000000,
             "effective_context_window_percent": 95}
        ]
    });
    let source_bytes = serde_json::to_vec(&original).unwrap();
    fs::write(&source, &source_bytes).unwrap();
    let mut policies = BTreeMap::from([(
        "gpt-5.6-sol".into(),
        crate::config::ModelContextConfig {
            context_window_tokens: 272_000,
            auto_compact_token_limit: Some(220_000),
            reserve_output_tokens: Some(16_000),
        },
    )]);
    let first = prepare_context_catalog_overlay(home.path(), &source, &policies, &[]).unwrap();
    assert_ne!(first, source);
    assert_eq!(
        first,
        prepare_context_catalog_overlay(home.path(), &source, &policies, &[]).unwrap()
    );
    let first_bytes = fs::read(&first).unwrap();
    let first_value: Value = serde_json::from_slice(&first_bytes).unwrap();
    let mut expected = original.clone();
    apply_model_context(&mut expected["models"][0], policies.values().next()).unwrap();
    assert_eq!(first_value, expected);
    assert_eq!(fs::read(&source).unwrap(), source_bytes);
    policies
        .get_mut("gpt-5.6-sol")
        .unwrap()
        .context_window_tokens = 400_000;
    let second = prepare_context_catalog_overlay(home.path(), &source, &policies, &[]).unwrap();
    assert_ne!(first, second);
    assert_eq!(fs::read(&first).unwrap(), first_bytes);
    assert_eq!(
        prepare_context_catalog_overlay(home.path(), &source, &BTreeMap::new(), &[]).unwrap(),
        source
    );
    assert_eq!(fs::read(&source).unwrap(), source_bytes);
    let mut updated = original;
    updated["models"][0]["base_instructions"] = json!("Updated by user");
    fs::write(&source, serde_json::to_vec(&updated).unwrap()).unwrap();
    let third = prepare_context_catalog_overlay(home.path(), &source, &policies, &[]).unwrap();
    assert_ne!(second, third);
    let latest: Value = serde_json::from_slice(&fs::read(third).unwrap()).unwrap();
    assert_eq!(latest["models"][0]["base_instructions"], "Updated by user");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[cfg(test)]
#[test]
fn custom_context_overlay_supplements_exact_route_models_with_full_metadata() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("custom.json");
    let original = json!({
        "revision": "user-owned",
        "models": [{"slug": "other-route/deepseek-v4.1-flash", "custom": [1, 2],
            "comp_hash": "user-declared", "codey_source": "third_party"}]
    });
    let source_bytes = serde_json::to_vec(&original).unwrap();
    fs::write(&source, &source_bytes).unwrap();
    let runtime_path = home.path().join(relative_path());
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let mut runtime_model = json!({
        "slug": "route-mv69jfev-blgrbg/deepseek-v4.1-flash",
        "codey_source": "third_party",
        "comp_hash": "legacy-template",
        "description": "Codey route model",
        "base_instructions": "Keep Codey instructions",
        "supported_reasoning_levels": [{"effort": "high"}],
        "input_modalities": ["text", "image"],
        "custom_field": {"nested": [1, 2]},
        "context_window": 1000000
    });
    let runtime_bytes = serde_json::to_vec(&json!({"models": [runtime_model]})).unwrap();
    fs::write(&runtime_path, &runtime_bytes).unwrap();
    let policies = BTreeMap::from([(
        runtime_model["slug"].as_str().unwrap().to_owned(),
        crate::config::ModelContextConfig {
            context_window_tokens: 128_000,
            auto_compact_token_limit: Some(100_000),
            reserve_output_tokens: Some(16_000),
        },
    )]);
    let first = prepare_context_catalog_overlay(home.path(), &source, &policies, &[]).unwrap();
    let first_bytes = fs::read(&first).unwrap();
    let projected: Value = serde_json::from_slice(&first_bytes).unwrap();
    let mut expected = original.clone();
    let mut expected_model = runtime_model.clone();
    expected_model.as_object_mut().unwrap().remove("comp_hash");
    apply_model_context(&mut expected_model, policies.values().next()).unwrap();
    expected["models"]
        .as_array_mut()
        .unwrap()
        .push(expected_model);
    assert_eq!(projected, expected);
    assert_eq!(fs::read(&source).unwrap(), source_bytes);
    assert_eq!(fs::read(&runtime_path).unwrap(), runtime_bytes);
    assert_eq!(
        first.file_name().unwrap().to_string_lossy(),
        format!("{}.json", crate::fs_util::sha256_hex(&first_bytes))
    );
    assert_eq!(
        first,
        prepare_context_catalog_overlay(home.path(), &source, &policies, &[]).unwrap()
    );

    runtime_model["base_instructions"] = json!("Updated Codey instructions");
    fs::write(
        &runtime_path,
        serde_json::to_vec(&json!({"models": [runtime_model]})).unwrap(),
    )
    .unwrap();
    let second = prepare_context_catalog_overlay(home.path(), &source, &policies, &[]).unwrap();
    assert_ne!(first, second);
    assert_eq!(fs::read(&first).unwrap(), first_bytes);
    assert_eq!(fs::read(&source).unwrap(), source_bytes);
    let latest: Value = serde_json::from_slice(&fs::read(second).unwrap()).unwrap();
    assert_eq!(
        latest["models"][1]["base_instructions"],
        "Updated Codey instructions"
    );
}

#[cfg(test)]
#[test]
fn custom_context_overlay_prefers_source_without_reading_unneeded_fallback() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("custom.json");
    let original = json!({"models": [{
        "slug": "route/model", "base_instructions": "User instructions", "custom": true
    }]});
    let source_bytes = serde_json::to_vec(&original).unwrap();
    fs::write(&source, &source_bytes).unwrap();
    let policies = BTreeMap::from([(
        "route/model".into(),
        crate::config::ModelContextConfig {
            context_window_tokens: 128_000,
            auto_compact_token_limit: None,
            reserve_output_tokens: None,
        },
    )]);
    let runtime_path = home.path().join(relative_path());
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    for fallback in [
        br#"{"models":[{"slug":"route/model","base_instructions":"Codey instructions","description":"Runtime"}]}"#.as_slice(),
        br#"{"models":[{"slug":"route/model"},{"slug":"route/model"}]}"#.as_slice(),
        b"invalid runtime catalog".as_slice(),
    ] {
        fs::write(&runtime_path, fallback).unwrap();
        let overlay = render_context_catalog_overlay(home.path(), &source, &policies, &[])
            .unwrap()
            .unwrap();
        let projected: Value = serde_json::from_slice(&overlay).unwrap();
        let mut expected = original.clone();
        apply_model_context(&mut expected["models"][0], policies.values().next()).unwrap();
        assert_eq!(projected, expected);
        assert_eq!(fs::read(&source).unwrap(), source_bytes);
        assert_eq!(fs::read(&runtime_path).unwrap(), fallback);
    }
}

#[cfg(test)]
#[test]
fn custom_context_overlay_rejects_missing_or_ambiguous_fallback_models() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("custom.json");
    let original = br#"{"models":[]}"#;
    fs::write(&source, original).unwrap();
    let policies = BTreeMap::from([(
        "route/model".into(),
        crate::config::ModelContextConfig {
            context_window_tokens: 128_000,
            auto_compact_token_limit: None,
            reserve_output_tokens: None,
        },
    )]);
    let runtime_path = home.path().join(relative_path());
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let candidate = json!({
        "slug": "route/model", "description": "Runtime", "base_instructions": "Instructions"
    });
    let mut differently_cased = candidate.clone();
    differently_cased["slug"] = json!("ROUTE/MODEL");
    for (catalog, message) in [
        (json!({"models": []}), "缺少已启用的预算模型"),
        (
            json!({"models": [{"slug": "model"}, {"slug": "other/model"}]}),
            "缺少已启用的预算模型",
        ),
        (
            json!({"models": [candidate, differently_cased]}),
            "预算模型重复",
        ),
        (
            json!({"models": [{"slug": "route/model"}]}),
            "缺少 Codex 必需字段",
        ),
        (json!({"models": {}}), "缺少 models 数组"),
    ] {
        let runtime_bytes = serde_json::to_vec(&catalog).unwrap();
        fs::write(&runtime_path, &runtime_bytes).unwrap();
        let error =
            prepare_context_catalog_overlay(home.path(), &source, &policies, &[]).unwrap_err();
        assert!(error.is::<ContextBudgetCatalogError>());
        assert!(error.to_string().contains(message), "{error}");
        assert_eq!(fs::read(&source).unwrap(), original);
        assert_eq!(fs::read(&runtime_path).unwrap(), runtime_bytes);
        assert!(
            !home
                .path()
                .join("model-catalogs/context-overrides")
                .exists()
        );
    }
}

#[cfg(test)]
#[test]
fn custom_context_overlay_rejects_missing_ambiguous_or_invalid_models() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("custom.json");
    let policies = BTreeMap::from([(
        "route/model".into(),
        crate::config::ModelContextConfig {
            context_window_tokens: 128_000,
            auto_compact_token_limit: None,
            reserve_output_tokens: None,
        },
    )]);
    for (catalog, message) in [
        (json!({"models": []}), "缺少已启用的预算模型"),
        (
            json!({"models": [{"slug": "other/model"}]}),
            "缺少已启用的预算模型",
        ),
        (
            json!({"models": [{"slug": "route/model"}, {"slug": "ROUTE/MODEL"}]}),
            "预算模型重复",
        ),
        (json!({"models": {}}), "缺少 models 数组"),
    ] {
        let bytes = serde_json::to_vec(&catalog).unwrap();
        fs::write(&source, &bytes).unwrap();
        let error =
            prepare_context_catalog_overlay(home.path(), &source, &policies, &[]).unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
        assert_eq!(
            error.is::<ContextBudgetCatalogError>(),
            message != "缺少 models 数组"
        );
        assert!(!anyhow::anyhow!(error.to_string()).is::<ContextBudgetCatalogError>());
        assert_eq!(fs::read(&source).unwrap(), bytes);
        assert!(
            !home
                .path()
                .join("model-catalogs/context-overrides")
                .exists()
        );
    }
    fs::write(&source, b"not json").unwrap();
    let error = render_context_catalog_overlay(home.path(), &source, &policies, &[]).unwrap_err();
    assert!(!error.is::<ContextBudgetCatalogError>());
    assert!(
        render_context_catalog_overlay(home.path(), &source, &BTreeMap::new(), &[])
            .unwrap()
            .is_none()
    );
    let original = br#"{"models":[{"slug":"route/model"}]}"#;
    fs::write(&source, original).unwrap();
    let mut invalid = policies;
    invalid
        .get_mut("route/model")
        .unwrap()
        .auto_compact_token_limit = Some(128_000);
    let error = prepare_context_catalog_overlay(home.path(), &source, &invalid, &[]).unwrap_err();
    assert!(error.is::<ContextBudgetCatalogError>());
    assert_eq!(fs::read(&source).unwrap(), original);
    assert!(
        !home
            .path()
            .join("model-catalogs/context-overrides")
            .exists()
    );
}

#[test]
fn custom_context_overlay_keeps_required_models_after_budget_removal() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("custom.json");
    let source_bytes = br#"{"models":[{"slug":"external/model","custom":true}]}"#;
    fs::write(&source, source_bytes).unwrap();
    let mut original_model = json!({
        "slug": "route/model",
        "description": "Runtime",
        "base_instructions": "Instructions",
        "context_window": 320000,
        "effective_context_window_percent": 91,
        "codey_context_source": "official_catalog",
        "custom": [1, 2]
    });
    prepare_cached_context_window(&mut original_model);
    let policies = BTreeMap::from([(
        "route/model".into(),
        crate::config::ModelContextConfig {
            context_window_tokens: 128_000,
            auto_compact_token_limit: Some(100_000),
            reserve_output_tokens: Some(16_000),
        },
    )]);
    let mut cached_model = original_model.clone();
    apply_model_context(&mut cached_model, policies.values().next()).unwrap();
    let runtime_path = home.path().join(relative_path());
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let runtime_bytes = serde_json::to_vec(&json!({"models": [cached_model]})).unwrap();
    fs::write(&runtime_path, &runtime_bytes).unwrap();
    let required = vec!["ROUTE/MODEL".to_string(), "route/model".to_string()];

    let first =
        prepare_context_catalog_overlay(home.path(), &source, &policies, &required).unwrap();
    let first_bytes = fs::read(&first).unwrap();
    let with_budget: Value = serde_json::from_slice(&first_bytes).unwrap();
    assert_eq!(with_budget["models"].as_array().unwrap().len(), 2);
    assert_eq!(with_budget["models"][1], cached_model);

    let second =
        prepare_context_catalog_overlay(home.path(), &source, &BTreeMap::new(), &required).unwrap();
    let without_budget: Value = serde_json::from_slice(&fs::read(&second).unwrap()).unwrap();
    assert_eq!(without_budget["models"].as_array().unwrap().len(), 2);
    assert_eq!(without_budget["models"][1], original_model);
    assert_ne!(first, second);
    assert_eq!(fs::read(&first).unwrap(), first_bytes);
    assert_eq!(fs::read(&source).unwrap(), source_bytes);
    assert_eq!(fs::read(&runtime_path).unwrap(), runtime_bytes);
}

#[test]
fn custom_context_overlay_required_only_preserves_plugin_and_unmarked_metadata() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("custom.json");
    fs::write(&source, br#"{"models":[]}"#).unwrap();
    let models = vec![
        json!({
            "slug": "route/plugin",
            "description": "Plugin model",
            "base_instructions": "Plugin instructions",
            "context_window": 918000,
            "codey_context_source": "plugin_declared",
            "codey_context_base": {"context_window": 272000},
            "custom": {"nested": [1, 2]}
        }),
        json!({
            "slug": "route/unmarked",
            "description": "Unmarked model",
            "base_instructions": "Unmarked instructions",
            "context_window": 1000000,
            "codey_source": "third_party"
        }),
    ];
    let runtime_path = home.path().join(relative_path());
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    fs::write(
        &runtime_path,
        serde_json::to_vec(&json!({"models": models})).unwrap(),
    )
    .unwrap();
    let required = vec![
        "route/plugin".into(),
        "route/unmarked".into(),
        "ROUTE/PLUGIN".into(),
    ];

    let overlay = render_context_catalog_overlay(home.path(), &source, &BTreeMap::new(), &required)
        .unwrap()
        .unwrap();
    let projected: Value = serde_json::from_slice(&overlay).unwrap();
    assert_eq!(projected["models"], json!(models));
}

#[test]
fn custom_context_overlay_complete_required_catalog_reuses_source() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("custom.json");
    let source_bytes = br#"{"models":[{"slug":"route/model","context_window":1000000,"codey_context_source":"user_declared","codey_context_base":{"context_window":272000}}]}"#;
    fs::write(&source, source_bytes).unwrap();
    let runtime_path = home.path().join(relative_path());
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    fs::write(&runtime_path, b"invalid unused fallback").unwrap();
    let required = vec!["ROUTE/MODEL".into(), "route/model".into()];

    assert!(
        render_context_catalog_overlay(home.path(), &source, &BTreeMap::new(), &required)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        prepare_context_catalog_overlay(home.path(), &source, &BTreeMap::new(), &required).unwrap(),
        source
    );
    assert_eq!(fs::read(&source).unwrap(), source_bytes);
    assert!(
        !home
            .path()
            .join("model-catalogs/context-overrides")
            .exists()
    );
}

#[test]
fn custom_context_overlay_without_requirements_does_not_read_catalogs() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("missing.json");
    assert!(
        render_context_catalog_overlay(home.path(), &source, &BTreeMap::new(), &[])
            .unwrap()
            .is_none()
    );
    assert_eq!(
        prepare_context_catalog_overlay(home.path(), &source, &BTreeMap::new(), &[]).unwrap(),
        source
    );
    assert!(!home.path().join("model-catalogs").exists());
}

#[test]
fn custom_context_overlay_required_errors_do_not_offer_budget_recovery() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join("custom.json");
    let runtime_path = home.path().join(relative_path());
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    let candidate = json!({
        "slug": "route/model", "description": "Runtime", "base_instructions": "Instructions"
    });
    let mut differently_cased = candidate.clone();
    differently_cased["slug"] = json!("ROUTE/MODEL");
    let required = vec!["route/model".into()];
    let policies = BTreeMap::from([(
        "ROUTE/MODEL".into(),
        crate::config::ModelContextConfig {
            context_window_tokens: 128_000,
            auto_compact_token_limit: None,
            reserve_output_tokens: None,
        },
    )]);
    for (external_models, fallback_models, message) in [
        (json!([]), json!([]), "缺少已启用的必需模型"),
        (
            json!([]),
            json!([{"slug": "model"}, {"slug": "other/model"}]),
            "缺少已启用的必需模型",
        ),
        (
            json!([candidate, differently_cased]),
            json!([]),
            "自定义模型目录中的必需模型重复",
        ),
        (
            json!([]),
            json!([candidate, differently_cased]),
            "Codey 运行时模型目录中的必需模型重复",
        ),
        (
            json!([]),
            json!([{"slug": "route/model"}]),
            "缺少 Codex 必需字段",
        ),
        (json!([]), json!({}), "缺少 models 数组"),
    ] {
        let source_bytes = serde_json::to_vec(&json!({"models": external_models})).unwrap();
        let runtime_bytes = serde_json::to_vec(&json!({"models": fallback_models})).unwrap();
        fs::write(&source, &source_bytes).unwrap();
        fs::write(&runtime_path, &runtime_bytes).unwrap();
        let error =
            prepare_context_catalog_overlay(home.path(), &source, &BTreeMap::new(), &required)
                .unwrap_err();
        assert!(!error.is::<ContextBudgetCatalogError>());
        assert!(format!("{error:#}").contains(message), "{error:#}");
        let budget_error =
            prepare_context_catalog_overlay(home.path(), &source, &policies, &required)
                .unwrap_err();
        assert!(budget_error.is::<ContextBudgetCatalogError>());
        assert_eq!(fs::read(&source).unwrap(), source_bytes);
        assert_eq!(fs::read(&runtime_path).unwrap(), runtime_bytes);
        assert!(
            !home
                .path()
                .join("model-catalogs/context-overrides")
                .exists()
        );
    }
}

#[cfg(test)]
#[test]
fn cached_context_projection_preserves_missing_fields_and_is_idempotent() {
    for mut model in [
        json!({"slug": "route/gpt-5.6-sol", "codey_source": "third_party"}),
        json!({"slug": "gpt-5.6-sol", "context_window": 272000, "auto_compact_token_limit": null}),
    ] {
        let original = model.clone();
        prepare_cached_context_window(&mut model);
        let projected = model.clone();
        prepare_cached_context_window(&mut model);
        assert_eq!(model, projected);
        model.as_object_mut().unwrap().remove("codey_context_base");
        assert_eq!(model, original);
    }
}

#[cfg(test)]
#[test]
fn model_context_projection_restores_cache_and_explicit_overrides_legacy() {
    use crate::config::ModelContextConfig;
    let mut model = json!({ "slug": "route/custom", "codey_source": "third_party", "context_window": 272000, "max_context_window": 872000 });
    prepare_cached_context_window(&mut model);
    assert_eq!(model["context_window"], 200_000);
    assert_eq!(model["codey_context_source"], "conservative_fallback");
    let policy = ModelContextConfig {
        context_window_tokens: 100_000,
        auto_compact_token_limit: Some(80_000),
        reserve_output_tokens: Some(12_345),
    };
    apply_model_context(&mut model, Some(&policy)).unwrap();
    assert_eq!(model["context_window"], 100_000);
    assert_eq!(model["max_context_window"], 100_000);
    assert_eq!(model["effective_context_window_percent"], 87);
    assert_eq!(model["auto_compact_token_limit"], 80_000);
    assert_eq!(model["codey_context_source"], "user_declared");
    prepare_cached_context_window(&mut model);
    assert_eq!(model["context_window"], 200_000);
    assert_eq!(model["max_context_window"], 200_000);
    assert_eq!(model["effective_context_window_percent"], 95);
    assert!(model["auto_compact_token_limit"].is_null());
    let mut trusted = json!({ "slug": "gpt-5.5", "context_window": 272000, "max_context_window": 872000, "effective_context_window_percent": 95 });
    prepare_cached_context_window(&mut trusted);
    assert_eq!(trusted["max_context_window"], 872000);
    let mut native_large = json!({"slug":"official", "context_window":1000000, "max_context_window":1000000, "effective_context_window_percent":95, "codey_context_source":"official_catalog"});
    prepare_cached_context_window(&mut native_large);
    assert_eq!(native_large["context_window"], 1000000);
    assert_eq!(native_large["effective_context_window_percent"], 95);
    let mut legacy_override = json!({ "slug": "route/legacy", "codey_source": "third_party", "context_window": 1000000, "max_context_window": 1000000, "effective_context_window_percent": 100, "auto_compact_token_limit": null, "codey_context_source": "legacy_1m" });
    prepare_cached_context_window(&mut legacy_override);
    assert_eq!(legacy_override["context_window"], 272_000);
    assert_eq!(legacy_override["max_context_window"], 272_000);
    assert_eq!(legacy_override["effective_context_window_percent"], 95);
}

#[cfg(test)]
fn level_has_runtime_description(level: &Value) -> bool {
    level
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|description| !description.is_empty())
}

#[cfg(test)]
#[test]
fn plugin_context_defaults_yield_to_users_and_restore_after_override_removal() {
    use crate::config::ModelContextConfig;
    use std::collections::BTreeMap;
    let plugin = BTreeMap::from([(
        "route/model".into(),
        ModelContextConfig {
            context_window_tokens: 918000,
            auto_compact_token_limit: Some(826000),
            reserve_output_tokens: None,
        },
    )]);
    let user = BTreeMap::from([(
        "route/model".into(),
        ModelContextConfig {
            context_window_tokens: 100000,
            auto_compact_token_limit: Some(80000),
            reserve_output_tokens: None,
        },
    )]);
    let empty = BTreeMap::new();
    let reasoning = BTreeMap::new();
    let mut models = vec![json!({"slug":"route/model"})];
    apply_overrides_to_models(
        &mut models,
        CatalogOverrides {
            plugin_contexts: &plugin,
            contexts: &empty,
            reasoning_efforts: &reasoning,
            display_names: &Default::default(),
        },
    )
    .unwrap();
    assert_eq!(models[0]["context_window"], 918000);
    assert_eq!(models[0]["codey_context_source"], "plugin_declared");
    apply_overrides_to_models(
        &mut models,
        CatalogOverrides {
            plugin_contexts: &plugin,
            contexts: &user,
            reasoning_efforts: &reasoning,
            display_names: &Default::default(),
        },
    )
    .unwrap();
    assert_eq!(models[0]["context_window"], 100000);
    assert_eq!(models[0]["codey_context_source"], "user_declared");
    apply_overrides_to_models(
        &mut models,
        CatalogOverrides {
            plugin_contexts: &plugin,
            contexts: &empty,
            reasoning_efforts: &reasoning,
            display_names: &Default::default(),
        },
    )
    .unwrap();
    assert_eq!(models[0]["context_window"], 918000);
}
