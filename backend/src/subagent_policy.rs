use std::path::Path;

use crate::config::{
    CodeyConfig, DEFAULT_SUBAGENT_MODEL, DEFAULT_SUBAGENT_REASONING_EFFORT, SUBAGENT_ROLE_DEFAULT,
    SUBAGENT_ROLE_IDS, SubagentRoleConfig, uniform_subagent_roles,
};
use crate::model_catalog;
use crate::model_id;
#[cfg(test)]
use crate::subagent::rules::{RoleAccess, RolePolicy};

#[cfg(test)]
pub(crate) fn role_policy(role: &str) -> Option<RolePolicy> {
    crate::subagent::rules::embedded().role_policy(role)
}

pub(crate) fn reconcile_for_current_provider(
    config: &mut CodeyConfig,
    codex_home: &Path,
    official_provider: bool,
) {
    prepare_subagent_roles(config);
    let selected_models = if official_provider {
        config.selected_models().to_vec()
    } else {
        config
            .current_provider_id()
            .map(|provider_id| config.enabled_route_models(provider_id))
            .unwrap_or_default()
    };
    let state = model_catalog::selection_state_with_manual_models(
        codex_home,
        official_provider,
        config.upstream_models_snapshot(),
        &selected_models,
        config.manual_third_party_models(),
        Some(&config.subagent_model),
    )
    .ok();
    reconcile_with_model_state(config, state.as_ref());
}

pub(crate) fn reconcile_with_model_state(
    config: &mut CodeyConfig,
    state: Option<&model_catalog::ModelSelectionState>,
) {
    prepare_subagent_roles(config);
    let route_targets = config.runtime_model_targets();
    let current_provider_id = config.current_provider_id().map(str::to_string);
    let Some(state) = state else {
        let route_aliases = route_targets
            .into_iter()
            .map(|target| target.alias)
            .collect::<Vec<_>>();
        canonicalize_route_aliases(&mut config.subagent_roles, &route_aliases);
        sync_legacy_default(config);
        return;
    };
    for selection in config.subagent_roles.values_mut() {
        let requested = selection.model.trim();
        if let Some(target) = route_targets
            .iter()
            .find(|target| model_id::equal(&target.alias, requested))
        {
            selection.model.clone_from(&target.alias);
            let current_route = current_provider_id.as_deref() == Some(target.provider_id.as_str());
            let metadata_model = if target.official {
                state
                    .official_models
                    .iter()
                    .find(|model| {
                        model.supported && model_id::equal(&model.slug, &target.upstream_model)
                    })
                    .map(|model| model.slug.as_str())
            } else {
                state
                    .third_party_model_metadata
                    .iter()
                    .find(|model| model_id::equal(&model.slug, &target.alias))
                    .or_else(|| {
                        state.third_party_model_metadata.iter().find(|model| {
                            current_route && model_id::equal(&model.slug, &target.upstream_model)
                        })
                    })
                    .map(|model| model.slug.as_str())
                    .or_else(|| {
                        state
                            .third_party_models
                            .iter()
                            .find(|model| {
                                current_route && model_id::equal(model, &target.upstream_model)
                            })
                            .map(String::as_str)
                    })
            };
            if let Some(model) = metadata_model {
                selection.reasoning_effort =
                    reasoning_effort_for_model(state, model, &selection.reasoning_effort);
            }
            continue;
        }
        let Some(model) = state
            .available_model(requested)
            .or_else(|| state.available_model(&state.default_model))
            .or_else(|| state.available_model(DEFAULT_SUBAGENT_MODEL))
            .or_else(|| state.first_available_model())
        else {
            continue;
        };
        let model = model.to_string();
        selection.reasoning_effort =
            reasoning_effort_for_model(state, &model, &selection.reasoning_effort);
        selection.model = model;
    }
    sync_legacy_default(config);
}

fn canonicalize_route_aliases(
    roles: &mut std::collections::BTreeMap<String, SubagentRoleConfig>,
    aliases: &[String],
) {
    for selection in roles.values_mut() {
        if let Some(alias) = aliases
            .iter()
            .find(|alias| model_id::equal(alias, selection.model.trim()))
        {
            selection.model.clone_from(alias);
        }
    }
}

fn prepare_subagent_roles(config: &mut CodeyConfig) {
    if config.subagent_model.trim().is_empty() {
        config.subagent_model = DEFAULT_SUBAGENT_MODEL.to_string();
    }
    if config.subagent_roles.is_empty() {
        config.subagent_roles =
            uniform_subagent_roles(&config.subagent_model, &config.subagent_reasoning_effort);
        return;
    }

    // The scalar fields are retained as the compatibility representation of
    // the fallback role. A caller that still mutates those fields directly
    // therefore continues to update `default` without resetting other roles.
    config.subagent_roles.insert(
        SUBAGENT_ROLE_DEFAULT.to_string(),
        SubagentRoleConfig::new(
            config.subagent_model.clone(),
            config.subagent_reasoning_effort.clone(),
        ),
    );
    let fallback = config
        .subagent_roles
        .get(SUBAGENT_ROLE_DEFAULT)
        .cloned()
        .expect("fallback subagent role was inserted");
    for role in SUBAGENT_ROLE_IDS {
        config
            .subagent_roles
            .entry(role.to_string())
            .or_insert_with(|| fallback.clone());
    }
}

fn sync_legacy_default(config: &mut CodeyConfig) {
    if let Some(selection) = config.subagent_roles.get(SUBAGENT_ROLE_DEFAULT) {
        config.subagent_model.clone_from(&selection.model);
        config
            .subagent_reasoning_effort
            .clone_from(&selection.reasoning_effort);
    }
}

pub(crate) fn reasoning_effort_for_model(
    state: &model_catalog::ModelSelectionState,
    model: &str,
    preferred_reasoning_effort: &str,
) -> String {
    let preferred_reasoning_effort = preferred_reasoning_effort.trim().to_ascii_lowercase();
    if let Some(official_model) = state
        .official_models
        .iter()
        .find(|candidate| candidate.supported && model_id::equal(&candidate.slug, model))
    {
        if official_model
            .supported_reasoning_efforts
            .iter()
            .any(|effort| effort.eq_ignore_ascii_case(&preferred_reasoning_effort))
        {
            return preferred_reasoning_effort;
        }
        if official_model
            .supported_reasoning_efforts
            .iter()
            .any(|effort| effort == DEFAULT_SUBAGENT_REASONING_EFFORT)
        {
            return DEFAULT_SUBAGENT_REASONING_EFFORT.to_string();
        }
        if !official_model.default_reasoning_effort.trim().is_empty() {
            return official_model.default_reasoning_effort.clone();
        }
    }
    if state
        .third_party_models
        .iter()
        .any(|candidate| model_id::equal(candidate, model))
        || state
            .third_party_model_metadata
            .iter()
            .any(|candidate| model_id::equal(&candidate.slug, model))
    {
        let metadata = state
            .third_party_model_metadata
            .iter()
            .find(|candidate| model_id::equal(&candidate.slug, model));
        let fallback_efforts = model_catalog::THIRD_PARTY_REASONING_EFFORTS.to_vec();
        let supported_reasoning_efforts = metadata
            .map(|metadata| {
                metadata
                    .supported_reasoning_efforts
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            })
            .unwrap_or(fallback_efforts);
        if supported_reasoning_efforts
            .iter()
            .any(|effort| effort.eq_ignore_ascii_case(&preferred_reasoning_effort))
        {
            return preferred_reasoning_effort;
        }
        if supported_reasoning_efforts.contains(&DEFAULT_SUBAGENT_REASONING_EFFORT) {
            return DEFAULT_SUBAGENT_REASONING_EFFORT.to_string();
        }
        if let Some(metadata) = metadata
            && !metadata.default_reasoning_effort.trim().is_empty()
        {
            return metadata.default_reasoning_effort.clone();
        }
    }
    DEFAULT_SUBAGENT_REASONING_EFFORT.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderProfile;

    #[test]
    fn role_policies_keep_access_and_visual_capabilities_explicit() {
        assert_eq!(
            role_policy(crate::config::SUBAGENT_ROLE_QUICK_SCAN),
            Some(RolePolicy {
                access: RoleAccess::ReadOnly,
                visual: false,
            })
        );
        assert_eq!(
            role_policy(crate::config::SUBAGENT_ROLE_WORKER),
            Some(RolePolicy {
                access: RoleAccess::Write,
                visual: false,
            })
        );
        assert_eq!(
            role_policy(crate::config::SUBAGENT_ROLE_VISUAL_WORKER),
            Some(RolePolicy {
                access: RoleAccess::Write,
                visual: true,
            })
        );
        assert_eq!(role_policy("unknown"), None);
    }

    fn model_state() -> model_catalog::ModelSelectionState {
        model_catalog::ModelSelectionState {
            official_models: ["gpt-5.6-luna", DEFAULT_SUBAGENT_MODEL]
                .into_iter()
                .map(|slug| model_catalog::OfficialModelAvailability {
                    slug: slug.to_string(),
                    display_name: slug.to_string(),
                    supported: true,
                    supported_reasoning_efforts: vec!["low".into(), "high".into()],
                    default_reasoning_effort: "low".into(),
                })
                .collect(),
            official_model_ids: vec!["gpt-5.6-luna".into(), DEFAULT_SUBAGENT_MODEL.into()],
            default_model: "gpt-5.6-luna".into(),
            ..model_catalog::ModelSelectionState::default()
        }
    }

    fn route_config(provider_id: &str) -> CodeyConfig {
        let mut profile = ProviderProfile::new("Route");
        profile.id = provider_id.to_string();
        profile.official_account = false;
        CodeyConfig {
            active_profile_id: provider_id.to_string(),
            profiles: vec![profile],
            subagent_optimization: true,
            subagent_model: "provider-old-model".into(),
            ..CodeyConfig::default()
        }
    }

    #[test]
    fn provider_change_selects_a_compatible_model_and_reasoning_effort() {
        struct Case {
            upstream_models: &'static [&'static str],
            saved_effort: &'static str,
            expected_model: &'static str,
            expected_effort: &'static str,
            optimization_enabled: bool,
        }

        let cases = [
            Case {
                upstream_models: &[DEFAULT_SUBAGENT_MODEL],
                saved_effort: "xhigh",
                expected_model: DEFAULT_SUBAGENT_MODEL,
                expected_effort: "xhigh",
                optimization_enabled: true,
            },
            Case {
                upstream_models: &["gpt-5.6-sol"],
                saved_effort: "ultra",
                expected_model: "gpt-5.6-sol",
                expected_effort: "ultra",
                optimization_enabled: true,
            },
            Case {
                upstream_models: &["gpt-5.4"],
                saved_effort: "ultra",
                expected_model: "gpt-5.4",
                expected_effort: DEFAULT_SUBAGENT_REASONING_EFFORT,
                optimization_enabled: true,
            },
            Case {
                upstream_models: &["provider-custom-model"],
                saved_effort: "high",
                expected_model: "provider-custom-model",
                expected_effort: "high",
                optimization_enabled: true,
            },
        ];

        for case in cases {
            let home = tempfile::tempdir().unwrap();
            let mut config = route_config("route-b");
            config.subagent_reasoning_effort = case.saved_effort.into();
            config.upstream_models_by_provider.insert(
                "route-b".into(),
                case.upstream_models
                    .iter()
                    .map(|model| (*model).to_string())
                    .collect(),
            );
            config.selected_models_by_provider.insert(
                "route-b".into(),
                case.upstream_models
                    .iter()
                    .map(|model| (*model).to_string())
                    .collect(),
            );

            reconcile_for_current_provider(&mut config, home.path(), false);

            assert_eq!(config.subagent_model, case.expected_model);
            assert_eq!(config.subagent_reasoning_effort, case.expected_effort);
            assert_eq!(
                config.subagent_optimization, case.optimization_enabled,
                "unexpected optimization state for {}",
                case.expected_model
            );
        }
    }

    #[test]
    fn model_state_falls_back_from_luna_to_terra() {
        let mut config = route_config("route-a");
        config.subagent_model = "gpt-5.6-luna".into();
        config.subagent_reasoning_effort = "high".into();
        let mut state = model_state();
        state
            .official_models
            .retain(|model| model.slug == DEFAULT_SUBAGENT_MODEL);

        reconcile_with_model_state(&mut config, Some(&state));

        assert!(config.subagent_optimization);
        assert_eq!(config.subagent_model, DEFAULT_SUBAGENT_MODEL);
        assert_eq!(config.subagent_reasoning_effort, "high");
    }

    #[test]
    fn missing_model_state_preserves_optimization_and_selection() {
        let mut config = route_config("route-a");
        config.subagent_model = "gpt-5.6-luna".into();
        config.subagent_reasoning_effort = "high".into();

        reconcile_with_model_state(&mut config, None);

        assert!(config.subagent_optimization);
        assert_eq!(config.subagent_model, "gpt-5.6-luna");
        assert_eq!(config.subagent_reasoning_effort, "high");
    }

    #[test]
    fn provider_change_accepts_a_selected_third_party_model() {
        let home = tempfile::tempdir().unwrap();
        let mut config = route_config("route-b");
        config.subagent_reasoning_effort = "high".into();
        config
            .selected_models_by_provider
            .insert("route-b".into(), vec!["provider-custom-model".into()]);
        config
            .upstream_models_by_provider
            .insert("route-b".into(), vec!["provider-custom-model".into()]);

        reconcile_for_current_provider(&mut config, home.path(), false);

        assert!(config.subagent_optimization);
        assert_eq!(config.subagent_model, "provider-custom-model");
        assert_eq!(config.subagent_reasoning_effort, "high");
    }

    #[test]
    fn provider_change_preserves_a_saved_compatible_subagent_model() {
        let home = tempfile::tempdir().unwrap();
        let mut config = route_config("route-b");
        config.subagent_model = "gpt-5.6-sol".into();
        config.subagent_reasoning_effort = "high".into();
        config.upstream_models_by_provider.insert(
            "route-b".into(),
            vec![DEFAULT_SUBAGENT_MODEL.into(), "gpt-5.6-sol".into()],
        );

        reconcile_for_current_provider(&mut config, home.path(), false);

        assert!(config.subagent_optimization);
        assert_eq!(config.subagent_model, "gpt-5.6-sol");
        assert_eq!(config.subagent_reasoning_effort, "high");
    }

    #[test]
    fn route_aliases_reconcile_reasoning_using_the_upstream_model_metadata() {
        let home = tempfile::tempdir().unwrap();
        for (model, preferred, expected) in [
            ("gpt-5.4", "ultra", DEFAULT_SUBAGENT_REASONING_EFFORT),
            ("gpt-5.6-luna", "ultra", "ultra"),
            ("gpt-5.6-sol", "ultra", "ultra"),
            (
                "provider-special",
                "ultra",
                DEFAULT_SUBAGENT_REASONING_EFFORT,
            ),
            ("provider-special", "high", "high"),
        ] {
            let mut config = route_config("route-a");
            config
                .selected_models_by_provider
                .insert("route-a".into(), vec![model.into()]);
            config.subagent_model = format!("ROUTE-A/{model}");
            config.subagent_reasoning_effort = preferred.into();
            config.subagent_roles = uniform_subagent_roles(&config.subagent_model, preferred);

            reconcile_for_current_provider(&mut config, home.path(), false);

            assert_eq!(config.subagent_model, format!("route-a/{model}"));
            assert_eq!(config.subagent_reasoning_effort, expected, "{model}");
            assert!(
                config
                    .subagent_roles
                    .values()
                    .all(|role| role.reasoning_effort == expected)
            );
        }
    }

    #[test]
    fn route_qualified_model_is_preserved_outside_the_current_route_state() {
        let mut provider_a = ProviderProfile::new("A");
        provider_a.id = "route-a".into();
        let mut provider_b = ProviderProfile::new("B");
        provider_b.id = "route-b".into();
        let mut config = CodeyConfig {
            active_profile_id: provider_a.id.clone(),
            profiles: vec![provider_a, provider_b],
            selected_models_by_provider: std::collections::BTreeMap::from([
                ("route-a".into(), vec![DEFAULT_SUBAGENT_MODEL.into()]),
                ("route-b".into(), vec!["provider-special".into()]),
            ]),
            subagent_optimization: true,
            subagent_model: "route-b/provider-special".into(),
            subagent_reasoning_effort: "high".into(),
            subagent_roles: uniform_subagent_roles("route-b/provider-special", "high"),
            ..CodeyConfig::default()
        }
        .normalize();

        let mut state = model_state();
        state.third_party_models.push("provider-special".into());
        state
            .third_party_model_metadata
            .push(model_catalog::ThirdPartyModelAvailability {
                slug: "provider-special".into(),
                supported_reasoning_efforts: vec!["low".into()],
                default_reasoning_effort: "low".into(),
            });
        reconcile_with_model_state(&mut config, Some(&state));

        assert_eq!(config.subagent_model, "route-b/provider-special");
        assert_eq!(config.subagent_reasoning_effort, "high");
        assert!(
            config
                .subagent_roles
                .values()
                .all(|selection| selection.model == "route-b/provider-special")
        );

        state
            .third_party_model_metadata
            .push(model_catalog::ThirdPartyModelAvailability {
                slug: "route-b/provider-special".into(),
                supported_reasoning_efforts: vec!["medium".into()],
                default_reasoning_effort: "medium".into(),
            });
        reconcile_with_model_state(&mut config, Some(&state));
        assert_eq!(config.subagent_model, "route-b/provider-special");
        assert_eq!(config.subagent_reasoning_effort, "medium");
        assert!(
            config
                .subagent_roles
                .values()
                .all(|selection| selection.reasoning_effort == "medium")
        );
    }

    #[test]
    fn incompatible_provider_fallback_remains_global_across_route_changes() {
        let mut provider_a = ProviderProfile::new("A");
        provider_a.id = "route-a".into();
        let mut provider_b = ProviderProfile::new("B");
        provider_b.id = "route-b".into();
        let mut config = CodeyConfig {
            active_profile_id: provider_a.id.clone(),
            profiles: vec![provider_a, provider_b],
            subagent_optimization: true,
            subagent_model: "gpt-5.6-luna".into(),
            subagent_reasoning_effort: "high".into(),
            subagent_roles: uniform_subagent_roles("gpt-5.6-luna", "high"),
            ..CodeyConfig::default()
        }
        .normalize();

        config.active_profile_id = "route-b".into();
        let mut route_b_models = model_state();
        route_b_models
            .official_models
            .retain(|model| model.slug == DEFAULT_SUBAGENT_MODEL);
        route_b_models.default_model = DEFAULT_SUBAGENT_MODEL.into();
        reconcile_with_model_state(&mut config, Some(&route_b_models));
        assert_eq!(config.subagent_model, DEFAULT_SUBAGENT_MODEL);

        config.active_profile_id = "route-a".into();
        assert_eq!(config.subagent_model, DEFAULT_SUBAGENT_MODEL);
        assert_eq!(config.subagent_reasoning_effort, "high");

        config.active_profile_id = "route-b".into();
        assert_eq!(config.subagent_model, DEFAULT_SUBAGENT_MODEL);
    }

    #[test]
    fn unchanged_provider_only_reconciles_an_unavailable_model() {
        let available_home = tempfile::tempdir().unwrap();
        let mut available = route_config("route-a");
        available.subagent_model = "gpt-5.6-sol".into();
        available.subagent_reasoning_effort = "high".into();

        reconcile_for_current_provider(&mut available, available_home.path(), false);

        assert!(available.subagent_optimization);
        assert_eq!(available.subagent_model, "gpt-5.6-sol");
        assert_eq!(available.subagent_reasoning_effort, "high");

        let unavailable_home = tempfile::tempdir().unwrap();
        let mut unavailable = route_config("route-a");
        unavailable.subagent_model = DEFAULT_SUBAGENT_MODEL.into();
        unavailable.subagent_reasoning_effort = "high".into();
        unavailable
            .upstream_models_by_provider
            .insert("route-a".into(), vec!["provider-custom-model".into()]);

        reconcile_for_current_provider(&mut unavailable, unavailable_home.path(), false);

        assert!(unavailable.subagent_optimization);
        assert_eq!(unavailable.subagent_model, DEFAULT_SUBAGENT_MODEL);
        assert_eq!(unavailable.subagent_reasoning_effort, "high");
    }

    #[test]
    fn api_route_models_use_route_reasoning_efforts_even_when_names_match_official_models() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("models_cache.json"),
            serde_json::to_vec(&serde_json::json!({
                "models": [{
                    "slug": "gpt-5.6-sol",
                    "display_name": "GPT-5.6-Sol",
                    "default_reasoning_level": "medium",
                    "supported_reasoning_levels": [
                        {"effort": "medium"},
                        {"effort": "high"}
                    ]
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let mut config = route_config("route-a");
        config.subagent_model = "gpt-5.6-sol".into();
        config.subagent_reasoning_effort = "low".into();
        config
            .upstream_models_by_provider
            .insert("route-a".into(), vec!["gpt-5.6-sol".into()]);
        config
            .selected_models_by_provider
            .insert("route-a".into(), vec!["gpt-5.6-sol".into()]);

        reconcile_for_current_provider(&mut config, home.path(), false);

        assert!(config.subagent_optimization);
        assert_eq!(config.subagent_model, "gpt-5.6-sol");
        assert_eq!(config.subagent_reasoning_effort, "low");
    }

    #[test]
    fn task_roles_keep_independent_models_and_reasoning_efforts() {
        let mut config = route_config("route-a").normalize();
        config.subagent_model = DEFAULT_SUBAGENT_MODEL.into();
        config.subagent_reasoning_effort = "low".into();
        config.subagent_roles.insert(
            crate::config::SUBAGENT_ROLE_QUICK_SCAN.into(),
            crate::config::SubagentRoleConfig::new("gpt-5.6-luna", "low"),
        );
        config.subagent_roles.insert(
            crate::config::SUBAGENT_ROLE_DEEP_RESEARCH.into(),
            crate::config::SubagentRoleConfig::new(DEFAULT_SUBAGENT_MODEL, "high"),
        );

        reconcile_with_model_state(&mut config, Some(&model_state()));

        assert_eq!(
            config.subagent_roles[crate::config::SUBAGENT_ROLE_QUICK_SCAN],
            crate::config::SubagentRoleConfig::new("gpt-5.6-luna", "low")
        );
        assert_eq!(
            config.subagent_roles[crate::config::SUBAGENT_ROLE_DEEP_RESEARCH],
            crate::config::SubagentRoleConfig::new(DEFAULT_SUBAGENT_MODEL, "high")
        );
        assert_eq!(config.subagent_model, DEFAULT_SUBAGENT_MODEL);
        assert_eq!(config.subagent_reasoning_effort, "low");
    }
}
