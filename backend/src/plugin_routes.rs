//! 把插件提交的线路描述写入 Codey 配置。插件不直接碰配置文件。
use std::collections::BTreeMap;

use crate::codey_plugins::PluginRouteSpec;
use crate::config::{
    AUTH_MODE_API_KEY, AUTH_MODE_OFFICIAL_ACCOUNT, CodeyConfig, MAX_ROUTE_NAME_CHARS,
    ProviderProfile, UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES,
    UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS, UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
};
use crate::local_router::ROUTER_PROVIDER_ID;
use crate::model_id;

const READ_ONLY_ROUTE_ERROR: &str = "本地路由已关闭，本地线路配置当前为只读；请先启用本地路由";

pub(crate) fn upsert(
    config: &mut CodeyConfig,
    plugin_id: &str,
    mut spec: PluginRouteSpec,
    create_if_missing: bool,
) -> Result<Option<String>, String> {
    if !config.local_router_enabled {
        if create_if_missing {
            return Err(READ_ONLY_ROUTE_ERROR.to_string());
        }
        return Ok(None);
    }
    spec = normalize_spec(spec)?;
    let owned = owned_indexes(config, plugin_id);
    let primary = owned
        .iter()
        .copied()
        .find(|index| structure_matches(&config.profiles[*index]));
    let reusable = primary.or_else(|| {
        owned
            .iter()
            .copied()
            .find(|index| config.profiles[*index].api_key.trim().is_empty())
    });
    let mut removed = Vec::new();
    for index in owned {
        if Some(index) == reusable {
            continue;
        }
        if config.profiles[index].api_key.trim().is_empty() {
            removed.push(config.profiles[index].provider_id().to_string());
        } else {
            detach(&mut config.profiles[index]);
        }
    }
    if let Some(index) = reusable {
        let provider_id = config.profiles[index].provider_id().to_string();
        let previous_models = config.profiles[index]
            .plugin_route_spec
            .as_ref()
            .map(|current| current.models.clone());
        apply_spec(&mut config.profiles[index], plugin_id, &spec, true);
        let profile_id = config.profiles[index].id.clone();
        assign_models(
            config,
            &provider_id,
            previous_models.as_deref(),
            &spec.models,
        );
        drop_removed(config, &removed);
        *config = std::mem::take(config).normalize();
        sync_stored_specs(config);
        return Ok(Some(profile_id));
    }
    if !create_if_missing {
        drop_removed(config, &removed);
        if !removed.is_empty() {
            *config = std::mem::take(config).normalize();
        }
        return Ok(None);
    }
    let mut profile = ProviderProfile::new(spec.name.clone());
    profile.short_name = config.allocate_route_short_name(&spec.name);
    apply_spec(&mut profile, plugin_id, &spec, false);
    let profile_id = profile.id.clone();
    let provider_id = profile.provider_id().to_string();
    if provider_id == ROUTER_PROVIDER_ID
        || crate::codex_config::RESERVED_BUILTIN_PROVIDER_IDS.contains(&provider_id.as_str())
    {
        return Err("插件线路不能使用保留的 Provider ID".into());
    }
    config.profiles.push(profile);
    assign_models(config, &provider_id, None, &spec.models);
    drop_removed(config, &removed);
    *config = std::mem::take(config).normalize();
    sync_stored_specs(config);
    Ok(Some(profile_id))
}

pub(crate) fn release(config: &mut CodeyConfig, plugin_id: &str) {
    let mut removed = Vec::new();
    let mut changed = false;
    for profile in &mut config.profiles {
        if profile.plugin_owner_id.as_deref() != Some(plugin_id) {
            continue;
        }
        changed = true;
        if profile.api_key.trim().is_empty() {
            removed.push(profile.provider_id().to_string());
        } else {
            detach(profile);
        }
    }
    if !changed {
        return;
    }
    drop_removed(config, &removed);
    *config = std::mem::take(config).normalize();
}

pub(crate) fn retain_plugin_ownership(
    profile: &mut ProviderProfile,
    previous: &ProviderProfile,
) -> Result<(), String> {
    profile.plugin_owner_id = previous.plugin_owner_id.clone();
    profile.plugin_route_spec = previous.plugin_route_spec.clone();
    if profile.plugin_owner_id.is_none() {
        profile.plugin_route_spec = None;
        return Ok(());
    }
    if structure_matches(profile) {
        return Ok(());
    }
    if profile.api_key.trim().is_empty() {
        return Err(format!(
            "修改插件线路「{}」的名称、地址、协议、短名称或请求头前，请先填写 API Key",
            profile.name.trim()
        ));
    }
    detach(profile);
    Ok(())
}

fn normalize_spec(mut spec: PluginRouteSpec) -> Result<PluginRouteSpec, String> {
    spec.name = spec.name.trim().to_string();
    if spec.name.is_empty() || spec.name.chars().count() > MAX_ROUTE_NAME_CHARS {
        return Err(format!(
            "插件线路名称需要 1 到 {MAX_ROUTE_NAME_CHARS} 个字符"
        ));
    }
    spec.base_url = spec.base_url.trim().trim_end_matches('/').to_string();
    crate::config::validate_outbound_api_url(&spec.base_url, "插件线路的 API URL")?;
    if !matches!(
        spec.upstream_protocol.as_str(),
        UPSTREAM_PROTOCOL_OPENAI_RESPONSES
            | UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS
            | UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES
    ) {
        return Err("插件线路协议不受支持".into());
    }
    spec.headers = spec
        .headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
        .collect();
    Ok(spec)
}

fn apply_spec(
    profile: &mut ProviderProfile,
    plugin_id: &str,
    spec: &PluginRouteSpec,
    keep_user_fields: bool,
) {
    profile.name = spec.name.clone();
    profile.base_url = spec.base_url.clone();
    profile.upstream_protocol = spec.upstream_protocol.clone();
    profile.model_request_headers = spec.headers.clone();
    profile.auth_mode = AUTH_MODE_API_KEY.to_string();
    profile.official_account = false;
    profile.official_account_id = None;
    profile.source_provider_id = None;
    if !keep_user_fields {
        profile.api_key.clear();
        profile.api_key_configured = false;
        profile.upstream_proxy.clear();
        profile.enabled = true;
        profile.supports_remote_compaction = false;
        profile.supports_websockets = false;
        profile.supports_native_web_search = false;
        profile.supports_auto_review = false;
    }
    profile.plugin_owner_id = Some(plugin_id.to_string());
    profile.normalize();
    let mut stored = spec.clone();
    stored.short_name = profile.short_name.clone();
    stored.base_url = profile.normalized_base_url();
    profile.plugin_route_spec = Some(stored);
}

fn sync_stored_specs(config: &mut CodeyConfig) {
    for profile in &mut config.profiles {
        let name = profile.name.clone();
        let base_url = profile.normalized_base_url();
        let short_name = profile.short_name.clone();
        let upstream_protocol = profile.upstream_protocol.clone();
        if let Some(spec) = &mut profile.plugin_route_spec {
            spec.name = name;
            spec.base_url = base_url;
            spec.short_name = short_name;
            spec.upstream_protocol = upstream_protocol;
        }
    }
}

fn detach(profile: &mut ProviderProfile) {
    profile.plugin_owner_id = None;
    profile.plugin_route_spec = None;
}

fn structure_matches(profile: &ProviderProfile) -> bool {
    let Some(spec) = &profile.plugin_route_spec else {
        return false;
    };
    profile.name.trim() == spec.name
        && profile.normalized_base_url() == spec.base_url
        && profile.upstream_protocol == spec.upstream_protocol
        && normalized_headers(&profile.model_request_headers) == spec.headers
        && profile.short_name == spec.short_name
        && profile.auth_mode.trim() != AUTH_MODE_OFFICIAL_ACCOUNT
        && !profile.official_account
        && profile.official_account_id.is_none()
        && profile.source_provider_id.is_none()
}

fn normalized_headers(headers: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    headers
        .iter()
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.clone()))
        .collect()
}

fn owned_indexes(config: &CodeyConfig, plugin_id: &str) -> Vec<usize> {
    config
        .profiles
        .iter()
        .enumerate()
        .filter(|(_, profile)| profile.plugin_owner_id.as_deref() == Some(plugin_id))
        .map(|(index, _)| index)
        .collect()
}

fn assign_models(
    config: &mut CodeyConfig,
    provider_id: &str,
    previous: Option<&[String]>,
    next: &[String],
) {
    if should_replace(
        config.upstream_models_by_provider.get(provider_id),
        previous,
    ) {
        config
            .upstream_models_by_provider
            .insert(provider_id.to_string(), next.to_vec());
    }
    if should_replace(
        config.selected_models_by_provider.get(provider_id),
        previous,
    ) {
        config
            .selected_models_by_provider
            .insert(provider_id.to_string(), next.to_vec());
    }
}

fn should_replace(current: Option<&Vec<String>>, previous: Option<&[String]>) -> bool {
    match (current, previous) {
        (None, _) => true,
        (Some(current), Some(previous)) => lists_match(current, previous),
        (Some(_), None) => false,
    }
}

fn lists_match(left: &[String], right: &[String]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| model_id::equal(left, right))
}

fn drop_removed(config: &mut CodeyConfig, provider_ids: &[String]) {
    if provider_ids.is_empty() {
        return;
    }
    config.profiles.retain(|profile| {
        !provider_ids
            .iter()
            .any(|provider_id| profile.provider_id() == provider_id)
    });
    forget_removed(config, provider_ids);
}

fn forget_removed(config: &mut CodeyConfig, provider_ids: &[String]) {
    for provider_id in provider_ids {
        config.selected_models_by_provider.remove(provider_id);
        config.model_context_by_provider.remove(provider_id);
        config
            .model_reasoning_efforts_by_provider
            .remove(provider_id);
        config
            .manual_third_party_models_by_provider
            .remove(provider_id);
        config
            .declared_official_models_by_provider
            .remove(provider_id);
        config.upstream_models_by_provider.remove(provider_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(url: &str) -> PluginRouteSpec {
        PluginRouteSpec {
            name: "示例线路".into(),
            base_url: url.into(),
            upstream_protocol: UPSTREAM_PROTOCOL_OPENAI_RESPONSES.into(),
            models: vec!["demo-model".into()],
            headers: BTreeMap::from([("x-region".into(), "us".into())]),
            short_name: String::new(),
        }
    }

    #[test]
    fn upsert_creates_a_keyless_route_and_release_removes_it() {
        let mut config = CodeyConfig::default();
        let id = upsert(
            &mut config,
            "dev.example",
            spec("https://relay.example/v1/"),
            true,
        )
        .unwrap()
        .unwrap();
        let profile = config
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .unwrap();
        assert_eq!(profile.plugin_owner_id.as_deref(), Some("dev.example"));
        assert_eq!(profile.normalized_base_url(), "https://relay.example/v1");
        assert!(profile.api_key.is_empty());
        assert!(profile.validate().is_ok());
        let provider_id = profile.provider_id().to_string();
        assert_eq!(
            config
                .selected_models_by_provider
                .get(&provider_id)
                .map(Vec::as_slice),
            Some(["demo-model".to_string()].as_slice())
        );
        release(&mut config, "dev.example");
        assert!(config.profiles.iter().all(|profile| profile.id != id));
        assert!(
            !config
                .selected_models_by_provider
                .contains_key(&provider_id)
        );
    }

    #[test]
    fn a_saved_key_keeps_the_route_after_release_and_a_later_enable_adds_another() {
        let mut config = CodeyConfig::default();
        let id = upsert(
            &mut config,
            "dev.example",
            spec("https://relay.example/v1"),
            true,
        )
        .unwrap()
        .unwrap();
        config
            .profiles
            .iter_mut()
            .find(|profile| profile.id == id)
            .unwrap()
            .api_key = "secret".into();
        release(&mut config, "dev.example");
        let kept = config
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .unwrap();
        assert!(kept.plugin_owner_id.is_none());
        assert_eq!(kept.api_key, "secret");
        let created = upsert(
            &mut config,
            "dev.example",
            spec("https://relay.example/v1"),
            true,
        )
        .unwrap()
        .unwrap();
        assert_ne!(created, id);
        assert_eq!(
            config
                .profiles
                .iter()
                .filter(|profile| profile.plugin_owner_id.is_some())
                .count(),
            1
        );
    }

    #[test]
    fn refresh_does_not_resurrect_a_deleted_route_or_replace_selected_models() {
        let mut config = CodeyConfig::default();
        let id = upsert(
            &mut config,
            "dev.example",
            spec("https://relay.example/v1"),
            true,
        )
        .unwrap()
        .unwrap();
        let provider_id = config
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .unwrap()
            .provider_id()
            .to_string();
        config
            .selected_models_by_provider
            .insert(provider_id.clone(), vec!["kept-model".into()]);
        let mut next = spec("https://relay.example/v2");
        next.models = vec!["demo-model".into(), "other-model".into()];
        upsert(&mut config, "dev.example", next, false).unwrap();
        let profile = config
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .unwrap();
        assert_eq!(profile.normalized_base_url(), "https://relay.example/v2");
        assert_eq!(
            config
                .selected_models_by_provider
                .get(&provider_id)
                .unwrap(),
            &vec!["kept-model".to_string()]
        );
        config.profiles.retain(|profile| profile.id != id);
        assert!(
            upsert(
                &mut config,
                "dev.example",
                spec("https://relay.example/v1"),
                false
            )
            .unwrap()
            .is_none()
        );
        assert!(
            config
                .profiles
                .iter()
                .all(|profile| profile.plugin_owner_id.is_none())
        );
    }

    #[test]
    fn changing_the_address_without_a_key_is_rejected_and_a_key_detaches_it() {
        let mut config = CodeyConfig::default();
        let id = upsert(
            &mut config,
            "dev.example",
            spec("https://relay.example/v1"),
            true,
        )
        .unwrap()
        .unwrap();
        let previous = config
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .unwrap()
            .clone();
        let mut edited = previous.clone();
        edited.base_url = "https://other.example/v1".into();
        let error = retain_plugin_ownership(&mut edited, &previous).unwrap_err();
        assert!(error.contains("API Key"), "{error}");
        edited.api_key = "secret".into();
        retain_plugin_ownership(&mut edited, &previous).unwrap();
        assert!(edited.plugin_owner_id.is_none());
        assert_eq!(edited.base_url, "https://other.example/v1");
    }

    #[test]
    fn closed_local_router_rejects_registration_and_keeps_the_existing_route() {
        let mut config = CodeyConfig::default();
        let id = upsert(
            &mut config,
            "dev.example",
            spec("https://relay.example/v1"),
            true,
        )
        .unwrap()
        .unwrap();
        config.local_router_enabled = false;
        let error = upsert(
            &mut config,
            "dev.example",
            spec("https://relay.example/v2"),
            true,
        )
        .unwrap_err();
        assert!(error.contains("本地路由已关闭"), "{error}");
        assert!(
            upsert(
                &mut config,
                "dev.example",
                spec("https://relay.example/v2"),
                false
            )
            .unwrap()
            .is_none()
        );
        let profile = config
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .unwrap();
        assert_eq!(profile.normalized_base_url(), "https://relay.example/v1");
        assert_eq!(profile.plugin_owner_id.as_deref(), Some("dev.example"));
    }

    #[test]
    fn route_descriptor_limits_match_the_config_constants() {
        assert_eq!(MAX_ROUTE_NAME_CHARS, 15);
        assert_eq!(UPSTREAM_PROTOCOL_OPENAI_RESPONSES, "openaiResponses");
        assert_eq!(
            UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
            "openaiChatCompletions"
        );
        assert_eq!(UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES, "anthropicMessages");
        assert_eq!(
            crate::local_router::CODEX_AUTO_REVIEW_MODEL,
            "codex-auto-review"
        );
    }
}
