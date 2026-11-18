use std::collections::BTreeMap;

use super::CodeyConfig;
use super::normalize::runtime_catalog_model_id;
use crate::model_catalog::{ModelSpeedPolicy, ModelSpeedTiers};
use crate::model_id;

pub(crate) fn can_configure_ultrafast(model: &str) -> bool {
    model_id::key(model)
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .split(['-', '_'])
        .any(|part| matches!(part, "astra" | "sol"))
}

impl CodeyConfig {
    /// 速度声明只应用于对应线路的启用模型；空声明阻止同名模板补回其他线路的能力。
    pub(crate) fn runtime_model_speed_tiers(&self) -> BTreeMap<String, ModelSpeedPolicy> {
        self.model_speed_policies(false)
    }

    /// 用户自备的目录只覆盖有上游声明或手动设置的条目。
    pub(crate) fn user_catalog_speed_overrides(&self) -> BTreeMap<String, ModelSpeedPolicy> {
        self.model_speed_policies(true)
    }

    pub(crate) fn needs_route_speed_catalog(&self) -> bool {
        self.local_router_enabled
            && self.runtime_profiles().any(|profile| {
                let models = self.enabled_profile_models(profile);
                !profile.official_siwc
                    && !models.is_empty()
                    && (self
                        .upstream_speed_tiers_by_provider
                        .contains_key(profile.provider_id())
                        || self
                            .model_ultrafast_by_provider
                            .get(profile.provider_id())
                            .is_some_and(|overrides| {
                                models
                                    .iter()
                                    .any(|model| overrides.contains_key(&model_id::key(model)))
                            }))
            })
    }

    /// 旧版共享快照只有在原生兼容线路归属明确时才能沿用。
    pub(crate) fn inherits_native_speed_tiers(&self, profile: &super::ProviderProfile) -> bool {
        let provider_id = profile.provider_id();
        profile.official_account
            && profile.official_account_id.is_none()
            && !profile
                .id
                .starts_with(&format!("{}-", super::DERIVED_OFFICIAL_PROFILE_ID))
            && !self.qualifies_official_model_ids()
            && !self
                .upstream_speed_tiers_by_provider
                .contains_key(provider_id)
            && !self.upstream_speed_tiers_by_provider.keys().any(|other| {
                super::is_official_profile_id(other)
                    || crate::codex_config::RESERVED_BUILTIN_PROVIDER_IDS.contains(&other.as_str())
                    || self.profiles.iter().any(|other_profile| {
                        other_profile.official_account && other_profile.provider_id() == other
                    })
            })
    }

    pub(crate) fn normalize_model_ultrafast(&mut self) {
        self.model_ultrafast_by_provider.retain(|provider, models| {
            if !self
                .profiles
                .iter()
                .any(|profile| !profile.official_account && profile.provider_id() == provider)
            {
                return false;
            }
            *models = std::mem::take(models)
                .into_iter()
                .filter_map(|(model, enabled)| {
                    can_configure_ultrafast(&model).then(|| (model_id::key(&model), enabled))
                })
                .collect();
            !models.is_empty()
        });
    }

    fn model_speed_policies(&self, user_catalog: bool) -> BTreeMap<String, ModelSpeedPolicy> {
        if !self.local_router_enabled {
            return BTreeMap::new();
        }
        let qualify_official = self.qualifies_official_model_ids();
        let mut policies = BTreeMap::new();
        for profile in self.runtime_profiles() {
            let provider_id = profile.provider_id();
            let synchronized = self.upstream_speed_tiers_by_provider.get(provider_id);
            let inherit_native = self.inherits_native_speed_tiers(profile);
            let overrides = (!profile.official_account)
                .then(|| self.model_ultrafast_by_provider.get(provider_id))
                .flatten();
            for model in self.enabled_profile_models(profile) {
                let key = model_id::key(&model);
                let declared = synchronized.and_then(|models| models.get(&key)).cloned();
                let ultrafast = overrides.and_then(|models| models.get(&key)).copied();
                if user_catalog
                    && !profile.official_siwc
                    && declared.is_none()
                    && ultrafast.is_none()
                {
                    continue;
                }
                let tiers = if profile.official_siwc {
                    Some(ModelSpeedTiers::default())
                } else {
                    declared.or_else(|| {
                        (!user_catalog && !inherit_native).then(ModelSpeedTiers::default)
                    })
                };
                let selector = runtime_catalog_model_id(profile, &model, qualify_official);
                policies.insert(
                    model_id::key(&selector),
                    ModelSpeedPolicy { tiers, ultrafast },
                );
            }
        }
        policies
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AUTH_MODE_OFFICIAL_ACCOUNT, ProviderProfile};
    use serde_json::json;

    fn route(id: &str, official: bool) -> ProviderProfile {
        let mut profile = ProviderProfile::new(id);
        profile.id = id.into();
        profile.base_url = "https://example.test/v1".into();
        if official {
            profile.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
        }
        profile.normalize();
        profile
    }

    fn config(profiles: Vec<ProviderProfile>) -> CodeyConfig {
        let selected_models_by_provider = profiles
            .iter()
            .map(|profile| {
                (
                    profile.provider_id().to_string(),
                    vec!["gpt-6-astra".into()],
                )
            })
            .collect();
        CodeyConfig {
            active_profile_id: profiles[0].id.clone(),
            profiles,
            selected_models_by_provider,
            official_account_available_this_launch: true,
            ..CodeyConfig::default()
        }
    }

    fn tiers() -> ModelSpeedTiers {
        ModelSpeedTiers::from_model(&json!({
            "service_tiers": [{ "id": "hyperspeed", "name": "New tier", "credits": 3 }],
            "additional_speed_tiers": ["hyperspeed"], "default_service_tier": "hyperspeed"
        }))
        .unwrap()
    }

    #[test]
    fn user_catalog_speed_overrides_keep_wire_and_picker_consistent() {
        let home = tempfile::tempdir().unwrap();
        let source = home.path().join("user.json");
        let original = serde_json::to_vec(&json!({"models": [{
            "slug":"relay/gpt-6-astra", "custom":true,
            "service_tiers":[{"id":"external","name":"External","description":"User catalog tier"}]
        }]}))
        .unwrap();
        std::fs::write(&source, &original).unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "model_catalog_json = 'user.json'\n",
        )
        .unwrap();
        let mut config = config(vec![route("relay", false)]);
        for (manual, declared) in [(false, false), (true, false), (false, true), (false, false)] {
            config.model_ultrafast_by_provider.clear();
            if manual {
                config.model_ultrafast_by_provider.insert(
                    "relay".into(),
                    BTreeMap::from([("gpt-6-astra".into(), true)]),
                );
            }
            config.upstream_speed_tiers_by_provider.insert(
                "relay".into(),
                if declared {
                    BTreeMap::from([("gpt-6-astra".into(), tiers())])
                } else {
                    BTreeMap::new()
                },
            );
            let required =
                crate::codex_config::runtime_required_catalog_models(&config, home.path(), true);
            let path =
                crate::codex_config::runtime_model_catalog_path(home.path(), true, None, &required)
                    .unwrap()
                    .unwrap();
            let catalog: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let model = &catalog["models"][0];
            assert_eq!(model["custom"], true);
            let picker = crate::model_catalog::renderer_speed_metadata(home.path(), &config);
            for id in ["external", "hyperspeed", "ultrafast"] {
                let expected = match id {
                    "external" => !declared,
                    "hyperspeed" => declared,
                    _ => manual,
                };
                for tiers in [
                    &model["service_tiers"],
                    &picker["relay/gpt-6-astra"]["service_tiers"],
                ] {
                    assert_eq!(
                        tiers
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|tier| tier["id"] == id),
                        expected,
                        "{id}"
                    );
                }
            }
            assert_eq!(path == source, !manual && !declared);
            assert_eq!(std::fs::read(&source).unwrap(), original);
        }
    }

    #[test]
    fn speed_policies_isolate_routes_and_keep_future_tiers() {
        let mut config = config(vec![
            route("one", true),
            route("two", true),
            route("relay", false),
        ]);
        config
            .selected_models_by_provider
            .get_mut("relay")
            .unwrap()
            .push("gpt-6.1-sol".into());
        config.model_ultrafast_by_provider.insert(
            "relay".into(),
            BTreeMap::from([("gpt-6-astra".into(), true)]),
        );
        config.upstream_speed_tiers_by_provider.insert(
            "one".into(),
            BTreeMap::from([("gpt-6-astra".into(), tiers())]),
        );
        config
            .upstream_speed_tiers_by_provider
            .insert("two".into(), BTreeMap::new());
        let policies = config.runtime_model_speed_tiers();
        assert_eq!(policies.len(), 4);
        assert_eq!(policies["one/gpt-6-astra"].tiers, Some(tiers()));
        assert_eq!(
            policies["two/gpt-6-astra"].tiers,
            Some(ModelSpeedTiers::default())
        );
        assert_eq!(
            policies["relay/gpt-6-astra"].tiers,
            Some(ModelSpeedTiers::default())
        );
        assert_eq!(policies["relay/gpt-6-astra"].ultrafast, Some(true));
        assert_eq!(policies["relay/gpt-6.1-sol"].ultrafast, None);
    }

    #[test]
    fn speed_policies_inherit_only_unambiguous_legacy_official_metadata() {
        let mut config = config(vec![route("one", true), route("two", true)]);
        assert!(
            config
                .runtime_model_speed_tiers()
                .values()
                .all(|policy| policy.tiers.is_some())
        );
        config.profiles[1].enabled = false;
        assert!(
            config.runtime_model_speed_tiers()["gpt-6-astra"]
                .tiers
                .is_none()
        );
        config.upstream_speed_tiers_by_provider.insert(
            "two".into(),
            BTreeMap::from([("gpt-6-astra".into(), tiers())]),
        );
        assert_eq!(
            config.runtime_model_speed_tiers()["gpt-6-astra"].tiers,
            Some(ModelSpeedTiers::default())
        );
        config
            .upstream_speed_tiers_by_provider
            .insert("one".into(), BTreeMap::new());
        assert_eq!(
            config.runtime_model_speed_tiers()["gpt-6-astra"].tiers,
            Some(ModelSpeedTiers::default())
        );
    }

    #[test]
    fn deleted_account_speed_declarations_never_reappear_on_an_unsynced_account() {
        let mut first = route("first", true);
        first.official_account_id = Some("first".into());
        let mut second = route("second", true);
        second.official_account_id = Some("second".into());
        let mut config = config(vec![first, second]);
        config.upstream_speed_tiers_by_provider.insert(
            "first".into(),
            BTreeMap::from([("gpt-6-astra".into(), tiers())]),
        );
        assert_eq!(
            config.runtime_model_speed_tiers()["second/gpt-6-astra"].tiers,
            Some(ModelSpeedTiers::default())
        );
        config.profiles.remove(0);
        config.upstream_speed_tiers_by_provider.remove("first");
        assert_eq!(
            config.runtime_model_speed_tiers()["gpt-6-astra"].tiers,
            Some(ModelSpeedTiers::default())
        );
        config.profiles[0].official_account_id = None;
        config.profiles[0].id = super::super::official_profile_id("second");
        config.profiles[0].source_provider_id = Some("second".into());
        assert_eq!(
            config.runtime_model_speed_tiers()["gpt-6-astra"].tiers,
            Some(ModelSpeedTiers::default())
        );
    }

    #[test]
    fn speed_policies_only_cover_available_enabled_models_and_skip_native_mode() {
        let mut config = config(vec![route("relay", false), route("official", true)]);
        config.upstream_speed_tiers_by_provider.insert(
            "relay".into(),
            BTreeMap::from([("disabled-model".into(), tiers())]),
        );
        config.official_account_available_this_launch = false;
        assert_eq!(
            config
                .runtime_model_speed_tiers()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["relay/gpt-6-astra"]
        );
        config.profiles[0].enabled = false;
        assert!(config.runtime_model_speed_tiers().is_empty());
        config.profiles[0].enabled = true;
        config.model_ultrafast_by_provider.insert(
            "relay".into(),
            BTreeMap::from([("gpt-6-astra".into(), true)]),
        );
        config.local_router_enabled = false;
        assert!(config.runtime_model_speed_tiers().is_empty());
    }

    #[test]
    fn siwc_speed_policies_clear_declared_tiers_without_losing_the_sync_result() {
        let mut config = config(vec![route("official", true)]);
        config.profiles[0].official_siwc = true;
        config.upstream_speed_tiers_by_provider.insert(
            "official".into(),
            BTreeMap::from([("gpt-6-astra".into(), tiers())]),
        );
        let policies = config.runtime_model_speed_tiers();
        assert_eq!(
            policies["gpt-6-astra"].tiers,
            Some(ModelSpeedTiers::default())
        );
        assert_eq!(policies["gpt-6-astra"].ultrafast, None);
        config.profiles[0].official_siwc = false;
        let policies = config.runtime_model_speed_tiers();
        assert_eq!(policies["gpt-6-astra"].tiers, Some(tiers()));
        assert_eq!(policies["gpt-6-astra"].ultrafast, None);
    }

    #[test]
    fn derived_official_refresh_preserves_only_each_accounts_sync_state() {
        let original = route("legacy", true);
        let mut config = config(vec![original]);
        config.upstream_speed_tiers_by_provider.insert(
            "legacy".into(),
            BTreeMap::from([("gpt-6-astra".into(), tiers())]),
        );
        let mut first = route("first", true);
        first.official_account_id = Some("first".into());
        let mut second = route("second", true);
        second.official_account_id = Some("second".into());
        config.apply_launch_official_profiles(vec![first.clone(), second.clone()]);
        assert!(
            config
                .upstream_speed_tiers_by_provider
                .contains_key("legacy")
        );
        assert!(
            !config
                .upstream_speed_tiers_by_provider
                .contains_key(config.profiles[0].provider_id())
        );
        let first_provider_id = config.profiles[0].provider_id().to_string();
        config.upstream_speed_tiers_by_provider.insert(
            first_provider_id.clone(),
            BTreeMap::from([("gpt-6-astra".into(), tiers())]),
        );
        config.apply_launch_official_profiles(vec![second, first]);
        assert_eq!(config.profiles[1].provider_id(), first_provider_id);
        assert_eq!(
            config.upstream_speed_tiers_by_provider[&first_provider_id]["gpt-6-astra"],
            tiers()
        );
        let restored: CodeyConfig =
            serde_json::from_value(serde_json::to_value(&config).unwrap()).unwrap();
        assert_eq!(
            restored.upstream_speed_tiers_by_provider,
            config.upstream_speed_tiers_by_provider
        );
    }
}
