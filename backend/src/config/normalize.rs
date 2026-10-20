use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;

use super::types::*;
use crate::{local_router, model_catalog, model_id};

pub(super) fn default_route_short_name(name: &str) -> String {
    name.trim().chars().take(2).collect()
}

/// Keeps one official account's preferred short name when it is still free and
/// otherwise numbers the following accounts so route-scoped model ids stay
/// readable.
pub(crate) fn unique_official_route_short_name(preferred: &str, used: &BTreeSet<String>) -> String {
    let preferred = preferred.trim();
    if !preferred.is_empty() && !used.contains(preferred) {
        return preferred.to_string();
    }
    let official_prefix = OFFICIAL_ROUTE_SHORT_NAME.chars().next().unwrap_or('官');
    let stem = preferred
        .chars()
        .next()
        .filter(|character| *character != official_prefix)
        .unwrap_or(official_prefix);
    for suffix in 1..=9 {
        let candidate = format!("{stem}{suffix}");
        if !used.contains(&candidate) {
            return candidate;
        }
        let official = format!("{official_prefix}{suffix}");
        if official != candidate && !used.contains(&official) {
            return official;
        }
    }
    let fallback = unique_default_route_short_name(preferred, used);
    if !used.contains(&fallback) {
        return fallback;
    }
    for codepoint in 0x4E00..=0x9FFF {
        let Some(character) = char::from_u32(codepoint) else {
            continue;
        };
        let candidate = character.to_string();
        if !used.contains(&candidate) {
            return candidate;
        }
    }
    fallback
}

pub(super) fn unique_default_route_short_name(name: &str, used: &BTreeSet<String>) -> String {
    let preferred = default_route_short_name(name);
    if !preferred.is_empty() && preferred != OFFICIAL_ROUTE_SHORT_NAME && !used.contains(&preferred)
    {
        return preferred;
    }

    let stem = preferred
        .chars()
        .next()
        .filter(|character| *character != '官')
        .unwrap_or('线');
    for suffix in 1..=9 {
        let candidate = format!("{stem}{suffix}");
        if !used.contains(&candidate) {
            return candidate;
        }
    }
    for suffix in 10..=99 {
        let candidate = suffix.to_string();
        if !used.contains(&candidate) {
            return candidate;
        }
    }
    for codepoint in 0x4E00..=0x9FFF {
        let Some(character) = char::from_u32(codepoint) else {
            continue;
        };
        let candidate = character.to_string();
        if candidate != OFFICIAL_ROUTE_SHORT_NAME && !used.contains(&candidate) {
            return candidate;
        }
    }
    preferred
}

pub(super) fn normalize_prompt_optimization_mode(value: &str) -> String {
    match value.trim() {
        PROMPT_OPTIMIZATION_MODE_CODEY_ROUTE => PROMPT_OPTIMIZATION_MODE_CODEY_ROUTE.to_string(),
        _ => PROMPT_OPTIMIZATION_MODE_MANUAL.to_string(),
    }
}
pub(super) fn normalize_prompt_optimization_upstream_protocol(value: &str) -> String {
    match value.trim() {
        UPSTREAM_PROTOCOL_OPENAI_RESPONSES
        | UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS
        | UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES => value.trim().to_string(),
        _ => default_prompt_optimization_upstream_protocol(),
    }
}

/// 线路上游代理地址。与 API URL 不同，代理地址允许携带用户名密码（代理认证）。
pub(crate) fn validate_outbound_proxy_url(value: &str, label: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| format!("{label}不是有效的代理地址（http/https/socks5/socks5h）"))?;
    if url.host_str().is_none() || !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h")
    {
        return Err(format!("{label}必须是 http、https、socks5 或 socks5h 地址"));
    }
    reqwest::Proxy::all(url.clone()).map_err(|_| format!("{label}无法用作代理"))?;
    let unusable_host = url
        .host_str()
        .and_then(|host| {
            host.trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|ip| match ip {
            std::net::IpAddr::V4(ip) => {
                ip.is_unspecified() || ip.is_link_local() || ip.is_broadcast()
            }
            std::net::IpAddr::V6(ip) => ip.is_unspecified() || ip.is_unicast_link_local(),
        });
    if unusable_host {
        return Err(format!("{label}不能指向未指定地址或链路本地地址"));
    }
    Ok(())
}

/// Codex 启动代理地址。除线路上游代理的通用要求外，这里叠加更严的约束：
/// 只允许协议、地址、端口和认证信息；SOCKS 地址必须显式端口，其
/// RFC 1929 凭据限长 255 字节。凭据不会交给 Chromium 命令行，启动时由
/// 本地认证中继注入上游。
pub(crate) fn validate_codex_launch_proxy(value: &str) -> Result<(), String> {
    validate_outbound_proxy_url(value, "Codex 启动代理")?;
    let url = reqwest::Url::parse(value).expect("启动代理地址已通过校验");
    // 非特殊协议（socks5/socks5h）解析出的 path 是空串而非 "/"。
    if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
        return Err("Codex 启动代理只能包含协议、地址、端口和认证信息".into());
    }
    if matches!(url.scheme(), "socks5" | "socks5h") {
        if url.port().is_none() {
            return Err("Codex 启动代理的 SOCKS 地址必须带端口".into());
        }
        let username = percent_encoding::percent_decode_str(url.username()).collect::<Vec<u8>>();
        let password = percent_encoding::percent_decode_str(url.password().unwrap_or_default())
            .collect::<Vec<u8>>();
        if crate::launch_proxy_relay::socks_credentials_too_long(&username, &password) {
            return Err("Codex 启动代理的 SOCKS 用户名或密码不能超过 255 字节".into());
        }
    }
    Ok(())
}

pub(crate) fn validate_outbound_api_url(value: &str, label: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(value).map_err(|_| format!("{label}不是有效的 HTTP(S) 地址"))?;
    if url.host_str().is_none() || !matches!(url.scheme(), "http" | "https") {
        return Err(format!("{label}必须是有效的 HTTP(S) 地址"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!(
            "{label}不能包含用户名或密码，请通过 API Key 单独配置凭据"
        ));
    }
    // Loopback and private ranges stay allowed (local model servers such as
    // Ollama are a supported target). Unspecified and link-local literals are
    // never a usable API host and usually mean a pasted metadata address.
    let unusable_host = url
        .host_str()
        .and_then(|host| {
            host.trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|ip| match ip {
            std::net::IpAddr::V4(ip) => {
                ip.is_unspecified() || ip.is_link_local() || ip.is_broadcast()
            }
            std::net::IpAddr::V6(ip) => ip.is_unspecified() || ip.is_unicast_link_local(),
        });
    if unusable_host {
        return Err(format!("{label}不能指向未指定地址或链路本地地址"));
    }
    Ok(url)
}
pub(super) fn normalize_model_reasoning_effort_values(efforts: &mut Vec<ModelReasoningEffort>) {
    for effort in efforts.iter_mut() {
        effort.level = effort.level.trim().to_string();
        effort.value = effort.value.trim().to_string();
    }
    let mut seen = std::collections::HashSet::new();
    efforts.retain(|effort| {
        !effort.value.is_empty()
            && effort.value.len() <= MAX_MODEL_REASONING_EFFORT_VALUE_BYTES
            && MODEL_REASONING_EFFORT_LEVELS.contains(&effort.level.as_str())
            && seen.insert(effort.level.clone())
    });
    efforts.truncate(MAX_MODEL_REASONING_EFFORTS);
}
impl CodeyConfig {
    pub fn normalize(mut self) -> Self {
        self.update_manifest_url = default_update_manifest_url();
        self.release_admin_url = default_release_admin_url();
        self.stream_max_retries = self.stream_max_retries.min(100);
        self.route_request_log.normalize();
        self.profiles
            .retain(|profile| !profile.id.trim().is_empty());
        for profile in &mut self.profiles {
            profile.normalize();
        }
        let mut used_short_names = self
            .profiles
            .iter()
            .map(|profile| profile.short_name.clone())
            .filter(|short_name| !short_name.is_empty())
            .collect::<BTreeSet<_>>();
        for profile in &mut self.profiles {
            if profile.short_name.is_empty() && !profile.is_unconfigured_default() {
                profile.short_name = if profile.official_account {
                    unique_official_route_short_name(
                        &profile.display_short_name(),
                        &used_short_names,
                    )
                } else {
                    unique_default_route_short_name(&profile.name, &used_short_names)
                };
                used_short_names.insert(profile.short_name.clone());
            }
        }
        if self.profiles.is_empty() {
            let profile = ProviderProfile::new("默认配置");
            self.active_profile_id = profile.id.clone();
            self.profiles.push(profile);
        }
        if !self
            .profiles
            .iter()
            .any(|profile| profile.enabled && profile.id == self.active_profile_id)
        {
            self.active_profile_id = self
                .profiles
                .iter()
                .find(|profile| profile.enabled)
                .unwrap_or(&self.profiles[0])
                .id
                .clone();
        }
        normalize_model_lists(&mut self.selected_models_by_provider);
        self.prune_retired_official_selections();
        normalize_model_reasoning_effort_lists(&mut self.model_reasoning_efforts_by_provider);
        normalize_model_lists(&mut self.manual_third_party_models_by_provider);
        normalize_model_lists(&mut self.declared_official_models_by_provider);
        normalize_upstream_model_lists(&mut self.upstream_models_by_provider);
        merge_declared_official_models_into_upstream(
            &self.declared_official_models_by_provider,
            &mut self.upstream_models_by_provider,
        );
        self.remember_model_aliases();
        self.normalize_global_default_model();
        normalize_subagent_config(
            &mut self.subagent_model,
            &mut self.subagent_reasoning_effort,
            &mut self.subagent_roles,
        );
        self.normalize_subagent_model_references();
        self.normalize_misc_model();
        if !self.initial_route_import_completed && !self.looks_like_empty_default_route() {
            self.initial_route_import_completed = true;
        }
        self.webhook.normalize();
        self.prompt_optimization.normalize();
        self.codex_launch_proxy = self.codex_launch_proxy.trim().to_string();
        self
    }

    /// Replaces every derived official route with one route per stored
    /// account. Route names, short names and proxies come from the account
    /// record, so the same account keeps its settings across launches.
    pub(crate) fn apply_launch_official_profiles(
        &mut self,
        official_profiles: Vec<ProviderProfile>,
    ) {
        let previous_active_id = self.active_profile_id.clone();
        // 没有独立账号记录的兼容线路仍从配置保存并发值；已存储账号始终
        // 使用账号记录中的值，不能被旧配置覆盖。
        let legacy_max_concurrency = self
            .profiles
            .iter()
            .find(|profile| profile.official_account && profile.official_account_id.is_none())
            .map(|profile| profile.max_concurrency)
            .unwrap_or(0);
        let previous_official_profiles = self
            .profiles
            .iter()
            .filter(|profile| profile.official_account)
            .map(|profile| {
                (
                    profile.provider_id().to_string(),
                    profile.official_account_id.clone(),
                    profile.enabled,
                )
            })
            .collect::<Vec<_>>();
        let placeholder_provider_id = self
            .looks_like_empty_default_route()
            .then(|| self.profiles[0].provider_id().to_string());
        if self.looks_like_empty_default_route() {
            self.profiles.clear();
        } else {
            self.profiles.retain(|profile| !profile.official_account);
        }
        // The launch-derived official profile may disappear on an API-key
        // launch and return on a later official launch. Only the disposable
        // empty placeholder owns route-scoped data that can be removed.
        if let Some(provider_id) = placeholder_provider_id {
            self.selected_models_by_provider.remove(&provider_id);
            self.model_context_by_provider.remove(&provider_id);
            self.model_reasoning_efforts_by_provider
                .remove(&provider_id);
            self.manual_third_party_models_by_provider
                .remove(&provider_id);
            self.declared_official_models_by_provider
                .remove(&provider_id);
            self.upstream_models_by_provider.remove(&provider_id);
        }
        if !official_profiles.is_empty() {
            // Official and third-party routes share the short-name namespace,
            // so third-party names are reserved before the official ones.
            let mut used_short_names = self
                .profiles
                .iter()
                .map(|profile| profile.short_name.trim().to_string())
                .filter(|short_name| !short_name.is_empty())
                .collect::<BTreeSet<_>>();
            let mut derived = Vec::with_capacity(official_profiles.len());
            for (index, mut official_profile) in official_profiles.into_iter().enumerate() {
                let account_id = official_profile.official_account_id.clone();
                match account_id.as_deref() {
                    // 每个账号拥有独立的 Provider ID，同一段对话里可以并存多条
                    // 官方线路，同名模型也能区分。
                    Some(account_id) => {
                        official_profile.id = official_profile_id(account_id);
                        official_profile.source_provider_id = None;
                    }
                    // 升级前没有账号记录时保留原有 Provider ID，Codex 仍按官方
                    // 登录直接访问。
                    None => {
                        official_profile.id = DERIVED_OFFICIAL_PROFILE_ID.to_string();
                        official_profile.max_concurrency = legacy_max_concurrency;
                    }
                }
                let account_key = account_id.unwrap_or_default();
                official_profile.enabled = previous_official_profiles
                    .iter()
                    .find(|(_, previous_account_id, _)| {
                        previous_account_id.as_deref() == Some(account_key.as_str())
                    })
                    .map(|(_, _, enabled)| *enabled)
                    .or_else(|| {
                        // 升级前只有一条官方线路，它的启用状态留给默认账号。
                        (index == 0)
                            .then(|| {
                                previous_official_profiles
                                    .iter()
                                    .find(|(_, previous_account_id, _)| {
                                        previous_account_id.is_none()
                                    })
                                    .map(|(_, _, enabled)| *enabled)
                            })
                            .flatten()
                    })
                    .unwrap_or(true);
                official_profile.normalize();
                official_profile.short_name = unique_official_route_short_name(
                    &official_profile.display_short_name(),
                    &used_short_names,
                );
                used_short_names.insert(official_profile.short_name.clone());
                derived.push(official_profile);
            }
            // 旧版本只保留一条官方线路，把它的模型选择等状态交给默认账号。
            if let Some(default_provider_id) = derived.first().map(|profile| profile.provider_id())
            {
                let default_provider_id = default_provider_id.to_string();
                for (previous_provider_id, previous_account_id, _) in &previous_official_profiles {
                    if previous_account_id.is_some() {
                        continue;
                    }
                    self.migrate_official_provider_state(
                        previous_provider_id,
                        &default_provider_id,
                    );
                }
            }
            for (index, official_profile) in derived.into_iter().enumerate() {
                let official_provider_id = official_profile.provider_id().to_string();
                self.selected_models_by_provider
                    .entry(official_provider_id)
                    .or_insert_with(model_catalog::default_official_model_slugs);
                self.profiles.insert(index, official_profile);
            }
        }
        if self
            .profiles
            .iter()
            .any(|profile| profile.id == previous_active_id)
        {
            self.active_profile_id = previous_active_id;
        } else if let Some(profile) = self.profiles.first() {
            self.active_profile_id = profile.id.clone();
        }
    }

    fn migrate_official_provider_state(
        &mut self,
        previous_provider_id: &str,
        official_provider_id: &str,
    ) {
        if previous_provider_id == official_provider_id {
            return;
        }
        if let Some(models) = self.model_context_by_provider.remove(previous_provider_id) {
            let target = self
                .model_context_by_provider
                .entry(official_provider_id.to_string())
                .or_default();
            for (model, policy) in models {
                target.entry(model).or_insert(policy);
            }
        }
        if let Some(models) = self
            .model_reasoning_efforts_by_provider
            .remove(previous_provider_id)
        {
            let target = self
                .model_reasoning_efforts_by_provider
                .entry(official_provider_id.to_string())
                .or_default();
            for (model, efforts) in models {
                target.entry(model).or_insert(efforts);
            }
        }
        migrate_provider_model_list(
            &mut self.selected_models_by_provider,
            previous_provider_id,
            official_provider_id,
        );
        migrate_provider_model_list(
            &mut self.manual_third_party_models_by_provider,
            previous_provider_id,
            official_provider_id,
        );
        migrate_provider_model_list(
            &mut self.declared_official_models_by_provider,
            previous_provider_id,
            official_provider_id,
        );
        migrate_provider_model_list(
            &mut self.upstream_models_by_provider,
            previous_provider_id,
            official_provider_id,
        );
        remap_model_provider_alias(
            &mut self.default_model,
            previous_provider_id,
            official_provider_id,
        );
        remap_model_provider_alias(
            &mut self.subagent_model,
            previous_provider_id,
            official_provider_id,
        );
        for selection in self.subagent_roles.values_mut() {
            remap_model_provider_alias(
                &mut selection.model,
                previous_provider_id,
                official_provider_id,
            );
        }
    }

    pub fn active_profile(&self) -> Option<ProviderProfile> {
        self.profiles
            .iter()
            .find(|profile| profile.id == self.active_profile_id)
            .cloned()
            .or_else(|| self.profiles.first().cloned())
    }

    pub fn current_provider_id(&self) -> Option<&str> {
        self.profiles
            .iter()
            .find(|profile| profile.id == self.active_profile_id)
            .map(ProviderProfile::provider_id)
    }

    pub fn selected_models(&self) -> &[String] {
        self.current_provider_id()
            .and_then(|provider_id| self.selected_models_by_provider.get(provider_id))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(crate) fn model_context(
        &self,
        provider_id: &str,
        model: &str,
    ) -> Option<&ModelContextConfig> {
        self.model_context_by_provider
            .get(provider_id)?
            .iter()
            .find(|(candidate, _)| model_id::equal(candidate, model))
            .map(|(_, policy)| policy)
    }

    pub(crate) fn runtime_plugin_model_contexts(&self) -> BTreeMap<String, ModelContextConfig> {
        let qualify_official = self.qualifies_official_model_ids();
        let mut contexts = BTreeMap::new();
        for profile in self
            .profiles
            .iter()
            .filter(|p| p.enabled && p.plugin_owner_id.is_some())
        {
            if let Some(transport) = profile
                .plugin_route_spec
                .as_ref()
                .and_then(|s| s.transport.as_ref())
            {
                for (model, caps) in &transport.models {
                    contexts.insert(
                        runtime_catalog_model_id(profile, model, qualify_official),
                        ModelContextConfig {
                            context_window_tokens: caps.context_window,
                            auto_compact_token_limit: Some(caps.auto_compact_token_limit),
                            reserve_output_tokens: None,
                        },
                    );
                }
            }
        }
        contexts
    }

    pub(crate) fn runtime_model_contexts(&self) -> BTreeMap<String, ModelContextConfig> {
        let qualify_official = self.qualifies_official_model_ids();
        self.profiles
            .iter()
            .filter(|profile| profile.enabled)
            .filter(|profile| !profile.official_account || self.official_route_usable(profile))
            .flat_map(|profile| {
                self.model_context_by_provider
                    .get(profile.provider_id())
                    .into_iter()
                    .flat_map(move |models| {
                        models.iter().map(move |(model, policy)| {
                            (
                                runtime_catalog_model_id(profile, model, qualify_official),
                                policy.clone(),
                            )
                        })
                    })
            })
            .collect()
    }

    /// 运行时覆盖仅针对启用的模型；未勾选模型的预算仍保留在配置里。
    pub(crate) fn runtime_enabled_model_contexts(&self) -> BTreeMap<String, ModelContextConfig> {
        if !self.local_router_enabled {
            return BTreeMap::new();
        }
        let policies = self
            .runtime_plugin_model_contexts()
            .into_iter()
            .chain(self.runtime_model_contexts())
            .map(|(model, policy)| (model_id::key(&model), policy))
            .collect::<BTreeMap<_, _>>();
        self.runtime_catalog_models()
            .1
            .into_iter()
            .filter_map(|model| {
                policies
                    .get(&model_id::key(&model))
                    .cloned()
                    .map(|policy| (model, policy))
            })
            .collect()
    }

    /// Declared thinking levels keyed by the runtime catalog id of each model.
    pub(crate) fn runtime_model_reasoning_efforts(
        &self,
    ) -> BTreeMap<String, Vec<ModelReasoningEffort>> {
        let qualify_official = self.qualifies_official_model_ids();
        self.profiles
            .iter()
            .filter(|profile| profile.enabled)
            .filter(|profile| !profile.official_account || self.official_route_usable(profile))
            .flat_map(|profile| {
                self.model_reasoning_efforts_for_provider(profile.provider_id())
                    .into_iter()
                    .map(move |(model, efforts)| {
                        (
                            runtime_catalog_model_id(profile, &model, qualify_official),
                            efforts,
                        )
                    })
            })
            .collect()
    }

    /// Returns the effective thinking levels for one route. Plugin declarations
    /// form the route capability boundary; user declarations can customize the
    /// upstream value for an allowed level but cannot add a new level.
    pub(crate) fn model_reasoning_efforts_for_provider(
        &self,
        provider_id: &str,
    ) -> BTreeMap<String, Vec<ModelReasoningEffort>> {
        let Some(profile) = self
            .profiles
            .iter()
            .find(|profile| profile.provider_id() == provider_id)
        else {
            return self
                .model_reasoning_efforts_by_provider
                .get(provider_id)
                .cloned()
                .unwrap_or_default();
        };
        let capabilities = profile
            .plugin_route_spec
            .as_ref()
            .map(|spec| spec.model_reasoning_efforts.clone())
            .unwrap_or_default();
        let mut effective = capabilities
            .iter()
            .map(|(model, levels)| {
                (
                    model.clone(),
                    levels
                        .iter()
                        .map(|level| ModelReasoningEffort {
                            level: level.clone(),
                            value: level.clone(),
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let Some(user_models) = self.model_reasoning_efforts_by_provider.get(provider_id) else {
            return effective;
        };
        for (model, configured) in user_models {
            let capability = capabilities
                .iter()
                .find(|(candidate, _)| model_id::equal(candidate, model));
            let Some((canonical_model, allowed_levels)) = capability else {
                effective.insert(model.clone(), configured.clone());
                continue;
            };
            let mut seen = BTreeSet::new();
            let mut values = Vec::new();
            for effort in configured {
                let (level, value) = if allowed_levels
                    .iter()
                    .any(|allowed| allowed == &effort.level)
                {
                    let value = if matches!(effort.value.as_str(), "max" | "ultra")
                        && !allowed_levels
                            .iter()
                            .any(|allowed| allowed == &effort.value)
                    {
                        effort.level.clone()
                    } else {
                        effort.value.clone()
                    };
                    (effort.level.clone(), value)
                } else if matches!(effort.level.as_str(), "max" | "ultra")
                    && allowed_levels.iter().any(|allowed| allowed == "xhigh")
                {
                    ("xhigh".to_string(), "xhigh".to_string())
                } else {
                    continue;
                };
                if seen.insert(level.clone()) {
                    values.push(ModelReasoningEffort { level, value });
                }
            }
            if !values.is_empty() {
                effective.insert(canonical_model.clone(), values);
            }
        }
        effective
    }

    pub(crate) fn provider_is_disabled(&self, provider_id: &str) -> bool {
        self.profiles
            .iter()
            .find(|profile| profile.id == provider_id || profile.provider_id() == provider_id)
            .is_some_and(|profile| !profile.enabled)
    }

    pub(crate) fn retain_model_contexts(&mut self, provider_id: &str, available: &[String]) {
        if let Some(models) = self.model_context_by_provider.get_mut(provider_id) {
            models.retain(|model, _| {
                available
                    .iter()
                    .any(|candidate| model_id::equal(candidate, model))
            });
        }
        if let Some(models) = self
            .model_reasoning_efforts_by_provider
            .get_mut(provider_id)
        {
            models.retain(|model, _| {
                available
                    .iter()
                    .any(|candidate| model_id::equal(candidate, model))
            });
        }
    }

    /// Models enabled on an API-key route. Legacy official-looking model IDs
    /// are stored separately for backward compatibility, but they still belong
    /// to this route and must be routed by provenance rather than by name.
    pub(crate) fn enabled_route_models(&self, provider_id: &str) -> Vec<String> {
        let selected = self
            .selected_models_by_provider
            .get(provider_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let declared = self
            .declared_official_models_by_provider
            .get(provider_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        model_id::dedupe_preserving_first(
            selected
                .iter()
                .chain(declared.iter())
                .map(String::as_str)
                .filter(|model| !model_id::equal(model, local_router::CODEX_AUTO_REVIEW_MODEL)),
        )
    }

    pub(crate) fn enabled_official_route_models(&self, provider_id: &str) -> Vec<String> {
        self.selected_models_by_provider
            .get(provider_id)
            .filter(|models| !models.is_empty())
            .cloned()
            .unwrap_or_else(model_catalog::default_official_model_slugs)
    }

    /// 官方线路只能选择内置官方模型清单里的模型。上游下线某个模型后，旧配置里
    /// 遗留的选择会在这里清掉，避免模型界面和生成的目录继续出现它。
    fn prune_retired_official_selections(&mut self) {
        let official_provider_ids = self
            .profiles
            .iter()
            .filter(|profile| profile.resolves_to_official_account_route())
            .map(|profile| profile.provider_id().to_string())
            .collect::<BTreeSet<_>>();
        if official_provider_ids.is_empty() {
            return;
        }
        let supported_models = official_models_by_key();
        let synchronized_models = self
            .upstream_models_by_provider
            .iter()
            .filter(|(_, models)| !models.is_empty())
            .map(|(provider_id, models)| {
                (
                    provider_id.clone(),
                    models
                        .iter()
                        .map(|model| model_id::key(model))
                        .collect::<BTreeSet<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        self.selected_models_by_provider
            .retain(|provider_id, models| {
                if !official_provider_ids.contains(provider_id) {
                    return true;
                }
                // A synchronized official account may expose models newer
                // than Codey's built-in fallback list. The per-route upstream
                // snapshot is authoritative when present; otherwise retain
                // the historical fixed-list behavior.
                let route_models = synchronized_models.get(provider_id);
                models.retain(|model| {
                    route_models
                        .as_ref()
                        .is_some_and(|known| known.contains(&model_id::key(model)))
                        || (route_models.is_none()
                            && supported_models.contains_key(&model_id::key(model)))
                });
                !models.is_empty()
            });
    }

    /// Whether official ChatGPT routes can be served this launch.
    ///
    /// The loopback provider keeps Codex's native OpenAI authentication when
    /// an official route is available. A separate Codey-only header protects
    /// the local gateway, which still replaces authentication per route before
    /// forwarding third-party traffic.
    pub(crate) fn router_requires_openai_auth(&self) -> bool {
        self.official_account_available_this_launch
            && self
                .profiles
                .iter()
                .any(|profile| profile.enabled && profile.official_account)
    }

    /// Whether one derived official route can be used this launch. Routes
    /// owned by a stored account carry that account's own credentials, so they
    /// no longer depend on the Codex login of the default account.
    pub(crate) fn official_route_usable(&self, profile: &ProviderProfile) -> bool {
        profile.official_account
            && (self.official_account_available_this_launch
                || (self.local_router_enabled && profile.official_account_id.is_some()))
    }

    /// Derived official routes of the accounts that are live this launch.
    pub(crate) fn usable_official_routes(&self) -> impl Iterator<Item = &ProviderProfile> {
        self.profiles
            .iter()
            .filter(|profile| profile.enabled && self.official_route_usable(profile))
    }

    /// 官方模型只在多条官方线路并存时才带上线路前缀，单个账号的展示保持原样。
    pub(crate) fn qualifies_official_model_ids(&self) -> bool {
        self.local_router_enabled && self.usable_official_routes().count() > 1
    }

    pub(crate) fn runtime_gateway_provider_id_for_profile(
        &self,
        _profile: &ProviderProfile,
    ) -> &'static str {
        local_router::ROUTER_PROVIDER_ID
    }

    /// Whether a route can use upstream Responses WebSocket this launch.
    /// Official ChatGPT-account routes enable it automatically once login is
    /// available; third-party routes must explicitly declare Responses WS.
    pub(crate) fn route_supports_websockets_this_launch(&self, profile: &ProviderProfile) -> bool {
        self.route_supports_websockets_this_launch_with_proxy(
            profile,
            local_router::outbound_proxy_applies_to_route(profile),
        )
    }

    pub(super) fn route_supports_websockets_this_launch_with_proxy(
        &self,
        profile: &ProviderProfile,
        outbound_proxy_configured: bool,
    ) -> bool {
        if !profile.enabled
            || outbound_proxy_configured
            || profile
                .plugin_route_spec
                .as_ref()
                .is_some_and(|s| s.transport.is_some())
        {
            return false;
        }
        if profile.official_account {
            // SIWC 直连只提供公开 API 的 HTTP Responses，没有上游 WS。
            return self.official_route_usable(profile) && !profile.official_siwc;
        }
        profile.supports_websockets
            && profile.upstream_protocol == UPSTREAM_PROTOCOL_OPENAI_RESPONSES
    }

    /// Runtime catalog model IDs that use Responses WebSocket.
    pub(crate) fn runtime_websocket_model_aliases(&self) -> Vec<String> {
        if !self.runtime_supports_websockets() {
            return Vec::new();
        }
        let qualify_official = self.qualifies_official_model_ids();
        self.profiles
            .iter()
            .filter(|profile| self.route_supports_websockets_this_launch(profile))
            .flat_map(|profile| {
                let provider_id = profile.provider_id();
                let models = if profile.official_account {
                    self.enabled_official_route_models(provider_id)
                } else {
                    self.enabled_route_models(provider_id)
                };
                models
                    .into_iter()
                    .map(move |model| runtime_catalog_model_id(profile, &model, qualify_official))
            })
            .collect()
    }

    /// Codex gates WebSocket transport at the provider level, then applies each
    /// model's `prefer_websockets` setting from the runtime catalog.
    pub(crate) fn runtime_supports_websockets(&self) -> bool {
        self.profiles.iter().any(|profile| {
            !profile.provider_id().trim().is_empty()
                && (profile.official_account || !profile.normalized_base_url().is_empty())
                && self.route_supports_websockets_this_launch(profile)
        })
    }

    /// Whether one route can expose Codex's native Responses Web Search tool
    /// this launch. A third-party route must opt in and use the native
    /// Responses protocol; adapted Chat/Anthropic routes never qualify.
    pub(crate) fn route_supports_native_web_search_this_launch(
        &self,
        profile: &ProviderProfile,
    ) -> bool {
        if !profile.enabled
            || profile
                .plugin_route_spec
                .as_ref()
                .is_some_and(|s| s.transport.is_some())
        {
            return false;
        }
        if profile.official_account {
            return self.official_route_usable(profile);
        }
        profile.supports_native_web_search
            && profile.upstream_protocol == UPSTREAM_PROTOCOL_OPENAI_RESPONSES
    }

    /// Runtime model IDs that may retain native Web Search metadata in
    /// the generated runtime catalog. The catalog applies a second gate and
    /// only preserves the capability when the source model metadata declares
    /// support as well.
    pub(crate) fn runtime_native_web_search_model_aliases(&self) -> Vec<String> {
        let qualify_official = self.qualifies_official_model_ids();
        self.profiles
            .iter()
            .filter(|profile| self.route_supports_native_web_search_this_launch(profile))
            .flat_map(|profile| {
                let provider_id = profile.provider_id();
                let models = if profile.official_account {
                    self.enabled_official_route_models(provider_id)
                } else {
                    self.enabled_route_models(provider_id)
                };
                models
                    .into_iter()
                    .map(move |model| runtime_catalog_model_id(profile, &model, qualify_official))
            })
            .collect()
    }

    /// Whether one route can carry the Responses `input_image.detail=original`
    /// hint end to end. Only the native Responses protocol preserves it;
    /// adapted Chat Completions and Anthropic routes would have to drop or
    /// rewrite the value, so their catalog entries must not advertise it.
    pub(crate) fn route_supports_image_detail_original_this_launch(
        &self,
        profile: &ProviderProfile,
    ) -> bool {
        if !profile.enabled {
            return false;
        }
        if profile.official_account {
            return self.official_route_usable(profile);
        }
        profile.upstream_protocol == UPSTREAM_PROTOCOL_OPENAI_RESPONSES
    }

    /// Runtime catalog model IDs that may keep `supports_image_detail_original`.
    pub(crate) fn runtime_image_detail_original_model_aliases(&self) -> Vec<String> {
        let qualify_official = self.qualifies_official_model_ids();
        self.profiles
            .iter()
            .filter(|profile| self.route_supports_image_detail_original_this_launch(profile))
            .flat_map(|profile| {
                let provider_id = profile.provider_id();
                let models = if profile.official_account {
                    self.enabled_official_route_models(provider_id)
                } else {
                    self.enabled_route_models(provider_id)
                };
                models
                    .into_iter()
                    .map(move |model| runtime_catalog_model_id(profile, &model, qualify_official))
            })
            .collect()
    }

    /// Whether one route natively supports the Responses compaction contract
    /// this launch, including the current `/responses` trigger flow and the
    /// legacy standalone compact endpoint.
    pub(crate) fn route_supports_remote_compaction_this_launch(
        &self,
        profile: &ProviderProfile,
    ) -> bool {
        if !profile.enabled
            || profile
                .plugin_route_spec
                .as_ref()
                .is_some_and(|s| s.transport.is_some())
        {
            return false;
        }
        if profile.official_account {
            // 远程压缩走 Codex backend-api 的 /responses/compact，SIWC 直连
            // 线路没有该端点。
            return self.official_route_usable(profile) && !profile.official_siwc;
        }
        profile.supports_remote_compaction
            && profile.upstream_protocol == UPSTREAM_PROTOCOL_OPENAI_RESPONSES
    }

    /// 这里只汇总可用能力；压缩方式由网关按所选线路热切换。
    pub(crate) fn runtime_supports_remote_compaction(&self) -> bool {
        self.remote_compaction_runtime_profiles()
            .any(|profile| self.route_supports_remote_compaction_this_launch(profile))
    }

    pub(crate) fn remote_compaction_route_capabilities(&self) -> BTreeMap<String, bool> {
        self.remote_compaction_runtime_profiles()
            .map(|profile| {
                (
                    profile.provider_id().to_string(),
                    self.route_supports_remote_compaction_this_launch(profile),
                )
            })
            .collect()
    }

    pub(crate) fn remote_compaction_mode(&self) -> &'static str {
        let routes = self.remote_compaction_route_capabilities();
        if !routes.values().any(|enabled| *enabled) {
            "local"
        } else if routes.values().all(|enabled| *enabled) {
            "remote"
        } else {
            "mixed"
        }
    }

    pub(crate) fn runtime_remote_compaction_model_aliases(&self) -> Vec<String> {
        let qualify_official = self.qualifies_official_model_ids();
        self.remote_compaction_runtime_profiles()
            .filter(|profile| self.route_supports_remote_compaction_this_launch(profile))
            .flat_map(|profile| {
                let models = if profile.official_account {
                    self.enabled_official_route_models(profile.provider_id())
                } else {
                    self.enabled_route_models(profile.provider_id())
                };
                models
                    .into_iter()
                    .map(move |model| runtime_catalog_model_id(profile, &model, qualify_official))
            })
            .collect()
    }

    fn remote_compaction_runtime_profiles(&self) -> impl Iterator<Item = &ProviderProfile> {
        self.profiles.iter().filter(|profile| {
            profile.enabled
                && !profile.provider_id().trim().is_empty()
                && if profile.official_account {
                    self.official_route_usable(profile)
                } else {
                    !profile.normalized_base_url().is_empty()
                }
        })
    }

    pub(crate) fn remote_compaction_blockers(&self) -> Vec<RemoteCompactionBlocker> {
        self.remote_compaction_runtime_profiles()
            .filter(|profile| !self.route_supports_remote_compaction_this_launch(profile))
            .map(|profile| RemoteCompactionBlocker {
                route_id: profile.id.clone(),
                route_name: if profile.name.trim().is_empty() {
                    profile.display_short_name()
                } else {
                    profile.name.clone()
                },
                reason: if profile
                    .plugin_route_spec
                    .as_ref()
                    .is_some_and(|s| s.transport.is_some())
                {
                    "插件传输不支持原生压缩"
                } else if profile.upstream_protocol != UPSTREAM_PROTOCOL_OPENAI_RESPONSES {
                    "上游协议不支持原生压缩"
                } else {
                    "未开启远程压缩"
                },
            })
            .collect()
    }

    pub fn manual_third_party_models(&self) -> &[String] {
        self.current_provider_id()
            .and_then(|provider_id| self.manual_third_party_models_by_provider.get(provider_id))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn declared_official_models(&self) -> &[String] {
        self.current_provider_id()
            .and_then(|provider_id| self.declared_official_models_by_provider.get(provider_id))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn upstream_models_snapshot(&self) -> Option<&[String]> {
        self.current_provider_id()
            .and_then(|provider_id| self.upstream_models_by_provider.get(provider_id))
            .map(Vec::as_slice)
    }

    pub fn default_model(&self) -> Option<&str> {
        let model = self.default_model.trim();
        (!model.is_empty()).then_some(model)
    }

    pub(crate) fn default_model_for_profile(&self, profile: &ProviderProfile) -> Option<String> {
        let default_model = self.default_model()?;
        self.configured_model_targets()
            .into_iter()
            .find(|target| {
                target.route_id == profile.id && model_id::equal(&target.alias, default_model)
            })
            .map(|target| target.upstream_model)
    }

    pub(crate) fn model_target_for_route(
        &self,
        route_id: &str,
        model: &str,
    ) -> Option<RuntimeModelTarget> {
        let route_id = route_id.trim();
        let model = model.trim();
        if route_id.is_empty() || model.is_empty() {
            return None;
        }
        self.configured_model_targets().into_iter().find(|target| {
            target.route_id == route_id
                && (model_id::equal(&target.upstream_model, model)
                    || model_id::equal(&target.alias, model))
        })
    }

    pub(crate) fn effective_runtime_default_target(&self) -> Option<RuntimeModelTarget> {
        let targets = self.runtime_model_targets();
        let requested = self.default_model();
        requested
            .and_then(|requested| {
                targets
                    .iter()
                    .find(|target| model_id::equal(&target.alias, requested))
                    .cloned()
            })
            .or_else(|| targets.into_iter().next())
    }

    pub(crate) fn runtime_model_targets(&self) -> Vec<RuntimeModelTarget> {
        let usable_official_routes = self
            .usable_official_routes()
            .map(|profile| profile.id.clone())
            .collect::<BTreeSet<_>>();
        self.configured_model_targets()
            .into_iter()
            .filter(|target| !target.official || usable_official_routes.contains(&target.route_id))
            .collect()
    }

    pub fn has_third_party_route(&self) -> bool {
        self.profiles.iter().any(|profile| {
            profile.enabled && !profile.official_account && !profile.is_unconfigured_default()
        })
    }

    pub(crate) fn uses_builtin_official_model_catalog(&self) -> bool {
        !self.has_third_party_route()
            // 多账号并存时官方模型名必须带线路前缀，内置目录无法表达。
            && !self.qualifies_official_model_ids()
            && self
                .profiles
                .iter()
                .any(|profile| profile.enabled && profile.official_account)
    }

    pub(crate) fn allocate_route_short_name(&self, name: &str) -> String {
        let used = self
            .profiles
            .iter()
            .map(|profile| profile.short_name.trim().to_string())
            .filter(|short_name| !short_name.is_empty())
            .collect::<BTreeSet<_>>();
        unique_default_route_short_name(name, &used)
    }

    pub(crate) fn looks_like_empty_default_route(&self) -> bool {
        let Some(profile) = self.profiles.first() else {
            return true;
        };
        self.profiles.len() == 1
            && profile.name == "默认配置"
            && profile.base_url.trim().is_empty()
            && profile.api_key.trim().is_empty()
            && !profile.api_key_configured
            && !profile.official_account
            && self.selected_models_by_provider.is_empty()
            && self.manual_third_party_models_by_provider.is_empty()
            && self.declared_official_models_by_provider.is_empty()
            && self.upstream_models_by_provider.is_empty()
            && self.default_model.trim().is_empty()
    }

    pub(crate) fn remember_model_aliases(&mut self) {
        self.model_alias_history = std::mem::take(&mut self.model_alias_history)
            .into_iter()
            .filter(|(alias, model)| {
                alias.split_once('/').is_some_and(|(provider, suffix)| {
                    !provider.trim().is_empty() && model_id::equal(suffix, model)
                }) && !model.trim().is_empty()
            })
            .map(|(alias, model)| (model_id::key(&alias), model.trim().to_string()))
            .collect();
        for target in self.configured_model_targets() {
            self.model_alias_history
                .insert(model_id::key(&target.alias), target.upstream_model);
        }
    }

    fn configured_model_targets(&self) -> Vec<RuntimeModelTarget> {
        let mut targets = Vec::new();
        for profile in &self.profiles {
            if !profile.enabled {
                continue;
            }
            let provider_id = profile.provider_id().trim();
            if provider_id.is_empty() {
                continue;
            }
            let models = if profile.official_account {
                self.enabled_official_route_models(provider_id)
            } else {
                self.enabled_route_models(provider_id)
            };
            for upstream_model in models {
                let alias = local_router::model_alias(provider_id, &upstream_model);
                let request_provider_id = self
                    .runtime_gateway_provider_id_for_profile(profile)
                    .to_string();
                targets.push(RuntimeModelTarget {
                    route_id: profile.id.clone(),
                    provider_id: provider_id.to_string(),
                    alias: alias.clone(),
                    request_provider_id,
                    // `request_model` is the upstream id published beside the
                    // stable route-qualified selector. The local gateway owns
                    // the final selector-to-upstream translation.
                    request_model: upstream_model.clone(),
                    upstream_model,
                    official: profile.official_account,
                });
            }
        }
        targets
    }

    fn normalize_global_default_model(&mut self) {
        self.default_model = self.default_model.trim().to_string();

        let targets = self.configured_model_targets();
        if targets.is_empty() {
            return;
        }
        if let Some(canonical) = targets
            .iter()
            .find(|target| model_id::equal(&target.alias, &self.default_model))
            .map(|target| target.alias.clone())
            .or_else(|| {
                let source = if targets
                    .iter()
                    .any(|target| model_id::equal(&target.upstream_model, &self.default_model))
                {
                    self.default_model.as_str()
                } else {
                    model_id::historical_source(&self.default_model, &self.model_alias_history)
                        .unwrap_or(&self.default_model)
                };
                let matches = targets
                    .iter()
                    .filter(|target| model_id::equal(&target.upstream_model, source))
                    .collect::<Vec<_>>();
                (matches.len() == 1).then(|| matches[0].alias.clone())
            })
        {
            self.default_model = canonical;
        } else {
            self.default_model = targets[0].alias.clone();
        }
    }

    pub(crate) fn needs_initial_route_import(&self) -> bool {
        // A launch-derived official route can disappear when the auth probe
        // falls back to an API-key launch. The resulting empty placeholder must
        // still be able to import the current Codex provider again, even if a
        // previous launch already marked the initial import as completed.
        self.profiles.len() == 1 && self.profiles[0].is_unconfigured_default()
    }

    /// Build one model catalog for all routes registered in the current Codex
    /// process. Third-party entries use local-router aliases so Codex can send
    /// requests through one stable provider while Codey restores upstream ids.
    pub fn runtime_catalog_models(&self) -> (Vec<String>, Vec<String>) {
        let qualify_official = self.qualifies_official_model_ids();
        let mut upstream = Vec::new();
        let mut selected = Vec::new();
        for profile in &self.profiles {
            if !profile.enabled {
                continue;
            }
            if profile.official_account {
                if self.official_route_usable(profile) {
                    let provider_id = profile.provider_id();
                    let enabled = self.enabled_official_route_models(provider_id);
                    let aliases = enabled
                        .iter()
                        .map(|model| runtime_catalog_model_id(profile, model, qualify_official))
                        .collect::<Vec<_>>();
                    upstream.extend(aliases.iter().cloned());
                    selected.extend(aliases);
                }
                continue;
            }
            let provider_id = profile.provider_id();
            if let Some(models) = self.upstream_models_by_provider.get(profile.provider_id()) {
                upstream.extend(
                    models
                        .iter()
                        .map(|model| local_router::model_alias(provider_id, model)),
                );
            }
            let enabled_models = self.enabled_route_models(provider_id);
            if !enabled_models.is_empty() {
                let aliases = enabled_models
                    .iter()
                    .map(|model| local_router::model_alias(provider_id, model))
                    .collect::<Vec<_>>();
                upstream.extend(aliases.iter().cloned());
                selected.extend(aliases);
            }
        }
        (
            model_id::dedupe_preserving_first(upstream.iter().map(String::as_str)),
            model_id::dedupe_preserving_first(selected.iter().map(String::as_str)),
        )
    }

    /// Native display names keyed by the route-qualified runtime catalog id.
    ///
    /// Codex renders this field directly in places Codey cannot patch, such as
    /// the synthetic model-change divider. Raw ids keep the native official
    /// names so a single-account setup stays unchanged; aliased ids gain the
    /// same `[短名] 模型名` form the Codey model menu already shows.
    pub(crate) fn runtime_catalog_display_names(&self) -> BTreeMap<String, String> {
        let qualify_official = self.qualifies_official_model_ids();
        let mut names = BTreeMap::new();
        for profile in &self.profiles {
            if !profile.enabled {
                continue;
            }
            let provider_id = profile.provider_id();
            if profile.official_account {
                if !self.official_route_usable(profile) {
                    continue;
                }
                for model in self.enabled_official_route_models(provider_id) {
                    let id = runtime_catalog_model_id(profile, &model, qualify_official);
                    if id.contains('/') {
                        names.insert(id, route_model_display_name(profile, &model));
                    }
                }
                continue;
            }
            for model in self.enabled_route_models(provider_id) {
                names.insert(
                    local_router::model_alias(provider_id, &model),
                    route_model_display_name(profile, &model),
                );
            }
        }
        names
    }

    /// Catalog id of one runtime model target. It matches the ids produced by
    /// `runtime_catalog_models` so the default model always names a model that
    /// Codex can find in the generated catalog.
    pub(crate) fn runtime_catalog_id_for_target(&self, target: &RuntimeModelTarget) -> String {
        if target.official && !self.qualifies_official_model_ids() {
            target.upstream_model.clone()
        } else {
            target.alias.clone()
        }
    }

    pub(crate) fn remember_current_provider_official_model_support(
        &mut self,
        models: impl IntoIterator<Item = String>,
    ) {
        let Some(provider_id) = self.current_provider_id().map(ToString::to_string) else {
            return;
        };
        self.remember_provider_official_model_support(&provider_id, models);
    }

    fn remember_provider_official_model_support(
        &mut self,
        provider_id: &str,
        models: impl IntoIterator<Item = String>,
    ) {
        if provider_id.trim().is_empty() || self.provider_is_official(provider_id) {
            return;
        }
        let official_models_by_key = official_models_by_key();
        let canonical_models =
            model_id::dedupe_preserving_first(models.into_iter().filter_map(|model| {
                official_models_by_key
                    .get(&model_id::key(&model))
                    .map(String::as_str)
            }));
        if canonical_models.is_empty() {
            return;
        }

        let declared_models = self
            .declared_official_models_by_provider
            .entry(provider_id.to_string())
            .or_default();
        declared_models.extend(canonical_models.iter().cloned());
        normalize_model_list(declared_models);

        let upstream_models = self
            .upstream_models_by_provider
            .entry(provider_id.to_string())
            .or_default();
        upstream_models.extend(canonical_models);
        normalize_model_list(upstream_models);
    }

    fn provider_is_official(&self, provider_id: &str) -> bool {
        self.profiles
            .iter()
            .any(|profile| profile.official_account && profile.provider_id() == provider_id)
    }

    fn normalize_subagent_model_references(&mut self) {
        let targets = self.configured_model_targets();
        if targets.is_empty() {
            return;
        }

        let fallback_alias = targets
            .iter()
            .find(|target| model_id::equal(&target.alias, &self.default_model))
            .unwrap_or(&targets[0])
            .alias
            .clone();
        let provider_prefixes = self
            .profiles
            .iter()
            .map(|profile| local_router::model_alias(profile.provider_id(), ""))
            .collect::<Vec<_>>();
        let qualify_unique_models = self.local_router_enabled;
        for selection in self.subagent_roles.values_mut() {
            let requested = selection.model.trim();
            let canonical = targets
                .iter()
                .find(|target| model_id::equal(&target.alias, requested))
                .map(|target| target.alias.clone())
                .or_else(|| {
                    if !qualify_unique_models {
                        return None;
                    }
                    // Preserve the model's route identity for subagents. The
                    // router selects their isolated HTTP/SSE transport per
                    // request, so catalog identity does not grant upstream WS.
                    // Never guess a route when the upstream model is ambiguous.
                    let source = if targets
                        .iter()
                        .any(|target| model_id::equal(&target.upstream_model, requested))
                    {
                        requested
                    } else {
                        model_id::historical_source(requested, &self.model_alias_history)
                            .unwrap_or(requested)
                    };
                    let mut matches = targets
                        .iter()
                        .filter(|target| model_id::equal(&target.upstream_model, source));
                    let target = matches.next()?;
                    matches.next().is_none().then(|| target.alias.clone())
                })
                .unwrap_or_else(|| {
                    if provider_prefixes.iter().any(|prefix| {
                        requested
                            .get(..prefix.len())
                            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
                    }) {
                        fallback_alias.clone()
                    } else {
                        requested.to_string()
                    }
                });
            selection.model = canonical;
        }
        if let Some(default_role) = self.subagent_roles.get(SUBAGENT_ROLE_DEFAULT) {
            self.subagent_model.clone_from(&default_role.model);
            self.subagent_reasoning_effort
                .clone_from(&default_role.reasoning_effort);
        }
    }

    /// 保留失效的选择供用户重新配置，但不把它交给运行期。路由模式保存
    /// 当前线路别名，直连模式只接受活动线路的模型，并保存上游模型名。
    fn normalize_misc_model(&mut self) {
        self.misc_model = self.misc_model.trim().to_string();
        if let Some(target) = self.configured_misc_model_target() {
            self.misc_model = if self.local_router_enabled {
                target.alias
            } else {
                target.upstream_model
            };
        }
    }

    fn configured_misc_model_target(&self) -> Option<RuntimeModelTarget> {
        let requested = self.misc_model.trim();
        if requested.is_empty() {
            return None;
        }
        let targets = self.configured_model_targets();
        let allowed = |target: &RuntimeModelTarget| {
            self.local_router_enabled || target.route_id == self.active_profile_id
        };
        if let Some(target) = targets
            .iter()
            .find(|target| model_id::equal(&target.alias, requested))
        {
            return allowed(target).then(|| target.clone());
        }
        // 历史别名只用于恢复普通会话，不能改变杂事模型指定的供应商或账号。
        if model_id::historical_source(requested, &self.model_alias_history).is_some() {
            return None;
        }
        let mut matches = targets
            .into_iter()
            .filter(|target| allowed(target) && model_id::equal(&target.upstream_model, requested));
        let target = matches.next()?;
        matches.next().is_none().then_some(target)
    }

    /// 杂事模型对应的运行期模型。空值、线路已移除或线路未启用该模型时返回
    /// `None`，调用方据此保持 Codex 原生行为。
    pub(crate) fn misc_model_target(&self) -> Option<RuntimeModelTarget> {
        let target = self.configured_misc_model_target()?;
        (!target.official
            || self
                .usable_official_routes()
                .any(|profile| profile.id == target.route_id))
        .then_some(target)
    }

    /// 杂事模型在 Codex 目录里实际使用的 id。官方模型在单一官方线路下沿用
    /// 原生 OpenAI id，其余情况使用带线路的稳定选择器。未启用本地路由时
    /// Codex 直接面向当前线路，只发送该线路已启用的上游模型名。
    pub(crate) fn misc_model_catalog_id(&self) -> Option<String> {
        let target = self.misc_model_target()?;
        if !self.local_router_enabled {
            return Some(target.upstream_model);
        }
        Some(self.runtime_catalog_id_for_target(&target))
    }

    pub(crate) fn reconcile_after_route_removal(&mut self, removed_provider_id: &str) {
        self.normalize_global_default_model();
        self.normalize_subagent_model_references();
        // 显式删除线路时移除杂事模型选择；临时停用线路则保留选择以便恢复。
        if model_references_provider(&self.misc_model, removed_provider_id) {
            self.misc_model.clear();
        }
        self.normalize_misc_model();
        let targets = self.configured_model_targets();
        let fallback_alias = targets
            .iter()
            .find(|target| model_id::equal(&target.alias, &self.default_model))
            .or_else(|| targets.first())
            .map(|target| target.alias.clone());

        if model_references_provider(&self.default_model, removed_provider_id) {
            self.default_model = fallback_alias.clone().unwrap_or_default();
        }
        for selection in self.subagent_roles.values_mut() {
            if model_references_provider(&selection.model, removed_provider_id) {
                selection.model = fallback_alias
                    .clone()
                    .unwrap_or_else(|| DEFAULT_SUBAGENT_MODEL.to_string());
            }
        }
        if let Some(default_role) = self.subagent_roles.get(SUBAGENT_ROLE_DEFAULT) {
            self.subagent_model.clone_from(&default_role.model);
            self.subagent_reasoning_effort
                .clone_from(&default_role.reasoning_effort);
        }
    }
}

/// Catalog id Codex sees for one route model. Official models keep their native
/// OpenAI ids while a single account is in use and gain the route prefix once
/// several official accounts are served side by side.
pub(super) fn runtime_catalog_model_id(
    profile: &ProviderProfile,
    model: &str,
    qualify_official: bool,
) -> String {
    if profile.official_account && !qualify_official {
        model.trim().to_string()
    } else {
        local_router::model_alias(profile.provider_id(), model)
    }
}

/// One line for the Codex model picker and other native surfaces: route short
/// name plus the upstream model, without the internal provider selector.
pub(super) fn route_model_display_name(profile: &ProviderProfile, model: &str) -> String {
    format!("[{}] {}", profile.display_short_name(), model.trim())
}

pub(super) fn model_references_provider(model: &str, provider_id: &str) -> bool {
    let prefix = local_router::model_alias(provider_id, "");
    model
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(&prefix))
}

pub(super) fn migrate_provider_model_list(
    models_by_provider: &mut BTreeMap<String, Vec<String>>,
    previous_provider_id: &str,
    official_provider_id: &str,
) {
    if previous_provider_id == official_provider_id {
        return;
    }
    let Some(mut models) = models_by_provider.remove(previous_provider_id) else {
        return;
    };
    let destination = models_by_provider
        .entry(official_provider_id.to_string())
        .or_default();
    destination.append(&mut models);
    normalize_model_list(destination);
}

pub(super) fn remap_model_provider_alias(
    model: &mut String,
    previous_provider_id: &str,
    official_provider_id: &str,
) {
    if previous_provider_id == official_provider_id {
        return;
    }
    let previous_prefix = local_router::model_alias(previous_provider_id, "");
    if !model
        .get(..previous_prefix.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&previous_prefix))
    {
        return;
    }
    let suffix = model[previous_prefix.len()..].to_string();
    *model = format!(
        "{}{}",
        local_router::model_alias(official_provider_id, ""),
        suffix
    );
}

pub(crate) fn validate_provider_profiles(profiles: &[ProviderProfile]) -> Result<(), String> {
    if profiles.is_empty() {
        return Err("至少需要保留一条线路".to_string());
    }
    let mut profile_ids = BTreeSet::new();
    let mut provider_ids = BTreeSet::new();
    let mut short_names = BTreeSet::new();
    let allows_empty_default = profiles.len() == 1 && profiles[0].is_unconfigured_default();
    for profile in profiles {
        if !allows_empty_default {
            profile.validate()?;
        }
        if !profile_ids.insert(profile.id.clone()) {
            return Err(format!("线路 ID 重复：{}", profile.id));
        }
        let provider_id = profile.provider_id().trim();
        if provider_id.is_empty() {
            return Err(format!("线路「{}」缺少 Codex Provider ID", profile.name));
        }
        if !provider_ids.insert(provider_id.to_string()) {
            return Err(format!(
                "多条线路使用了相同的 Codex Provider ID：{provider_id}"
            ));
        }
        // Official and third-party routes share one namespace because the
        // short name prefixes every route-scoped model name.
        let short_name = profile.short_name.trim();
        if !short_name.is_empty() && !short_names.insert(short_name.to_string()) {
            return Err(format!("多条线路使用了相同的短名称：{short_name}"));
        }
    }
    Ok(())
}

pub(super) fn normalize_model_reasoning_effort_lists(
    lists: &mut BTreeMap<String, BTreeMap<String, Vec<ModelReasoningEffort>>>,
) {
    lists.retain(|provider_id, models| {
        let mut normalized = BTreeMap::new();
        for (model, mut efforts) in std::mem::take(models) {
            normalize_model_reasoning_effort_values(&mut efforts);
            let model = model.trim().to_string();
            if !model.is_empty() && !efforts.is_empty() {
                normalized.entry(model).or_insert(efforts);
            }
        }
        *models = normalized;
        !provider_id.trim().is_empty() && !models.is_empty()
    });
}

pub(super) fn normalize_model_lists(lists: &mut BTreeMap<String, Vec<String>>) {
    lists.retain(|provider_id, models| {
        normalize_model_list(models);
        !provider_id.trim().is_empty() && !models.is_empty()
    });
}

pub(super) fn normalize_upstream_model_lists(lists: &mut BTreeMap<String, Vec<String>>) {
    lists.retain(|provider_id, models| {
        normalize_model_list(models);
        !provider_id.trim().is_empty()
    });
}

pub(super) fn normalize_model_list(models: &mut Vec<String>) {
    *models = model_id::dedupe_preserving_first(
        models
            .iter()
            .map(String::as_str)
            .filter(|model| !model_id::equal(model, local_router::CODEX_AUTO_REVIEW_MODEL)),
    );
}

pub(super) fn official_models_by_key() -> BTreeMap<String, String> {
    model_catalog::default_official_model_slugs()
        .into_iter()
        .map(|model| (model_id::key(&model), model))
        .collect()
}

pub(super) fn merge_declared_official_models_into_upstream(
    declared_official_models_by_provider: &BTreeMap<String, Vec<String>>,
    upstream_models_by_provider: &mut BTreeMap<String, Vec<String>>,
) {
    let official_models_by_key = official_models_by_key();
    for (provider_id, declared_models) in declared_official_models_by_provider {
        let upstream_models = upstream_models_by_provider
            .entry(provider_id.clone())
            .or_default();
        upstream_models.extend(
            declared_models
                .iter()
                .filter_map(|model| official_models_by_key.get(&model_id::key(model)).cloned()),
        );
        normalize_model_list(upstream_models);
    }
}

pub fn default_subagent_roles() -> BTreeMap<String, SubagentRoleConfig> {
    [
        (SUBAGENT_ROLE_QUICK_SCAN, "low"),
        (SUBAGENT_ROLE_DEEP_RESEARCH, "high"),
        (SUBAGENT_ROLE_VISUAL_ANALYSIS, "high"),
        (SUBAGENT_ROLE_WORKER, "medium"),
        (SUBAGENT_ROLE_VISUAL_WORKER, "high"),
        (SUBAGENT_ROLE_DEFAULT, DEFAULT_SUBAGENT_REASONING_EFFORT),
    ]
    .into_iter()
    .map(|(role, effort)| {
        (
            role.to_string(),
            SubagentRoleConfig::new(DEFAULT_SUBAGENT_MODEL, effort),
        )
    })
    .collect()
}

pub fn uniform_subagent_roles(
    model: &str,
    reasoning_effort: &str,
) -> BTreeMap<String, SubagentRoleConfig> {
    SUBAGENT_ROLE_IDS
        .into_iter()
        .map(|role| {
            (
                role.to_string(),
                SubagentRoleConfig::new(model, reasoning_effort),
            )
        })
        .collect()
}

pub(super) fn normalize_subagent_selection(model: &mut String, reasoning_effort: &mut String) {
    *model = model.trim().to_string();
    if model.is_empty() {
        *model = default_subagent_model();
    }
    *reasoning_effort = reasoning_effort.trim().to_ascii_lowercase();
    if !SUBAGENT_REASONING_EFFORTS.contains(&reasoning_effort.as_str()) {
        *reasoning_effort = default_subagent_reasoning_effort();
    }
}

/// 空值对应旧版默认协议；这里只接受明确支持的旧名称，不猜测未知值。
pub(super) fn recognized_upstream_protocol(value: &str) -> Option<&'static str> {
    Some(match value.trim() {
        UPSTREAM_PROTOCOL_OFFICIAL => UPSTREAM_PROTOCOL_OFFICIAL,
        "" | UPSTREAM_PROTOCOL_OPENAI_RESPONSES => UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
        UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS | "chatCompletions" | "openaiChatCompletion" => {
            UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS
        }
        UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES | "anthropic" | "anthropicMessagesApi" => {
            UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES
        }
        _ => return None,
    })
}

pub(super) fn normalize_upstream_protocol(value: &str) -> String {
    // 读取历史配置保留旧版默认行为；用户提交必须在归一化前校验原始值。
    recognized_upstream_protocol(value)
        .unwrap_or(UPSTREAM_PROTOCOL_OPENAI_RESPONSES)
        .to_string()
}

pub(super) fn normalize_auth_mode(value: &str, official: bool) -> String {
    if official || value.trim() == AUTH_MODE_OFFICIAL_ACCOUNT {
        AUTH_MODE_OFFICIAL_ACCOUNT
    } else {
        AUTH_MODE_API_KEY
    }
    .to_string()
}

pub(super) fn normalize_subagent_config(
    model: &mut String,
    reasoning_effort: &mut String,
    roles: &mut BTreeMap<String, SubagentRoleConfig>,
) {
    normalize_subagent_selection(model, reasoning_effort);
    roles.retain(|role, _| SUBAGENT_ROLE_IDS.contains(&role.as_str()));
    if roles.is_empty() {
        *roles = uniform_subagent_roles(model, reasoning_effort);
    } else {
        let fallback = roles
            .get(SUBAGENT_ROLE_DEFAULT)
            .cloned()
            .unwrap_or_else(|| SubagentRoleConfig::new(model.clone(), reasoning_effort.clone()));
        for role in SUBAGENT_ROLE_IDS {
            roles
                .entry(role.to_string())
                .or_insert_with(|| fallback.clone());
        }
        for selection in roles.values_mut() {
            normalize_subagent_selection(&mut selection.model, &mut selection.reasoning_effort);
        }
    }
    if let Some(default_role) = roles.get(SUBAGENT_ROLE_DEFAULT) {
        model.clone_from(&default_role.model);
        reasoning_effort.clone_from(&default_role.reasoning_effort);
    }
    if let Some(default_role) = roles.get_mut(SUBAGENT_ROLE_DEFAULT) {
        // `default` is an internal compatibility fallback rather than a
        // user-selectable role. Keep it available so omitted legacy agent
        // types cannot produce an empty or inconsistent runtime mapping.
        default_role.enabled = true;
    }
}
