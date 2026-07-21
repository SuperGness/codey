use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use codex_plus_core::settings::RelayProtocol;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfile {
    pub id: String,
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub protocol: RelayProtocol,
    /// Stable id of the Codex provider in cc-switch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc_switch_provider_id: Option<String>,
    #[serde(default)]
    pub cc_switch_read_only: bool,
}

impl ProviderProfile {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            base_url: String::new(),
            api_key: String::new(),
            protocol: RelayProtocol::Responses,
            cc_switch_provider_id: None,
            cc_switch_read_only: false,
        }
    }

    pub fn normalized_base_url(&self) -> String {
        self.base_url.trim().trim_end_matches('/').to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct WebhookConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodeyConfig {
    #[serde(default)]
    pub active_profile_id: String,
    #[serde(default)]
    pub profiles: Vec<ProviderProfile>,
    #[serde(default)]
    pub webhook: WebhookConfig,
    #[serde(default)]
    pub codex_app_path: String,
    #[serde(default)]
    pub user_scripts: Vec<String>,
    /// Codey-owned model selections. Provider connection data remains owned
    /// by cc-switch (or the local Codex configuration).
    #[serde(default)]
    pub selected_models_by_provider: BTreeMap<String, Vec<String>>,
    /// Last successful upstream model response, used to keep unsupported
    /// official models disabled between launches.
    #[serde(default)]
    pub upstream_models_by_provider: BTreeMap<String, Vec<String>>,
    #[serde(default = "default_true")]
    pub disable_trace_log_writes: bool,
    #[serde(default = "default_true")]
    pub slim_codex_pet: bool,
    #[serde(default = "default_true")]
    pub slim_codex_voice: bool,
    /// Publishes Codey's embedded FastCtx file tools to Codex for the next
    /// runtime. Disabled by default so existing tool behavior is unchanged.
    #[serde(default)]
    pub fast_context_tools: bool,
    /// Temporarily enables Codey's opinionated Codex multi-agent V2 setup for
    /// the next runtime. Disabled by default and restored on shutdown.
    #[serde(default)]
    pub subagent_optimization: bool,
    /// Automatically dismisses Codex's full-access safety notice in the
    /// renderer. Opt-in so the native warning remains visible by default.
    #[serde(default)]
    pub hide_full_access_warning: bool,
}

impl Default for CodeyConfig {
    fn default() -> Self {
        let profile = ProviderProfile::new("默认配置");
        Self {
            active_profile_id: profile.id.clone(),
            profiles: vec![profile],
            webhook: WebhookConfig::default(),
            codex_app_path: String::new(),
            user_scripts: Vec::new(),
            selected_models_by_provider: BTreeMap::new(),
            upstream_models_by_provider: BTreeMap::new(),
            disable_trace_log_writes: true,
            slim_codex_pet: true,
            slim_codex_voice: true,
            fast_context_tools: false,
            subagent_optimization: false,
            hide_full_access_warning: false,
        }
    }
}

impl CodeyConfig {
    pub fn normalize(mut self) -> Self {
        self.profiles
            .retain(|profile| !profile.id.trim().is_empty());
        if self.profiles.is_empty() {
            let profile = ProviderProfile::new("默认配置");
            self.active_profile_id = profile.id.clone();
            self.profiles.push(profile);
        }
        if !self
            .profiles
            .iter()
            .any(|profile| profile.id == self.active_profile_id)
        {
            self.active_profile_id = self.profiles[0].id.clone();
        }
        normalize_model_lists(&mut self.selected_models_by_provider);
        normalize_model_lists(&mut self.upstream_models_by_provider);
        self
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
            .map(|profile| {
                profile
                    .cc_switch_provider_id
                    .as_deref()
                    .unwrap_or(profile.id.as_str())
            })
    }

    pub fn selected_models(&self) -> &[String] {
        self.current_provider_id()
            .and_then(|provider_id| self.selected_models_by_provider.get(provider_id))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn upstream_models(&self) -> &[String] {
        self.current_provider_id()
            .and_then(|provider_id| self.upstream_models_by_provider.get(provider_id))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

fn normalize_model_lists(lists: &mut BTreeMap<String, Vec<String>>) {
    lists.retain(|provider_id, models| {
        *models = models
            .iter()
            .map(|model| model.trim())
            .filter(|model| !model.is_empty())
            .fold(Vec::<String>::new(), |mut unique, model| {
                if !unique.iter().any(|existing| existing == model) {
                    unique.push(model.to_string());
                }
                unique
            });
        !provider_id.trim().is_empty() && !models.is_empty()
    });
}

pub fn default_true() -> bool {
    true
}

pub fn default_config_path() -> PathBuf {
    ProjectDirs::from("com", "Codey", "Codey")
        .map(|dirs| dirs.config_dir().join("config.json"))
        .unwrap_or_else(|| PathBuf::from(".codey").join("config.json"))
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl Default for ConfigStore {
    fn default() -> Self {
        Self::new(default_config_path())
    }
}

impl ConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<CodeyConfig> {
        match fs::read_to_string(&self.path) {
            Ok(contents) => {
                let config = serde_json::from_str::<CodeyConfig>(&contents)
                    .with_context(|| format!("解析 Codey 配置失败：{}", self.path.display()))?;
                Ok(config.normalize())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(CodeyConfig::default())
            }
            Err(error) => {
                Err(error).with_context(|| format!("读取 Codey 配置失败：{}", self.path.display()))
            }
        }
    }

    pub fn save(&self, config: &CodeyConfig) -> Result<()> {
        let config = config.clone().normalize();
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Codey 配置路径无父目录"))?;
        fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec_pretty(&config)?;
        let temp = parent.join(format!(
            ".{}.tmp",
            self.path.file_name().unwrap().to_string_lossy()
        ));
        fs::write(&temp, bytes)?;
        atomic_replace(&temp, &self.path)
            .with_context(|| format!("替换 Codey 配置失败：{}", self.path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

fn atomic_replace(temp: &Path, destination: &Path) -> std::io::Result<()> {
    match fs::rename(temp, destination) {
        Ok(()) => Ok(()),
        Err(error) => {
            #[cfg(windows)]
            {
                // MoveFileEx used by std::fs::rename cannot replace an open
                // destination on some Windows versions. Keep the operation
                // in the same directory and retry after removing the old
                // file; Unix remains a single atomic rename.
                if destination.exists() {
                    fs::remove_file(destination)?;
                    return fs::rename(temp, destination);
                }
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_missing_active_profile() {
        let mut config = CodeyConfig::default();
        config.active_profile_id = "missing".to_string();
        let normalized = config.normalize();
        assert_eq!(normalized.active_profile_id, normalized.profiles[0].id);
    }

    #[test]
    fn legacy_micro_switch_is_ignored_but_feature_controls_are_preserved() {
        let config = serde_json::from_str::<CodeyConfig>(
            r#"{"activeProfileId":"","profiles":[],"disableTraceLogWrites":false,"disableCodexMicro":false,"slimCodexPet":false,"slimCodexVoice":false}"#,
        )
        .unwrap()
        .normalize();
        let serialized = serde_json::to_value(&config).unwrap();

        assert_eq!(config.disable_trace_log_writes, false);
        assert_eq!(config.slim_codex_pet, false);
        assert_eq!(config.slim_codex_voice, false);
        assert_eq!(
            serialized.get("disableTraceLogWrites"),
            Some(&serde_json::json!(false))
        );
        assert!(serialized.get("disableCodexMicro").is_none());
    }

    #[test]
    fn legacy_webhook_secret_is_ignored_and_not_serialized() {
        let config = serde_json::from_str::<CodeyConfig>(
            r#"{"activeProfileId":"","profiles":[],"webhook":{"enabled":true,"url":"https://open.feishu.cn/example","secret":"legacy-sign-key"}}"#,
        )
        .unwrap()
        .normalize();
        let serialized = serde_json::to_value(&config).unwrap();

        assert!(config.webhook.enabled);
        assert_eq!(config.webhook.url, "https://open.feishu.cn/example");
        assert!(serialized["webhook"].get("secret").is_none());
    }

    #[test]
    fn trace_log_guard_defaults_to_enabled_for_existing_configs() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();

        assert!(config.disable_trace_log_writes);
    }

    #[test]
    fn pet_slim_mode_defaults_to_enabled_for_existing_configs() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();

        assert!(config.slim_codex_pet);
    }

    #[test]
    fn pet_slim_mode_can_be_disabled_explicitly() {
        let config = serde_json::from_str::<CodeyConfig>(
            r#"{"activeProfileId":"","profiles":[],"slimCodexPet":false}"#,
        )
        .unwrap()
        .normalize();

        assert!(!config.slim_codex_pet);
    }

    #[test]
    fn voice_slim_mode_defaults_to_enabled_for_existing_configs() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();

        assert!(config.slim_codex_voice);
    }

    #[test]
    fn voice_slim_mode_can_be_disabled_explicitly() {
        let config = serde_json::from_str::<CodeyConfig>(
            r#"{"activeProfileId":"","profiles":[],"slimCodexVoice":false}"#,
        )
        .unwrap()
        .normalize();

        assert!(!config.slim_codex_voice);
    }

    #[test]
    fn fast_context_tools_default_to_disabled_for_existing_configs() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();

        assert!(!config.fast_context_tools);
    }

    #[test]
    fn subagent_optimization_defaults_to_disabled_for_existing_configs() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();

        assert!(!config.subagent_optimization);
    }

    #[test]
    fn full_access_warning_shield_defaults_to_disabled_for_existing_configs() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();

        assert!(!config.hide_full_access_warning);
    }
}
