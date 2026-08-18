use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use codey_runtime_core::settings::RelayProtocol;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model_id;
pub use crate::notifications::WebhookConfig;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfile {
    pub id: String,
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    /// Request-only provider headers loaded from the active Codex/CC Switch
    /// source. They may contain credentials, so they are never serialized into
    /// Codey's store or exposed to the renderer.
    #[serde(skip)]
    pub model_request_headers: BTreeMap<String, String>,
    #[serde(default)]
    pub protocol: RelayProtocol,
    /// Stable id of the Codex provider in cc-switch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc_switch_provider_id: Option<String>,
    #[serde(default)]
    pub cc_switch_read_only: bool,
    /// Preserve the exact Codex provider identity required for remote
    /// compaction when it was explicitly enabled by the source configuration.
    #[serde(default)]
    pub supports_remote_compaction: bool,
}

impl ProviderProfile {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            base_url: String::new(),
            api_key: String::new(),
            model_request_headers: BTreeMap::new(),
            protocol: RelayProtocol::Responses,
            cc_switch_provider_id: None,
            cc_switch_read_only: false,
            supports_remote_compaction: false,
        }
    }

    pub fn normalized_base_url(&self) -> String {
        self.base_url.trim().trim_end_matches('/').to_string()
    }
}

/// Prompt-optimization settings. The API key follows the notification-channel
/// credential pattern: redacted to the renderer, restored on save, cleared
/// only on explicit request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PromptOptimizationConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub api_key_configured: bool,
    #[serde(default, skip_serializing)]
    pub clear_api_key: bool,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_prompt_optimization_protocol")]
    pub protocol: RelayProtocol,
    /// Optional custom optimizer instructions. When empty the built-in
    /// default system prompt is used.
    #[serde(default)]
    pub instruction: String,
}

fn default_prompt_optimization_protocol() -> RelayProtocol {
    RelayProtocol::ChatCompletions
}

impl Default for PromptOptimizationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: String::new(),
            api_key: String::new(),
            api_key_configured: false,
            clear_api_key: false,
            model: String::new(),
            protocol: default_prompt_optimization_protocol(),
            instruction: String::new(),
        }
    }
}

impl PromptOptimizationConfig {
    pub(crate) fn normalize(&mut self) {
        self.base_url = self.base_url.trim().trim_end_matches('/').to_string();
        self.api_key = self.api_key.trim().to_string();
        self.api_key_configured = !self.api_key.is_empty();
        self.clear_api_key = false;
        self.model = self.model.trim().to_string();
        self.instruction = self.instruction.trim().to_string();
    }

    pub fn merge_redacted_secrets(&mut self, previous: &Self) {
        if self.clear_api_key {
            self.api_key.clear();
            self.api_key_configured = false;
            return;
        }
        if !self.api_key.trim().is_empty() || !self.api_key_configured {
            return;
        }
        self.api_key = previous.api_key.clone();
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        let base_url = self.base_url.trim();
        if base_url.is_empty() {
            return Ok(());
        }
        let url = reqwest::Url::parse(base_url)
            .map_err(|_| "提示词优化 API 地址不是有效的 HTTP(S) 地址".to_string())?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err("提示词优化 API 地址必须是有效的 HTTP(S) 地址".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum GpuLaunchMode {
    #[default]
    Off,
    DisableGpu,
    DisableGpuRasterization,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubagentRoleConfig {
    #[serde(default = "default_subagent_model")]
    pub model: String,
    #[serde(default = "default_subagent_reasoning_effort")]
    pub reasoning_effort: String,
}

impl SubagentRoleConfig {
    pub fn new(model: impl Into<String>, reasoning_effort: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            reasoning_effort: reasoning_effort.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodeyConfig {
    #[serde(default)]
    pub settings_revision: u64,
    #[serde(default)]
    pub active_profile_id: String,
    #[serde(default)]
    pub profiles: Vec<ProviderProfile>,
    #[serde(default)]
    pub webhook: WebhookConfig,
    #[serde(default)]
    pub prompt_optimization: PromptOptimizationConfig,
    #[serde(default)]
    pub codex_app_path: String,
    #[serde(default)]
    pub user_scripts: Vec<String>,
    /// Codey-owned model selections. Provider connection data remains owned
    /// by cc-switch (or the local Codex configuration).
    #[serde(default)]
    pub selected_models_by_provider: BTreeMap<String, Vec<String>>,
    /// Third-party model IDs that were explicitly typed by the user. Synced
    /// provider models are intentionally excluded so only manual entries can be
    /// deleted from Codey's saved support list.
    #[serde(default)]
    pub manual_third_party_models_by_provider: BTreeMap<String, Vec<String>>,
    /// Official model IDs that the user explicitly confirmed as supported by
    /// each third-party provider. Kept separate from synchronized results so a
    /// later model-list refresh cannot erase the user's declaration.
    #[serde(default)]
    pub declared_official_models_by_provider: BTreeMap<String, Vec<String>>,
    /// Effective provider model support after combining the last synchronized
    /// result with user-confirmed model declarations.
    #[serde(default)]
    pub upstream_models_by_provider: BTreeMap<String, Vec<String>>,
    /// Codey-owned default model selection per provider. Empty or unavailable
    /// values fall back to the first selectable official model.
    #[serde(default)]
    pub default_model_by_provider: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub disable_trace_log_writes: bool,
    /// Keeps Codex/ChatGPT Crashpad pending reports below a bounded disk
    /// budget. The guard only manages validated report files on macOS.
    #[serde(default = "default_true")]
    pub protect_crashpad_pending: bool,
    #[serde(default = "default_true")]
    pub slim_codex_pet: bool,
    /// Selects at most one Chromium GPU diagnostic argument for the next
    /// Codey-managed Codex launch. Disabled by default and ignored on macOS.
    #[serde(default)]
    pub gpu_launch_mode: GpuLaunchMode,
    /// Publishes Codey's embedded FastCtx file tools to Codex for the next
    /// runtime. Disabled by default so existing tool behavior is unchanged.
    #[serde(default)]
    pub fast_context_tools: bool,
    /// Bounds slow Codex remote feature bootstrap requests in the renderer so
    /// they cannot hold the initial route behind a long network timeout.
    #[serde(default = "default_true")]
    pub fast_codex_startup: bool,
    /// Temporarily enables Codey's opinionated Codex multi-agent V2 setup for
    /// the next runtime. Disabled by default and restored on shutdown.
    #[serde(default)]
    pub subagent_optimization: bool,
    /// Default model used by newly spawned subagents while Codey's
    /// multi-agent optimization is enabled.
    #[serde(default = "default_subagent_model")]
    pub subagent_model: String,
    /// Default reasoning effort used by newly spawned subagents.
    #[serde(default = "default_subagent_reasoning_effort")]
    pub subagent_reasoning_effort: String,
    /// Per-task agent selections. The legacy scalar defaults above mirror the
    /// `default` role so older Codey stores and Codex builds remain readable.
    #[serde(default)]
    pub subagent_roles: BTreeMap<String, SubagentRoleConfig>,
    /// Automatically dismisses Codex's full-access safety notice in the
    /// renderer. Opt-in so the native warning remains visible by default.
    #[serde(default)]
    pub hide_full_access_warning: bool,
    /// Shows the current ChatGPT account rate-limit windows in the Codex
    /// header. The renderer only activates this for an official login route.
    #[serde(default = "default_true")]
    pub show_account_usage_in_header: bool,
    /// Shows an inline performance/token footer below each Codex assistant
    /// reply. Timings and token counts are read from the local session rollout
    /// where available and degrade to dashes otherwise.
    #[serde(default = "default_true")]
    pub show_token_stats_card: bool,
    /// Public HTTPS endpoint for the version manifest published to Cloudflare R2.
    /// This is build-time configuration, not a user setting.
    #[serde(
        default = "default_update_manifest_url",
        skip_serializing,
        skip_deserializing
    )]
    pub update_manifest_url: String,
}

impl Default for CodeyConfig {
    fn default() -> Self {
        let profile = ProviderProfile::new("默认配置");
        Self {
            settings_revision: 0,
            active_profile_id: profile.id.clone(),
            profiles: vec![profile],
            webhook: WebhookConfig::default(),
            prompt_optimization: PromptOptimizationConfig::default(),
            codex_app_path: String::new(),
            user_scripts: Vec::new(),
            selected_models_by_provider: BTreeMap::new(),
            manual_third_party_models_by_provider: BTreeMap::new(),
            declared_official_models_by_provider: BTreeMap::new(),
            upstream_models_by_provider: BTreeMap::new(),
            default_model_by_provider: BTreeMap::new(),
            disable_trace_log_writes: true,
            protect_crashpad_pending: true,
            slim_codex_pet: true,
            gpu_launch_mode: GpuLaunchMode::Off,
            fast_context_tools: false,
            fast_codex_startup: true,
            subagent_optimization: false,
            subagent_model: default_subagent_model(),
            subagent_reasoning_effort: default_subagent_reasoning_effort(),
            subagent_roles: default_subagent_roles(),
            hide_full_access_warning: false,
            show_account_usage_in_header: true,
            show_token_stats_card: true,
            update_manifest_url: default_update_manifest_url(),
        }
    }
}

impl CodeyConfig {
    pub fn normalize(mut self) -> Self {
        self.update_manifest_url = default_update_manifest_url();
        self.profiles
            .retain(|profile| !profile.id.trim().is_empty());
        for profile in &mut self.profiles {
            if profile.cc_switch_read_only {
                profile.supports_remote_compaction = true;
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
            .any(|profile| profile.id == self.active_profile_id)
        {
            self.active_profile_id = self.profiles[0].id.clone();
        }
        normalize_model_lists(&mut self.selected_models_by_provider);
        normalize_model_lists(&mut self.manual_third_party_models_by_provider);
        normalize_model_lists(&mut self.declared_official_models_by_provider);
        normalize_upstream_model_lists(&mut self.upstream_models_by_provider);
        normalize_model_map(&mut self.default_model_by_provider);
        normalize_subagent_selection(
            &mut self.subagent_model,
            &mut self.subagent_reasoning_effort,
        );
        self.subagent_roles
            .retain(|role, _| SUBAGENT_ROLE_IDS.contains(&role.as_str()));
        if self.subagent_roles.is_empty() {
            self.subagent_roles =
                uniform_subagent_roles(&self.subagent_model, &self.subagent_reasoning_effort);
        } else {
            let fallback = self
                .subagent_roles
                .get(SUBAGENT_ROLE_DEFAULT)
                .cloned()
                .unwrap_or_else(|| {
                    SubagentRoleConfig::new(
                        self.subagent_model.clone(),
                        self.subagent_reasoning_effort.clone(),
                    )
                });
            for role in SUBAGENT_ROLE_IDS {
                self.subagent_roles
                    .entry(role.to_string())
                    .or_insert_with(|| fallback.clone());
            }
            for selection in self.subagent_roles.values_mut() {
                normalize_subagent_selection(&mut selection.model, &mut selection.reasoning_effort);
            }
        }
        if let Some(default_role) = self.subagent_roles.get(SUBAGENT_ROLE_DEFAULT) {
            self.subagent_model.clone_from(&default_role.model);
            self.subagent_reasoning_effort
                .clone_from(&default_role.reasoning_effort);
        }
        self.webhook.normalize();
        self.prompt_optimization.normalize();
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

    pub fn upstream_models(&self) -> &[String] {
        self.upstream_models_snapshot().unwrap_or_default()
    }

    pub fn upstream_models_snapshot(&self) -> Option<&[String]> {
        self.current_provider_id()
            .and_then(|provider_id| self.upstream_models_by_provider.get(provider_id))
            .map(Vec::as_slice)
    }

    pub fn default_model(&self) -> Option<&str> {
        self.current_provider_id()
            .and_then(|provider_id| self.default_model_by_provider.get(provider_id))
            .map(String::as_str)
    }
}

fn normalize_model_lists(lists: &mut BTreeMap<String, Vec<String>>) {
    lists.retain(|provider_id, models| {
        normalize_model_list(models);
        !provider_id.trim().is_empty() && !models.is_empty()
    });
}

fn normalize_upstream_model_lists(lists: &mut BTreeMap<String, Vec<String>>) {
    lists.retain(|provider_id, models| {
        normalize_model_list(models);
        !provider_id.trim().is_empty()
    });
}

fn normalize_model_list(models: &mut Vec<String>) {
    *models = model_id::dedupe_preserving_first(models.iter().map(String::as_str));
}

fn normalize_model_map(models_by_provider: &mut BTreeMap<String, String>) {
    models_by_provider.retain(|provider_id, model| {
        *model = model.trim().to_string();
        !provider_id.trim().is_empty() && !model.is_empty()
    });
}

fn default_true() -> bool {
    true
}

pub const DEFAULT_SUBAGENT_MODEL: &str = "gpt-5.6-terra";
pub const DEFAULT_SUBAGENT_REASONING_EFFORT: &str = "low";
pub const SUBAGENT_REASONING_EFFORTS: [&str; 6] =
    ["low", "medium", "high", "xhigh", "max", "ultra"];
pub const SUBAGENT_ROLE_QUICK_SCAN: &str = "codey_quick_scan";
pub const SUBAGENT_ROLE_DEEP_RESEARCH: &str = "codey_deep_research";
pub const SUBAGENT_ROLE_VISUAL_ANALYSIS: &str = "codey_visual_analysis";
pub const SUBAGENT_ROLE_WORKER: &str = "codey_worker";
pub const SUBAGENT_ROLE_VISUAL_WORKER: &str = "codey_visual_worker";
pub const SUBAGENT_ROLE_DEFAULT: &str = "default";
pub const SUBAGENT_ROLE_IDS: [&str; 6] = [
    SUBAGENT_ROLE_QUICK_SCAN,
    SUBAGENT_ROLE_DEEP_RESEARCH,
    SUBAGENT_ROLE_VISUAL_ANALYSIS,
    SUBAGENT_ROLE_WORKER,
    SUBAGENT_ROLE_VISUAL_WORKER,
    SUBAGENT_ROLE_DEFAULT,
];

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

fn normalize_subagent_selection(model: &mut String, reasoning_effort: &mut String) {
    *model = model.trim().to_string();
    if model.is_empty() {
        *model = default_subagent_model();
    }
    *reasoning_effort = reasoning_effort.trim().to_ascii_lowercase();
    if !SUBAGENT_REASONING_EFFORTS.contains(&reasoning_effort.as_str()) {
        *reasoning_effort = default_subagent_reasoning_effort();
    }
}

fn default_subagent_model() -> String {
    DEFAULT_SUBAGENT_MODEL.to_string()
}

fn default_subagent_reasoning_effort() -> String {
    DEFAULT_SUBAGENT_REASONING_EFFORT.to_string()
}

const DEFAULT_UPDATE_BASE_URL: &str = "https://pub-2d17a6a8bc22426a92e297a59f55ccc3.r2.dev";

fn update_manifest_url_from_base(configured_base_url: Option<&str>) -> String {
    let base_url = configured_base_url
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .unwrap_or(DEFAULT_UPDATE_BASE_URL)
        .trim_end_matches('/');
    format!("{base_url}/latest.json")
}

pub fn default_update_manifest_url() -> String {
    update_manifest_url_from_base(option_env!("CODEY_UPDATE_BASE_URL"))
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
        let file_name = self
            .path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Codey 配置路径缺少文件名"))?
            .to_string_lossy();
        let temp = parent.join(format!(
            ".{file_name}.codey-{}.tmp",
            Uuid::new_v4().simple()
        ));
        let replace_result = write_private_temp(&temp, &bytes).and_then(|()| {
            crate::fs_util::persist_temp_file(&temp, &self.path)
                .with_context(|| format!("替换 Codey 配置失败：{}", self.path.display()))
        });
        if replace_result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        replace_result?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

fn write_private_temp(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("创建 Codey 配置临时文件失败：{}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("写入 Codey 配置临时文件失败：{}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("同步 Codey 配置临时文件失败：{}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn config_temp_files_are_private_before_atomic_replace() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(".config.json.codey-test.tmp");

        write_private_temp(&path, b"private-secret").unwrap();

        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn config_save_does_not_leave_a_plaintext_temp_file() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(directory.path().join("config.json"));

        store.save(&CodeyConfig::default()).unwrap();

        let names = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, [std::ffi::OsString::from("config.json")]);
    }

    #[test]
    fn request_only_provider_headers_are_never_serialized() {
        let mut profile = ProviderProfile::new("Private Relay");
        profile
            .model_request_headers
            .insert("Authorization".to_string(), "secret".to_string());

        let serialized = serde_json::to_value(profile).unwrap();

        assert!(serialized.get("modelRequestHeaders").is_none());
        assert!(!serialized.to_string().contains("secret"));
    }

    #[test]
    fn normalizes_missing_active_profile() {
        let config = CodeyConfig {
            active_profile_id: "missing".to_string(),
            ..CodeyConfig::default()
        };
        let normalized = config.normalize();
        assert_eq!(normalized.active_profile_id, normalized.profiles[0].id);
    }

    #[test]
    fn preserves_an_empty_upstream_snapshot_as_a_successful_sync() {
        let mut config = CodeyConfig::default();
        let provider_id = config.current_provider_id().unwrap().to_string();
        config
            .upstream_models_by_provider
            .insert(provider_id, Vec::new());

        let normalized = config.normalize();

        assert_eq!(normalized.upstream_models_snapshot(), Some([].as_slice()));
    }

    #[test]
    fn model_lists_trim_and_dedupe_case_insensitively() {
        let mut config = CodeyConfig::default();
        let provider_id = config.current_provider_id().unwrap().to_string();
        config.selected_models_by_provider.insert(
            provider_id.clone(),
            vec![
                " Provider-A ".to_string(),
                "provider-a".to_string(),
                "Provider-B".to_string(),
            ],
        );
        config.upstream_models_by_provider.insert(
            provider_id.clone(),
            vec!["UPSTREAM-A".to_string(), "upstream-a".to_string()],
        );
        config.declared_official_models_by_provider.insert(
            provider_id.clone(),
            vec![" GPT-5.6-SOL ".to_string(), "gpt-5.6-sol".to_string()],
        );

        let normalized = config.normalize();

        assert_eq!(
            normalized.selected_models_by_provider[&provider_id],
            ["Provider-A", "Provider-B"]
        );
        assert_eq!(
            normalized.upstream_models_by_provider[&provider_id],
            ["UPSTREAM-A"]
        );
        assert_eq!(
            normalized.declared_official_models_by_provider[&provider_id],
            ["GPT-5.6-SOL"]
        );
    }

    #[test]
    fn diagnostic_guards_can_be_disabled_explicitly() {
        let config = serde_json::from_str::<CodeyConfig>(
            r#"{"activeProfileId":"","profiles":[],"disableTraceLogWrites":false,"protectCrashpadPending":false}"#,
        )
        .unwrap()
        .normalize();
        let serialized = serde_json::to_value(&config).unwrap();

        assert!(!config.disable_trace_log_writes);
        assert!(!config.protect_crashpad_pending);
        assert_eq!(
            serialized.get("disableTraceLogWrites"),
            Some(&serde_json::json!(false))
        );
        assert_eq!(
            serialized.get("protectCrashpadPending"),
            Some(&serde_json::json!(false))
        );
    }

    #[test]
    fn legacy_webhook_is_migrated_to_a_feishu_channel_without_the_old_secret() {
        let config = serde_json::from_str::<CodeyConfig>(
            r#"{"activeProfileId":"","profiles":[],"webhook":{"enabled":true,"url":"https://open.feishu.cn/example","secret":"legacy-sign-key"}}"#,
        )
        .unwrap()
        .normalize();
        let serialized = serde_json::to_value(&config).unwrap();

        assert!(!config.webhook.enabled);
        assert!(config.webhook.url.is_empty());
        assert_eq!(config.webhook.channels.len(), 1);
        let channel = &config.webhook.channels[0];
        assert_eq!(channel.id, "legacy-feishu");
        assert_eq!(
            channel.kind,
            crate::notifications::NotificationChannelKind::Feishu
        );
        assert!(channel.enabled);
        assert_eq!(channel.url, "https://open.feishu.cn/example");
        assert!(serialized["webhook"].get("enabled").is_none());
        assert!(serialized["webhook"].get("url").is_none());
        assert!(serialized["webhook"].get("secret").is_none());
    }

    #[test]
    fn trace_log_guard_defaults_to_enabled_for_existing_configs() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();

        assert!(config.disable_trace_log_writes);
        assert!(config.protect_crashpad_pending);
    }

    #[test]
    fn user_update_manifest_url_is_ignored_and_not_persisted() {
        let config = serde_json::from_str::<CodeyConfig>(
            r#"{"activeProfileId":"","profiles":[],"updateManifestUrl":"https://example.com/latest.json"}"#,
        )
        .unwrap()
        .normalize();
        let serialized = serde_json::to_value(&config).unwrap();

        assert_eq!(config.update_manifest_url, default_update_manifest_url());
        assert!(serialized.get("updateManifestUrl").is_none());
    }

    #[test]
    fn update_manifest_url_defaults_to_the_public_source_for_local_builds() {
        let expected = format!("{DEFAULT_UPDATE_BASE_URL}/latest.json");

        assert_eq!(update_manifest_url_from_base(None), expected);
        assert_eq!(update_manifest_url_from_base(Some("  ")), expected);
        assert_eq!(
            update_manifest_url_from_base(Some("https://updates.example.com/codey/")),
            "https://updates.example.com/codey/latest.json"
        );
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
    fn gpu_launch_mode_defaults_to_off() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();
        let serialized = serde_json::to_value(&config).unwrap();

        assert_eq!(config.gpu_launch_mode, GpuLaunchMode::Off);
        assert_eq!(serialized["gpuLaunchMode"], "off");
    }

    #[test]
    fn gpu_launch_modes_round_trip_as_mutually_exclusive_values() {
        for (wire_value, expected) in [
            ("off", GpuLaunchMode::Off),
            ("disableGpu", GpuLaunchMode::DisableGpu),
            (
                "disableGpuRasterization",
                GpuLaunchMode::DisableGpuRasterization,
            ),
        ] {
            let config = serde_json::from_value::<CodeyConfig>(serde_json::json!({
                "activeProfileId": "",
                "profiles": [],
                "gpuLaunchMode": wire_value,
            }))
            .unwrap()
            .normalize();

            assert_eq!(config.gpu_launch_mode, expected);
            assert_eq!(
                serde_json::to_value(&config).unwrap()["gpuLaunchMode"],
                wire_value
            );
        }
    }

    #[test]
    fn fast_context_tools_default_to_disabled_for_existing_configs() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();

        assert!(!config.fast_context_tools);
    }

    #[test]
    fn fast_codex_startup_defaults_to_enabled_for_existing_configs() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();

        assert!(config.fast_codex_startup);
    }

    #[test]
    fn subagent_optimization_defaults_to_disabled_for_existing_configs() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();

        assert!(!config.subagent_optimization);
        assert_eq!(config.subagent_model, DEFAULT_SUBAGENT_MODEL);
        assert_eq!(
            config.subagent_reasoning_effort,
            DEFAULT_SUBAGENT_REASONING_EFFORT
        );
        assert_eq!(config.subagent_roles.len(), SUBAGENT_ROLE_IDS.len());
        assert!(
            config
                .subagent_roles
                .values()
                .all(|selection| selection.model == DEFAULT_SUBAGENT_MODEL)
        );
    }

    #[test]
    fn subagent_defaults_preserve_models_and_invalid_effort_falls_back() {
        let config = serde_json::from_str::<CodeyConfig>(
            r#"{"activeProfileId":"","profiles":[],"subagentModel":"  provider-coder  ","subagentReasoningEffort":"unsupported"}"#,
        )
        .unwrap()
        .normalize();

        assert_eq!(config.subagent_model, "provider-coder");
        assert_eq!(
            config.subagent_reasoning_effort,
            DEFAULT_SUBAGENT_REASONING_EFFORT
        );
        assert!(config.subagent_roles.values().all(|selection| {
            selection.model == "provider-coder"
                && selection.reasoning_effort == DEFAULT_SUBAGENT_REASONING_EFFORT
        }));

        let empty = serde_json::from_str::<CodeyConfig>(
            r#"{"activeProfileId":"","profiles":[],"subagentModel":"   ","subagentReasoningEffort":"high"}"#,
        )
        .unwrap()
        .normalize();

        assert_eq!(empty.subagent_model, DEFAULT_SUBAGENT_MODEL);
        assert_eq!(empty.subagent_reasoning_effort, "high");
    }

    #[test]
    fn subagent_role_map_normalizes_independently_and_syncs_the_legacy_fallback() {
        let config = serde_json::from_str::<CodeyConfig>(
            r#"{
                "activeProfileId":"",
                "profiles":[],
                "subagentModel":"legacy-model",
                "subagentReasoningEffort":"low",
                "subagentRoles":{
                    "codey_quick_scan":{"model":" quick-model ","reasoningEffort":"MEDIUM"},
                    "default":{"model":" fallback-model ","reasoningEffort":"high"},
                    "unknown":{"model":"ignored","reasoningEffort":"low"}
                }
            }"#,
        )
        .unwrap()
        .normalize();

        assert_eq!(config.subagent_roles.len(), SUBAGENT_ROLE_IDS.len());
        assert!(!config.subagent_roles.contains_key("unknown"));
        assert_eq!(
            config.subagent_roles[SUBAGENT_ROLE_QUICK_SCAN],
            SubagentRoleConfig::new("quick-model", "medium")
        );
        assert_eq!(config.subagent_model, "fallback-model");
        assert_eq!(config.subagent_reasoning_effort, "high");
        assert_eq!(
            config.subagent_roles[SUBAGENT_ROLE_WORKER],
            SubagentRoleConfig::new("fallback-model", "high")
        );
    }

    #[test]
    fn full_access_warning_shield_defaults_to_disabled_for_existing_configs() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();

        assert!(!config.hide_full_access_warning);
    }

    #[test]
    fn header_account_usage_defaults_to_enabled_for_supported_configs() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();

        assert!(config.show_account_usage_in_header);
    }

    #[test]
    fn token_stats_card_defaults_to_enabled_and_round_trips() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();

        assert!(config.show_token_stats_card);

        let disabled = serde_json::from_str::<CodeyConfig>(
            r#"{"activeProfileId":"","profiles":[],"showTokenStatsCard":false}"#,
        )
        .unwrap()
        .normalize();
        assert!(!disabled.show_token_stats_card);
    }

    #[test]
    fn prompt_optimization_defaults_to_disabled_for_existing_configs() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();

        assert!(!config.prompt_optimization.enabled);
        assert!(config.prompt_optimization.api_key.is_empty());
        assert_eq!(
            config.prompt_optimization.protocol,
            RelayProtocol::ChatCompletions
        );
    }

    #[test]
    fn prompt_optimization_round_trips_without_persisting_clear_flag() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[],"promptOptimization":{"enabled":true,"baseUrl":" https://api.example.com/v1/ ","apiKey":"sk-secret","model":" gpt-x ","protocol":"responses","instruction":" 保持简洁 "}}"#)
            .unwrap()
            .normalize();
        let serialized = serde_json::to_value(&config).unwrap();

        assert!(config.prompt_optimization.enabled);
        assert_eq!(
            config.prompt_optimization.base_url,
            "https://api.example.com/v1"
        );
        assert_eq!(config.prompt_optimization.api_key, "sk-secret");
        assert!(config.prompt_optimization.api_key_configured);
        assert_eq!(config.prompt_optimization.model, "gpt-x");
        assert_eq!(
            config.prompt_optimization.protocol,
            RelayProtocol::Responses
        );
        assert_eq!(config.prompt_optimization.instruction, "保持简洁");
        assert!(
            serialized["promptOptimization"]
                .get("clearApiKey")
                .is_none()
        );
    }

    #[test]
    fn redacted_prompt_optimization_key_is_restored_when_other_settings_are_saved() {
        let previous = CodeyConfig {
            prompt_optimization: PromptOptimizationConfig {
                enabled: true,
                base_url: "https://api.example.com/v1".to_string(),
                api_key: "sk-secret".to_string(),
                api_key_configured: true,
                model: "gpt-x".to_string(),
                ..PromptOptimizationConfig::default()
            },
            ..CodeyConfig::default()
        };
        let mut incoming = previous.clone();
        incoming.prompt_optimization.api_key.clear();
        incoming
            .prompt_optimization
            .merge_redacted_secrets(&previous.prompt_optimization);

        assert_eq!(incoming.prompt_optimization.api_key, "sk-secret");
    }

    #[test]
    fn explicit_prompt_optimization_key_clear_does_not_restore_the_previous_secret() {
        let previous = CodeyConfig {
            prompt_optimization: PromptOptimizationConfig {
                api_key: "sk-secret".to_string(),
                api_key_configured: true,
                ..PromptOptimizationConfig::default()
            },
            ..CodeyConfig::default()
        };
        let mut incoming = previous.clone();
        incoming.prompt_optimization.api_key.clear();
        incoming.prompt_optimization.clear_api_key = true;
        incoming
            .prompt_optimization
            .merge_redacted_secrets(&previous.prompt_optimization);

        assert!(incoming.prompt_optimization.api_key.is_empty());
        assert!(!incoming.prompt_optimization.api_key_configured);
    }
}
