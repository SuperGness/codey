use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use crate::notifications::WebhookConfig;
use crate::{local_router, model_catalog, model_id};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub short_name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_upstream_protocol")]
    pub upstream_protocol: String,
    #[serde(default = "default_auth_mode")]
    pub auth_mode: String,
    #[serde(default)]
    pub api_key_configured: bool,
    #[serde(default, skip_serializing)]
    pub clear_api_key: bool,
    /// Request-only headers loaded from the active Codex provider. They may
    /// contain credentials, so they are never serialized into Codey's store or
    /// exposed to the renderer.
    #[serde(skip)]
    pub model_request_headers: BTreeMap<String, String>,
    /// Stable id of the provider in the source Codex configuration.
    #[serde(
        default,
        alias = "ccSwitchProviderId",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_provider_id: Option<String>,
    #[serde(default, alias = "ccSwitchReadOnly")]
    pub official_account: bool,
    /// Preserve the exact Codex provider identity required for remote
    /// compaction when it was explicitly enabled by the source configuration.
    #[serde(default)]
    pub supports_remote_compaction: bool,
    /// Whether this route supports the Responses WebSocket transport.
    /// Official ChatGPT-account routes normalize to enabled; third-party
    /// routes remain disabled unless the user explicitly opts in.
    #[serde(default)]
    pub supports_websockets: bool,
    /// Whether this route natively serves the Responses hosted Web Search
    /// tool. Official ChatGPT-account routes normalize to enabled;
    /// third-party Responses routes require an explicit opt-in.
    #[serde(default)]
    pub supports_native_web_search: bool,
    /// Whether this route can serve Codex's hidden automatic review model.
    /// Official ChatGPT-account routes normalize to enabled; third-party
    /// routes remain disabled unless synchronization or the user enables it.
    #[serde(default)]
    pub supports_auto_review: bool,
}

pub const DERIVED_OFFICIAL_PROFILE_ID: &str = "codey-official-account";
pub const OFFICIAL_ROUTE_SHORT_NAME: &str = "官";
pub const MAX_ROUTE_SHORT_NAME_CHARS: usize = 2;

fn default_route_short_name(name: &str) -> String {
    name.trim()
        .chars()
        .take(MAX_ROUTE_SHORT_NAME_CHARS)
        .collect()
}

fn unique_default_route_short_name(name: &str, used: &BTreeSet<String>) -> String {
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

impl ProviderProfile {
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            id: Uuid::new_v4().to_string(),
            short_name: default_route_short_name(&name),
            name,
            base_url: String::new(),
            api_key: String::new(),
            upstream_protocol: default_upstream_protocol(),
            auth_mode: default_auth_mode(),
            api_key_configured: false,
            clear_api_key: false,
            model_request_headers: BTreeMap::new(),
            source_provider_id: None,
            official_account: false,
            supports_remote_compaction: false,
            supports_websockets: false,
            supports_native_web_search: false,
            supports_auto_review: false,
        }
    }

    pub fn normalized_base_url(&self) -> String {
        self.base_url.trim().trim_end_matches('/').to_string()
    }

    /// The provider id passed to Codex and used by every route-scoped model
    /// map. Imported routes keep their source provider identity; Codey-owned
    /// routes use the profile id directly.
    pub fn provider_id(&self) -> &str {
        self.source_provider_id
            .as_deref()
            .unwrap_or(self.id.as_str())
    }

    pub(crate) fn runtime_wire_api(&self) -> Result<&'static str, String> {
        match self.upstream_protocol.as_str() {
            UPSTREAM_PROTOCOL_OFFICIAL
            | UPSTREAM_PROTOCOL_OPENAI_RESPONSES
            | UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS
            | UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES => Ok("responses"),
            protocol => Err(format!(
                "线路「{}」使用了不支持的上游协议：{protocol}",
                self.name
            )),
        }
    }

    pub(crate) fn is_unconfigured_default(&self) -> bool {
        self.name == "默认配置"
            && self.base_url.trim().is_empty()
            && self.api_key.trim().is_empty()
            && !self.api_key_configured
            && !self.official_account
    }

    pub(crate) fn normalize(&mut self) {
        self.id = self.id.trim().to_string();
        self.name = self.name.trim().to_string();
        if self.name.is_empty() {
            self.name = "未命名线路".to_string();
        }
        self.short_name = self.short_name.trim().to_string();
        self.base_url = self.base_url.trim().trim_end_matches('/').to_string();
        self.api_key = self.api_key.trim().to_string();
        self.source_provider_id = self
            .source_provider_id
            .take()
            .map(|provider_id| provider_id.trim().to_string())
            .filter(|provider_id| !provider_id.is_empty());
        self.upstream_protocol = normalize_upstream_protocol(&self.upstream_protocol);
        self.auth_mode = normalize_auth_mode(&self.auth_mode, self.official_account);
        if self.auth_mode == AUTH_MODE_OFFICIAL_ACCOUNT {
            self.official_account = true;
            self.short_name = OFFICIAL_ROUTE_SHORT_NAME.to_string();
            self.api_key.clear();
            self.supports_remote_compaction = true;
            self.supports_websockets = true;
            self.supports_native_web_search = true;
            self.supports_auto_review = true;
            self.upstream_protocol = UPSTREAM_PROTOCOL_OFFICIAL.to_string();
            self.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.to_string();
        } else {
            self.official_account = false;
            if self.upstream_protocol == UPSTREAM_PROTOCOL_OFFICIAL {
                self.upstream_protocol = UPSTREAM_PROTOCOL_OPENAI_RESPONSES.to_string();
            }
            if self.upstream_protocol != UPSTREAM_PROTOCOL_OPENAI_RESPONSES {
                self.supports_native_web_search = false;
            }
        }
        self.api_key_configured = !self.api_key.is_empty();
        self.clear_api_key = false;
    }

    pub fn merge_redacted_secret(&mut self, previous: Option<&Self>) {
        if self.clear_api_key {
            self.api_key.clear();
            self.api_key_configured = false;
            return;
        }
        if !self.api_key.trim().is_empty() || !self.api_key_configured {
            return;
        }
        if let Some(previous) = previous {
            self.api_key = previous.api_key.clone();
        }
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("线路 ID 不能为空".to_string());
        }
        if self.provider_id() == local_router::ROUTER_PROVIDER_ID {
            return Err(format!(
                "线路不能使用 Codey 内部 Provider ID「{}」",
                local_router::ROUTER_PROVIDER_ID
            ));
        }
        let name = self.name.trim();
        if name.is_empty() {
            return Err("线路名称不能为空".to_string());
        }
        if self.supports_websockets
            && !self.official_account
            && self.upstream_protocol != UPSTREAM_PROTOCOL_OPENAI_RESPONSES
        {
            return Err(format!(
                "线路「{name}」只有 OpenAI Responses 协议可以启用 WebSocket"
            ));
        }
        if self.auth_mode == AUTH_MODE_OFFICIAL_ACCOUNT || self.official_account {
            return Ok(());
        }
        let short_name = self.short_name.trim();
        if short_name.is_empty() {
            return Err(format!("线路「{name}」缺少短名称"));
        }
        if short_name.chars().count() > MAX_ROUTE_SHORT_NAME_CHARS {
            return Err(format!(
                "线路「{name}」的短名称最多 {MAX_ROUTE_SHORT_NAME_CHARS} 个字符"
            ));
        }
        if short_name == OFFICIAL_ROUTE_SHORT_NAME {
            return Err(format!(
                "线路「{name}」不能使用官方账号专属短名称「{OFFICIAL_ROUTE_SHORT_NAME}」"
            ));
        }
        let base_url = self.base_url.trim();
        if base_url.is_empty() {
            return Err(format!("线路「{name}」缺少 API URL"));
        }
        validate_outbound_api_url(base_url, &format!("线路「{name}」的 API URL"))?;
        self.runtime_wire_api()?;
        if self.api_key.trim().is_empty() {
            return Err(format!("线路「{name}」缺少第三方 API Key"));
        }
        Ok(())
    }
}

/// Prompt-optimization settings. The local renderer receives the API key and
/// masks it with a password input; clearing still requires an explicit request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PromptOptimizationConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Chooses whether optimization requests use an enabled Codey route or a
    /// separately configured upstream service. Existing configurations keep
    /// the manual mode so their connection settings remain usable.
    #[serde(default = "default_prompt_optimization_mode")]
    pub mode: String,
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
    #[serde(default = "default_prompt_optimization_upstream_protocol")]
    pub upstream_protocol: String,
    /// Optional custom optimizer instructions. When empty the built-in
    /// default system prompt is used.
    #[serde(default)]
    pub instruction: String,
}

impl Default for PromptOptimizationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: default_prompt_optimization_mode(),
            base_url: String::new(),
            api_key: String::new(),
            api_key_configured: false,
            clear_api_key: false,
            model: String::new(),
            upstream_protocol: default_prompt_optimization_upstream_protocol(),
            instruction: String::new(),
        }
    }
}

impl PromptOptimizationConfig {
    pub(crate) fn normalize(&mut self) {
        self.mode = normalize_prompt_optimization_mode(&self.mode);
        self.base_url = self.base_url.trim().trim_end_matches('/').to_string();
        self.api_key = self.api_key.trim().to_string();
        self.api_key_configured = !self.api_key.is_empty();
        self.clear_api_key = false;
        self.model = self.model.trim().to_string();
        self.upstream_protocol =
            normalize_prompt_optimization_upstream_protocol(&self.upstream_protocol);
        self.instruction = self.instruction.trim().to_string();
    }

    pub(crate) fn uses_codey_route(&self) -> bool {
        self.mode == PROMPT_OPTIMIZATION_MODE_CODEY_ROUTE
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
        if self.uses_codey_route() {
            if self.enabled && self.model.trim().is_empty() {
                return Err("启用提示词优化前，请选择 Codey 路由模型".to_string());
            }
            return Ok(());
        }
        let base_url = self.base_url.trim();
        if base_url.is_empty() {
            return if self.enabled {
                Err("启用提示词优化前，请先填写 API 地址".to_string())
            } else {
                Ok(())
            };
        }
        validate_outbound_api_url(base_url, "提示词优化 API 地址")?;
        if self.enabled && self.api_key.trim().is_empty() {
            return Err("启用提示词优化前，请先填写 API Key".to_string());
        }
        if self.enabled && self.model.trim().is_empty() {
            return Err("启用提示词优化前，请先选择或填写模型".to_string());
        }
        Ok(())
    }
}

pub const PROMPT_OPTIMIZATION_MODE_CODEY_ROUTE: &str = "codeyRoute";
pub const PROMPT_OPTIMIZATION_MODE_MANUAL: &str = "manual";

fn default_prompt_optimization_mode() -> String {
    PROMPT_OPTIMIZATION_MODE_MANUAL.to_string()
}

fn normalize_prompt_optimization_mode(value: &str) -> String {
    match value.trim() {
        PROMPT_OPTIMIZATION_MODE_CODEY_ROUTE => PROMPT_OPTIMIZATION_MODE_CODEY_ROUTE.to_string(),
        _ => PROMPT_OPTIMIZATION_MODE_MANUAL.to_string(),
    }
}

fn default_prompt_optimization_upstream_protocol() -> String {
    UPSTREAM_PROTOCOL_OPENAI_RESPONSES.to_string()
}

fn normalize_prompt_optimization_upstream_protocol(value: &str) -> String {
    match value.trim() {
        UPSTREAM_PROTOCOL_OPENAI_RESPONSES
        | UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS
        | UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES => value.trim().to_string(),
        _ => default_prompt_optimization_upstream_protocol(),
    }
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
    Ok(url)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum GpuLaunchMode {
    #[default]
    Off,
    DisableGpu,
    DisableGpuRasterization,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LaunchOfficialAccountStatus {
    Authenticated,
    #[default]
    Unauthenticated,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubagentRoleConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_subagent_model")]
    pub model: String,
    #[serde(default = "default_subagent_reasoning_effort")]
    pub reasoning_effort: String,
}

impl SubagentRoleConfig {
    pub fn new(model: impl Into<String>, reasoning_effort: impl Into<String>) -> Self {
        Self {
            enabled: true,
            model: model.into(),
            reasoning_effort: reasoning_effort.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeModelTarget {
    pub route_id: String,
    pub provider_id: String,
    pub alias: String,
    pub request_provider_id: String,
    pub request_model: String,
    pub upstream_model: String,
    pub official: bool,
}

const DEFAULT_ROUTE_REQUEST_LOG_QUEUE_CAPACITY: usize = 8_192;
const DEFAULT_ROUTE_REQUEST_LOG_BATCH_SIZE: usize = 256;
const DEFAULT_ROUTE_REQUEST_LOG_FLUSH_INTERVAL_MS: u64 = 1_000;
const DEFAULT_ROUTE_REQUEST_LOG_SHUTDOWN_FLUSH_TIMEOUT_MS: u64 = 1_500;
const DEFAULT_ROUTE_REQUEST_LOG_SAMPLE_RATE_PER_MILLION: u32 = 1_000_000;
const DEFAULT_ROUTE_REQUEST_LOG_MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const DEFAULT_ROUTE_REQUEST_LOG_RETAINED_FILES: usize = 7;
const DEFAULT_ROUTE_REQUEST_LOG_RETENTION_DAYS: u32 = 30;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RouteRequestLogBackend {
    #[default]
    Ndjson,
    Sqlite,
}

/// Best-effort request observations for the built-in router. The feature is
/// opt-in: when disabled the router does not create a queue or writer thread.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RouteRequestLogConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub backend: RouteRequestLogBackend,
    #[serde(default = "default_route_request_log_queue_capacity")]
    pub queue_capacity: usize,
    #[serde(default = "default_route_request_log_batch_size")]
    pub batch_size: usize,
    #[serde(default = "default_route_request_log_flush_interval_ms")]
    pub flush_interval_ms: u64,
    #[serde(default = "default_route_request_log_shutdown_flush_timeout_ms")]
    pub shutdown_flush_timeout_ms: u64,
    /// Deterministic process-local sampling in parts per million.
    #[serde(default = "default_route_request_log_sample_rate_per_million")]
    pub sample_rate_per_million: u32,
    #[serde(default = "default_route_request_log_max_file_bytes")]
    pub max_file_bytes: u64,
    #[serde(default = "default_route_request_log_retained_files")]
    pub retained_files: usize,
    #[serde(default = "default_route_request_log_retention_days")]
    pub retention_days: u32,
}

impl Default for RouteRequestLogConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: RouteRequestLogBackend::Ndjson,
            queue_capacity: DEFAULT_ROUTE_REQUEST_LOG_QUEUE_CAPACITY,
            batch_size: DEFAULT_ROUTE_REQUEST_LOG_BATCH_SIZE,
            flush_interval_ms: DEFAULT_ROUTE_REQUEST_LOG_FLUSH_INTERVAL_MS,
            shutdown_flush_timeout_ms: DEFAULT_ROUTE_REQUEST_LOG_SHUTDOWN_FLUSH_TIMEOUT_MS,
            sample_rate_per_million: DEFAULT_ROUTE_REQUEST_LOG_SAMPLE_RATE_PER_MILLION,
            max_file_bytes: DEFAULT_ROUTE_REQUEST_LOG_MAX_FILE_BYTES,
            retained_files: DEFAULT_ROUTE_REQUEST_LOG_RETAINED_FILES,
            retention_days: DEFAULT_ROUTE_REQUEST_LOG_RETENTION_DAYS,
        }
    }
}

impl RouteRequestLogConfig {
    fn normalize(&mut self) {
        self.queue_capacity = self.queue_capacity.clamp(128, 65_536);
        self.batch_size = self.batch_size.clamp(1, 2_048).min(self.queue_capacity);
        self.flush_interval_ms = self.flush_interval_ms.clamp(50, 60_000);
        self.shutdown_flush_timeout_ms = self.shutdown_flush_timeout_ms.clamp(100, 10_000);
        self.sample_rate_per_million = self.sample_rate_per_million.min(1_000_000);
        self.max_file_bytes = self
            .max_file_bytes
            .clamp(1024 * 1024, 4 * 1024 * 1024 * 1024);
        self.retained_files = self.retained_files.clamp(1, 100);
        self.retention_days = self.retention_days.clamp(1, 3_650);
    }
}

fn default_route_request_log_queue_capacity() -> usize {
    DEFAULT_ROUTE_REQUEST_LOG_QUEUE_CAPACITY
}

fn default_route_request_log_batch_size() -> usize {
    DEFAULT_ROUTE_REQUEST_LOG_BATCH_SIZE
}

fn default_route_request_log_flush_interval_ms() -> u64 {
    DEFAULT_ROUTE_REQUEST_LOG_FLUSH_INTERVAL_MS
}

fn default_route_request_log_shutdown_flush_timeout_ms() -> u64 {
    DEFAULT_ROUTE_REQUEST_LOG_SHUTDOWN_FLUSH_TIMEOUT_MS
}

fn default_route_request_log_sample_rate_per_million() -> u32 {
    DEFAULT_ROUTE_REQUEST_LOG_SAMPLE_RATE_PER_MILLION
}

fn default_route_request_log_max_file_bytes() -> u64 {
    DEFAULT_ROUTE_REQUEST_LOG_MAX_FILE_BYTES
}

fn default_route_request_log_retained_files() -> usize {
    DEFAULT_ROUTE_REQUEST_LOG_RETAINED_FILES
}

fn default_route_request_log_retention_days() -> u32 {
    DEFAULT_ROUTE_REQUEST_LOG_RETENTION_DAYS
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodeyConfig {
    #[serde(default)]
    pub settings_revision: u64,
    /// Controls whether Codey installs and uses its process-local multi-route
    /// gateway. Missing values default to enabled so existing installations
    /// keep their current behavior after upgrading.
    #[serde(default = "default_true")]
    pub local_router_enabled: bool,
    /// Structured, best-effort request observations for the built-in router.
    /// Disabled by default so existing installations pay no producer cost.
    #[serde(default)]
    pub route_request_log: RouteRequestLogConfig,
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
    /// Codey-owned model selections. Imported connection data originates from
    /// the local Codex configuration.
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
    /// One route-aware default model for the entire Codey model catalog.
    /// Official models keep their raw model id; third-party models use the
    /// local-router alias (`provider/model`) so equal upstream ids remain
    /// unambiguous across suppliers.
    #[serde(default)]
    pub default_model: String,
    /// Legacy per-provider defaults are read once and migrated into
    /// `default_model`. They are intentionally never written again.
    #[serde(default)]
    #[serde(skip_serializing)]
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
    /// Tracks whether Codey has already consumed the one-time default route
    /// import window. Existing non-empty configs are treated as already
    /// initialized so later launches never overwrite saved third-party routes
    /// from the ambient Codex configuration.
    #[serde(default)]
    pub initial_route_import_completed: bool,
    /// Automatically dismisses Codex's full-access safety notice in the
    /// renderer. Opt-in so the native warning remains visible by default.
    #[serde(default)]
    pub hide_full_access_warning: bool,
    /// Shows the current ChatGPT account rate-limit windows in the Codex
    /// header. The renderer only activates this for an official login route.
    #[serde(default = "default_true")]
    pub show_account_usage_in_header: bool,
    /// Launch-scoped authentication capability captured from Codex before
    /// Codey's temporary provider overrides are applied. It is intentionally
    /// never persisted or exposed as part of the editable configuration.
    #[serde(skip)]
    pub official_account_available_this_launch: bool,
    /// Three-state launch-scoped result for official account detection. The
    /// boolean above remains the runtime routing capability flag; this field
    /// preserves whether the preflight was authoritative or inconclusive.
    #[serde(skip)]
    pub official_account_status_this_launch: LaunchOfficialAccountStatus,
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
            local_router_enabled: true,
            route_request_log: RouteRequestLogConfig::default(),
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
            default_model: String::new(),
            default_model_by_provider: BTreeMap::new(),
            disable_trace_log_writes: true,
            protect_crashpad_pending: true,
            slim_codex_pet: true,
            gpu_launch_mode: GpuLaunchMode::Off,
            fast_context_tools: false,
            subagent_optimization: false,
            subagent_model: default_subagent_model(),
            subagent_reasoning_effort: default_subagent_reasoning_effort(),
            subagent_roles: default_subagent_roles(),
            initial_route_import_completed: false,
            hide_full_access_warning: false,
            show_account_usage_in_header: true,
            official_account_available_this_launch: false,
            official_account_status_this_launch: LaunchOfficialAccountStatus::Unauthenticated,
            update_manifest_url: default_update_manifest_url(),
        }
    }
}

impl CodeyConfig {
    pub fn normalize(mut self) -> Self {
        self.update_manifest_url = default_update_manifest_url();
        self.route_request_log.normalize();
        self.profiles
            .retain(|profile| !profile.id.trim().is_empty());
        let mut used_short_names = self
            .profiles
            .iter()
            .filter(|profile| {
                !profile.official_account
                    && profile.auth_mode.trim() != AUTH_MODE_OFFICIAL_ACCOUNT
                    && !profile.short_name.trim().is_empty()
            })
            .map(|profile| profile.short_name.trim().to_string())
            .collect::<BTreeSet<_>>();
        for profile in &mut self.profiles {
            if !profile.official_account
                && profile.auth_mode.trim() != AUTH_MODE_OFFICIAL_ACCOUNT
                && profile.short_name.trim().is_empty()
            {
                profile.short_name =
                    unique_default_route_short_name(&profile.name, &used_short_names);
                used_short_names.insert(profile.short_name.clone());
            }
            profile.normalize();
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
        let official_provider_ids = self
            .profiles
            .iter()
            .filter(|profile| profile.official_account)
            .map(|profile| profile.provider_id().to_string())
            .collect::<BTreeSet<_>>();
        normalize_model_lists(&mut self.selected_models_by_provider);
        normalize_model_lists(&mut self.manual_third_party_models_by_provider);
        normalize_model_lists(&mut self.declared_official_models_by_provider);
        if self.local_router_enabled {
            migrate_legacy_official_model_selections(
                &mut self.selected_models_by_provider,
                &mut self.manual_third_party_models_by_provider,
                &mut self.declared_official_models_by_provider,
                &official_provider_ids,
            );
        }
        normalize_upstream_model_lists(&mut self.upstream_models_by_provider);
        merge_declared_official_models_into_upstream(
            &self.declared_official_models_by_provider,
            &mut self.upstream_models_by_provider,
        );
        normalize_model_map(&mut self.default_model_by_provider);
        self.normalize_global_default_model();
        normalize_subagent_config(
            &mut self.subagent_model,
            &mut self.subagent_reasoning_effort,
            &mut self.subagent_roles,
        );
        self.normalize_subagent_model_references();
        if !self.initial_route_import_completed && !self.looks_like_empty_default_route() {
            self.initial_route_import_completed = true;
        }
        self.webhook.normalize();
        self.prompt_optimization.normalize();
        self
    }

    pub(crate) fn apply_launch_official_profile(
        &mut self,
        official_profile: Option<ProviderProfile>,
    ) {
        let previous_active_id = self.active_profile_id.clone();
        let previous_official_provider_ids = self
            .profiles
            .iter()
            .filter(|profile| profile.official_account)
            .map(|profile| profile.provider_id().to_string())
            .collect::<BTreeSet<_>>();
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
            self.manual_third_party_models_by_provider
                .remove(&provider_id);
            self.declared_official_models_by_provider
                .remove(&provider_id);
            self.upstream_models_by_provider.remove(&provider_id);
        }
        if let Some(mut official_profile) = official_profile {
            official_profile.id = DERIVED_OFFICIAL_PROFILE_ID.to_string();
            official_profile.normalize();
            let official_provider_id = official_profile.provider_id().to_string();
            for previous_provider_id in previous_official_provider_ids {
                self.migrate_official_provider_state(&previous_provider_id, &official_provider_id);
            }
            if let Some(existing) = self
                .profiles
                .iter_mut()
                .find(|profile| profile.id == DERIVED_OFFICIAL_PROFILE_ID)
            {
                *existing = official_profile;
            } else {
                self.profiles.insert(0, official_profile);
            }
            self.selected_models_by_provider
                .entry(official_provider_id)
                .or_insert_with(model_catalog::default_official_model_slugs);
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
        migrate_provider_default(
            &mut self.default_model_by_provider,
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

    /// Whether official ChatGPT routes can be served this launch.
    ///
    /// The loopback provider keeps Codex's native OpenAI authentication when
    /// an official route is available. A separate Codey-only header protects
    /// the local gateway, which still replaces authentication per route before
    /// forwarding third-party traffic.
    pub(crate) fn router_requires_openai_auth(&self) -> bool {
        self.official_account_available_this_launch
            && self.profiles.iter().any(|profile| profile.official_account)
    }

    pub(crate) fn runtime_gateway_provider_id(&self) -> &'static str {
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

    fn route_supports_websockets_this_launch_with_proxy(
        &self,
        profile: &ProviderProfile,
        outbound_proxy_configured: bool,
    ) -> bool {
        if outbound_proxy_configured {
            return false;
        }
        if profile.official_account {
            return self.official_account_available_this_launch;
        }
        profile.supports_websockets
            && profile.upstream_protocol == UPSTREAM_PROTOCOL_OPENAI_RESPONSES
    }

    /// Route-qualified catalog model IDs that use Responses WebSocket.
    /// Keeping this list model-scoped prevents one WS route from changing the
    /// upstream transport selected for every route on the shared provider.
    pub(crate) fn runtime_websocket_model_aliases(&self) -> Vec<String> {
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
                    .map(move |model| local_router::model_alias(provider_id, &model))
            })
            .collect()
    }

    pub(crate) fn runtime_supports_websockets(&self) -> bool {
        !self.runtime_websocket_model_aliases().is_empty()
    }

    /// Whether one route can expose Codex's native Responses Web Search tool
    /// this launch. A third-party route must opt in and use the native
    /// Responses protocol; adapted Chat/Anthropic routes never qualify.
    pub(crate) fn route_supports_native_web_search_this_launch(
        &self,
        profile: &ProviderProfile,
    ) -> bool {
        if profile.official_account {
            return self.official_account_available_this_launch;
        }
        profile.supports_native_web_search
            && profile.upstream_protocol == UPSTREAM_PROTOCOL_OPENAI_RESPONSES
    }

    /// Route-qualified model IDs that may retain native Web Search metadata in
    /// the generated runtime catalog. The catalog applies a second gate and
    /// only preserves the capability when the source model metadata declares
    /// support as well.
    pub(crate) fn runtime_native_web_search_model_aliases(&self) -> Vec<String> {
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
                    .map(move |model| local_router::model_alias(provider_id, &model))
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
        if profile.official_account {
            return self.official_account_available_this_launch;
        }
        profile.supports_remote_compaction
            && profile.upstream_protocol == UPSTREAM_PROTOCOL_OPENAI_RESPONSES
    }

    /// Advertise the OpenAI provider identity only when every runtime route
    /// supports native compaction. Codex derives this capability from the
    /// shared provider, so one adapted Chat/Anthropic route must disable it for
    /// the whole runtime even when an official account route is also present.
    pub(crate) fn runtime_supports_remote_compaction(&self) -> bool {
        let mut has_runtime_route = false;
        for profile in &self.profiles {
            if profile.provider_id().trim().is_empty() {
                continue;
            }
            if profile.official_account {
                if !self.official_account_available_this_launch {
                    continue;
                }
            } else if profile.normalized_base_url().is_empty() {
                continue;
            }
            has_runtime_route = true;
            if !self.route_supports_remote_compaction_this_launch(profile) {
                return false;
            }
        }
        has_runtime_route
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
        self.configured_model_targets()
            .into_iter()
            .filter(|target| !target.official || self.official_account_available_this_launch)
            .collect()
    }

    pub fn has_third_party_route(&self) -> bool {
        self.profiles
            .iter()
            .any(|profile| !profile.official_account)
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
            && self.default_model_by_provider.is_empty()
    }

    fn configured_model_targets(&self) -> Vec<RuntimeModelTarget> {
        let mut targets = Vec::new();
        for profile in &self.profiles {
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
                let request_provider_id = self.runtime_gateway_provider_id().to_string();
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
        if self.default_model.is_empty() {
            let mut provider_ids = Vec::new();
            if let Some(provider_id) = self.current_provider_id() {
                provider_ids.push(provider_id.to_string());
            }
            provider_ids.extend(
                self.profiles
                    .iter()
                    .map(|profile| profile.provider_id().to_string()),
            );
            provider_ids.extend(self.default_model_by_provider.keys().cloned());
            let mut seen = BTreeSet::new();
            for provider_id in provider_ids {
                if !seen.insert(provider_id.clone()) {
                    continue;
                }
                let Some(model) = self.default_model_by_provider.get(&provider_id) else {
                    continue;
                };
                self.default_model = self
                    .profiles
                    .iter()
                    .find(|profile| profile.provider_id() == provider_id)
                    .map(|_| local_router::model_alias(&provider_id, model))
                    .unwrap_or_else(|| model.clone());
                break;
            }
        }
        self.default_model_by_provider.clear();

        let targets = self.configured_model_targets();
        if targets.is_empty() {
            return;
        }
        if let Some(canonical) = targets
            .iter()
            .find(|target| model_id::equal(&target.alias, &self.default_model))
            .map(|target| target.alias.clone())
            .or_else(|| {
                let matches = targets
                    .iter()
                    .filter(|target| model_id::equal(&target.upstream_model, &self.default_model))
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
        !self.initial_route_import_completed && self.looks_like_empty_default_route()
    }

    /// Build one model catalog for all routes registered in the current Codex
    /// process. Third-party entries use local-router aliases so Codex can send
    /// requests through one stable provider while Codey restores upstream ids.
    pub fn runtime_catalog_models(&self) -> (Vec<String>, Vec<String>) {
        let include_all_official = self.official_account_available_this_launch
            && self.profiles.iter().any(|profile| profile.official_account);
        let mut upstream = Vec::new();
        let mut selected = Vec::new();
        for profile in &self.profiles {
            if profile.official_account {
                if include_all_official {
                    let provider_id = profile.provider_id();
                    let enabled = self.enabled_official_route_models(provider_id);
                    let aliases = enabled
                        .iter()
                        .map(|model| local_router::model_alias(provider_id, model))
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
                    let mut matches = targets
                        .iter()
                        .filter(|target| model_id::equal(&target.upstream_model, requested));
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

    pub(crate) fn reconcile_after_route_removal(&mut self, removed_provider_id: &str) {
        self.normalize_global_default_model();
        self.normalize_subagent_model_references();
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

fn model_references_provider(model: &str, provider_id: &str) -> bool {
    let prefix = local_router::model_alias(provider_id, "");
    model
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(&prefix))
}

fn migrate_provider_model_list(
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

fn migrate_provider_default(
    defaults_by_provider: &mut BTreeMap<String, String>,
    previous_provider_id: &str,
    official_provider_id: &str,
) {
    if previous_provider_id == official_provider_id {
        return;
    }
    let Some(model) = defaults_by_provider.remove(previous_provider_id) else {
        return;
    };
    defaults_by_provider
        .entry(official_provider_id.to_string())
        .or_insert(model);
}

fn remap_model_provider_alias(
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
        if !profile.official_account {
            let short_name = profile.short_name.trim();
            if !short_names.insert(short_name.to_string()) {
                return Err(format!("多条第三方线路使用了相同的短名称：{short_name}"));
            }
        }
    }
    Ok(())
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
    *models = model_id::dedupe_preserving_first(
        models
            .iter()
            .map(String::as_str)
            .filter(|model| !model_id::equal(model, local_router::CODEX_AUTO_REVIEW_MODEL)),
    );
}

fn official_models_by_key() -> BTreeMap<String, String> {
    model_catalog::default_official_model_slugs()
        .into_iter()
        .map(|model| (model_id::key(&model), model))
        .collect()
}

fn migrate_legacy_official_model_selections(
    selected_models_by_provider: &mut BTreeMap<String, Vec<String>>,
    manual_third_party_models_by_provider: &mut BTreeMap<String, Vec<String>>,
    declared_official_models_by_provider: &mut BTreeMap<String, Vec<String>>,
    official_provider_ids: &BTreeSet<String>,
) {
    let official_models_by_key = official_models_by_key();
    let provider_ids = selected_models_by_provider
        .keys()
        .chain(manual_third_party_models_by_provider.keys())
        .cloned()
        .collect::<BTreeSet<_>>();

    for provider_id in provider_ids {
        if official_provider_ids.contains(&provider_id) {
            continue;
        }
        let mut migrated_models = Vec::new();
        if let Some(models) = selected_models_by_provider.get_mut(&provider_id) {
            take_official_models(models, &official_models_by_key, &mut migrated_models);
        }
        if let Some(models) = manual_third_party_models_by_provider.get_mut(&provider_id) {
            take_official_models(models, &official_models_by_key, &mut migrated_models);
        }
        if migrated_models.is_empty() {
            continue;
        }

        let declared_models = declared_official_models_by_provider
            .entry(provider_id)
            .or_default();
        declared_models.extend(migrated_models);
        normalize_model_list(declared_models);
    }

    selected_models_by_provider.retain(|_, models| !models.is_empty());
    manual_third_party_models_by_provider.retain(|_, models| !models.is_empty());
}

fn merge_declared_official_models_into_upstream(
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

fn take_official_models(
    models: &mut Vec<String>,
    official_models_by_key: &BTreeMap<String, String>,
    migrated_models: &mut Vec<String>,
) {
    models.retain(|model| {
        let Some(canonical_model) = official_models_by_key.get(&model_id::key(model)) else {
            return true;
        };
        migrated_models.push(canonical_model.clone());
        false
    });
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
pub const UPSTREAM_PROTOCOL_OFFICIAL: &str = "official";
pub const UPSTREAM_PROTOCOL_OPENAI_RESPONSES: &str = "openaiResponses";
pub const UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS: &str = "openaiChatCompletions";
pub const UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES: &str = "anthropicMessages";
pub const AUTH_MODE_OFFICIAL_ACCOUNT: &str = "officialAccount";
pub const AUTH_MODE_API_KEY: &str = "apiKey";
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

fn default_upstream_protocol() -> String {
    UPSTREAM_PROTOCOL_OPENAI_RESPONSES.to_string()
}

fn default_auth_mode() -> String {
    AUTH_MODE_API_KEY.to_string()
}

fn normalize_upstream_protocol(value: &str) -> String {
    match value.trim() {
        UPSTREAM_PROTOCOL_OFFICIAL => UPSTREAM_PROTOCOL_OFFICIAL,
        UPSTREAM_PROTOCOL_OPENAI_RESPONSES => UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
        UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS | "chatCompletions" | "openaiChatCompletion" => {
            UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS
        }
        UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES | "anthropic" | "anthropicMessagesApi" => {
            UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES
        }
        _ => UPSTREAM_PROTOCOL_OPENAI_RESPONSES,
    }
    .to_string()
}

fn normalize_auth_mode(value: &str, official: bool) -> String {
    if official || value.trim() == AUTH_MODE_OFFICIAL_ACCOUNT {
        AUTH_MODE_OFFICIAL_ACCOUNT
    } else {
        AUTH_MODE_API_KEY
    }
    .to_string()
}

fn normalize_subagent_config(
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

const CONFIG_BACKUP_COUNT: usize = 3;

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
        let primary = read_config_file(&self.path);
        if let Ok(config) = primary {
            return Ok(config);
        }
        let primary_missing = primary.as_ref().is_err_and(|error| {
            error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        });
        let primary_error = primary.unwrap_err();
        let mut found_backup = false;
        let mut backup_errors = Vec::new();
        for index in 1..=CONFIG_BACKUP_COUNT {
            let path = self.backup_path(index);
            match read_config_file(&path) {
                Ok(config) => return Ok(config),
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
                Err(error) => {
                    found_backup = true;
                    backup_errors.push(format!("{}：{error:#}", path.display()));
                }
            }
        }
        if primary_missing && !found_backup {
            return Ok(CodeyConfig::default());
        }
        let backup_summary = if backup_errors.is_empty() {
            "没有可用的配置备份".to_string()
        } else {
            format!("配置备份也无法读取：{}", backup_errors.join("；"))
        };
        Err(primary_error).context(backup_summary)
    }

    pub fn save(&self, config: &CodeyConfig) -> Result<()> {
        let config = config.clone().normalize();
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Codey 配置路径无父目录"))?;
        fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec_pretty(&config)?;
        self.rotate_valid_backups()?;
        persist_private_bytes(&self.path, &bytes, "替换 Codey 配置")
    }

    fn backup_path(&self, index: usize) -> PathBuf {
        let file_name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config.json".to_string());
        self.path.with_file_name(format!("{file_name}.bak.{index}"))
    }

    fn rotate_valid_backups(&self) -> Result<()> {
        let mut snapshots = Vec::<Vec<u8>>::new();
        for path in std::iter::once(self.path.clone())
            .chain((1..=CONFIG_BACKUP_COUNT).map(|index| self.backup_path(index)))
        {
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            let Ok(contents) = std::str::from_utf8(&bytes) else {
                continue;
            };
            if parse_config_contents(contents, &path).is_err()
                || snapshots.iter().any(|snapshot| snapshot == &bytes)
            {
                continue;
            }
            snapshots.push(bytes);
            if snapshots.len() == CONFIG_BACKUP_COUNT {
                break;
            }
        }
        for (offset, bytes) in snapshots.iter().enumerate().rev() {
            let path = self.backup_path(offset + 1);
            persist_private_bytes(&path, bytes, "写入 Codey 配置备份")?;
        }
        Ok(())
    }
}

fn read_config_file(path: &Path) -> Result<CodeyConfig> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("读取 Codey 配置失败：{}", path.display()))?;
    parse_config_contents(&contents, path)
}

fn parse_config_contents(contents: &str, path: &Path) -> Result<CodeyConfig> {
    let raw = serde_json::from_str::<serde_json::Value>(contents)
        .with_context(|| format!("解析 Codey 配置失败：{}", path.display()))?;
    let has_initial_import_marker = raw
        .as_object()
        .is_some_and(|object| object.contains_key("initialRouteImportCompleted"));
    let mut config = serde_json::from_value::<CodeyConfig>(raw)
        .with_context(|| format!("解析 Codey 配置失败：{}", path.display()))?;
    if !has_initial_import_marker && !config.looks_like_empty_default_route() {
        config.initial_route_import_completed = true;
    }
    Ok(config.normalize())
}

fn persist_private_bytes(path: &Path, bytes: &[u8], operation: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Codey 配置路径无父目录"))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Codey 配置路径缺少文件名"))?
        .to_string_lossy();
    let temp = parent.join(format!(
        ".{file_name}.codey-{}.tmp",
        Uuid::new_v4().simple()
    ));
    let replace_result = write_private_temp(&temp, bytes).and_then(|()| {
        crate::fs_util::persist_temp_file(&temp, path)
            .with_context(|| format!("{operation}失败：{}", path.display()))
    });
    if replace_result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    replace_result?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
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

    fn named_config(name: &str) -> CodeyConfig {
        let mut config = CodeyConfig::default();
        config.profiles[0].name = name.to_string();
        config
    }

    #[test]
    fn outbound_api_urls_allow_http_or_https() {
        for accepted in [
            "https://api.example.com/v1",
            "http://localhost:11434/v1",
            "http://127.0.0.2:8080/v1",
            "http://[::1]:8080/v1",
            "http://api.example.com/v1",
            "http://192.168.1.8:8080/v1",
        ] {
            assert!(
                validate_outbound_api_url(accepted, "测试 API 地址").is_ok(),
                "{accepted}"
            );
        }

        for rejected in [
            "ftp://localhost/models",
            "https://user:password@api.example.com/v1",
            "http://token@localhost:11434/v1",
        ] {
            assert!(
                validate_outbound_api_url(rejected, "测试 API 地址").is_err(),
                "{rejected}"
            );
        }
    }

    #[test]
    fn enabled_prompt_optimization_requires_complete_connection_settings() {
        let mut optimization = PromptOptimizationConfig {
            enabled: true,
            ..PromptOptimizationConfig::default()
        };
        assert!(optimization.validate().unwrap_err().contains("API 地址"));

        optimization.base_url = "https://api.example.com/v1".to_string();
        assert!(optimization.validate().unwrap_err().contains("API Key"));

        optimization.api_key = "sk-test".to_string();
        assert!(optimization.validate().unwrap_err().contains("模型"));

        optimization.model = "gpt-test".to_string();
        assert!(optimization.validate().is_ok());

        optimization.mode = PROMPT_OPTIMIZATION_MODE_CODEY_ROUTE.to_string();
        optimization.base_url.clear();
        optimization.api_key.clear();
        assert!(optimization.validate().is_ok());
    }

    #[test]
    fn provider_profiles_cannot_shadow_the_internal_router_provider() {
        let mut profile = ProviderProfile::new("Reserved route");
        profile.id = local_router::ROUTER_PROVIDER_ID.to_string();
        profile.base_url = "https://relay.example/v1".into();
        profile.api_key = "sk-test".into();
        profile.normalize();

        assert!(
            profile
                .validate()
                .unwrap_err()
                .contains("Codey 内部 Provider ID")
        );
    }

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
    fn config_load_recovers_from_the_newest_valid_backup() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(directory.path().join("config.json"));

        store.save(&named_config("version-1")).unwrap();
        store.save(&named_config("version-2")).unwrap();
        fs::write(store.path(), b"{broken-json").unwrap();

        let recovered = store.load().unwrap();
        assert_eq!(recovered.profiles[0].name, "version-1");
    }

    #[test]
    fn config_load_skips_a_corrupt_newer_backup() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(directory.path().join("config.json"));

        for version in 1..=4 {
            store
                .save(&named_config(&format!("version-{version}")))
                .unwrap();
        }
        fs::write(store.path(), b"corrupt-primary").unwrap();
        fs::write(store.backup_path(1), b"corrupt-backup").unwrap();

        let recovered = store.load().unwrap();
        assert_eq!(recovered.profiles[0].name, "version-2");
    }

    #[cfg(unix)]
    #[test]
    fn config_backups_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(directory.path().join("config.json"));
        store.save(&named_config("version-1")).unwrap();
        store.save(&named_config("version-2")).unwrap();

        assert_eq!(
            fs::metadata(store.backup_path(1))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
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
    fn legacy_profile_field_aliases_serialize_only_the_generic_schema() {
        let profile = serde_json::from_value::<ProviderProfile>(serde_json::json!({
            "id": "official-profile",
            "name": "Official",
            "baseUrl": "",
            "apiKey": "",
            "ccSwitchProviderId": "openai",
            "ccSwitchReadOnly": true
        }))
        .unwrap();

        assert_eq!(profile.source_provider_id.as_deref(), Some("openai"));
        assert!(profile.official_account);
        let serialized = serde_json::to_value(profile).unwrap();
        assert_eq!(serialized["sourceProviderId"], "openai");
        assert_eq!(serialized["officialAccount"], true);
        assert!(serialized.get("ccSwitchProviderId").is_none());
        assert!(serialized.get("ccSwitchReadOnly").is_none());
    }

    #[test]
    fn deprecated_provider_protocol_fields_are_ignored() {
        let profile = serde_json::from_value::<ProviderProfile>(serde_json::json!({
            "id": "legacy-provider",
            "name": "Legacy Provider",
            "baseUrl": "https://gateway.example/v1",
            "apiKey": "",
            "protocol": "chatCompletions",
            "chatCompletionsModels": ["legacy-model"]
        }))
        .unwrap();

        let serialized = serde_json::to_value(profile).unwrap();

        assert!(serialized.get("protocol").is_none());
        assert!(serialized.get("chatCompletionsModels").is_none());
    }

    #[test]
    fn third_party_websocket_support_defaults_off_and_only_allows_responses() {
        let legacy = serde_json::from_value::<ProviderProfile>(serde_json::json!({
            "id": "legacy-provider",
            "name": "Legacy Provider",
            "shortName": "旧",
            "baseUrl": "https://gateway.example/v1",
            "apiKey": "sk-test",
            "upstreamProtocol": "openaiResponses"
        }))
        .unwrap();
        assert!(!legacy.supports_websockets);
        assert!(!legacy.supports_native_web_search);
        assert!(!legacy.supports_auto_review);

        let mut chat = ProviderProfile::new("Chat Relay");
        chat.base_url = "https://gateway.example/v1".into();
        chat.api_key = "sk-test".into();
        chat.upstream_protocol = UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
        chat.supports_websockets = true;
        chat.normalize();
        assert!(
            chat.validate()
                .unwrap_err()
                .contains("只有 OpenAI Responses")
        );

        let mut responses = ProviderProfile::new("Responses Relay");
        responses.base_url = "https://gateway.example/v1".into();
        responses.api_key = "sk-test".into();
        responses.supports_websockets = true;
        responses.normalize();
        assert!(responses.validate().is_ok());

        let mut official = ProviderProfile::new("OpenAI 官方直登");
        official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
        official.normalize();
        assert!(official.official_account);
        assert!(official.supports_websockets);
        assert!(official.supports_native_web_search);
        assert!(official.supports_auto_review);
        assert_eq!(official.upstream_protocol, UPSTREAM_PROTOCOL_OFFICIAL);
        assert!(official.validate().is_ok());
    }

    #[test]
    fn auto_review_is_filtered_from_regular_model_state() {
        let mut config = CodeyConfig::default();
        let provider_id = config.current_provider_id().unwrap().to_string();
        let models = vec![
            "provider-model".to_string(),
            local_router::CODEX_AUTO_REVIEW_MODEL.to_string(),
        ];
        config
            .selected_models_by_provider
            .insert(provider_id.clone(), models.clone());
        config
            .manual_third_party_models_by_provider
            .insert(provider_id.clone(), models.clone());
        config
            .upstream_models_by_provider
            .insert(provider_id.clone(), models);

        let normalized = config.normalize();

        assert_eq!(
            normalized.enabled_route_models(&provider_id),
            ["provider-model"]
        );
        assert_eq!(
            normalized.upstream_models_by_provider[&provider_id],
            ["provider-model"]
        );
        assert!(!normalized.profiles[0].supports_auto_review);
    }

    #[test]
    fn chat_completions_protocol_still_exposes_responses_to_codex() {
        let mut profile = ProviderProfile::new("Chat Relay");
        profile.upstream_protocol = UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string();
        profile.normalize();

        assert_eq!(
            profile.upstream_protocol,
            UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS
        );
        assert_eq!(profile.runtime_wire_api().unwrap(), "responses");
    }

    #[test]
    fn anthropic_messages_protocol_still_exposes_responses_to_codex() {
        let mut profile = ProviderProfile::new("Anthropic");
        profile.upstream_protocol = UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES.to_string();
        profile.normalize();

        assert_eq!(
            profile.upstream_protocol,
            UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES
        );
        assert_eq!(profile.runtime_wire_api().unwrap(), "responses");
    }

    #[test]
    fn redacted_third_party_route_requires_its_saved_secret_before_validation() {
        let mut saved = ProviderProfile::new("Relay");
        saved.id = "relay".into();
        saved.base_url = "https://relay.example/v1".into();
        saved.api_key = "secret-token".into();
        saved.normalize();

        let mut redacted = saved.clone();
        redacted.api_key.clear();
        redacted.api_key_configured = true;
        assert!(redacted.validate().is_err());

        redacted.merge_redacted_secret(Some(&saved));
        redacted.normalize();
        assert!(redacted.validate().is_ok());
        assert_eq!(redacted.api_key, "secret-token");
    }

    #[test]
    fn third_party_short_names_are_required_limited_and_migrated() {
        let mut route = ProviderProfile::new("中转线路");
        route.id = "relay".into();
        route.base_url = "https://relay.example/v1".into();
        route.api_key = "relay-key".into();

        route.short_name.clear();
        route.normalize();
        assert_eq!(route.validate().unwrap_err(), "线路「中转线路」缺少短名称");

        route.short_name = "中转线".into();
        assert!(route.validate().unwrap_err().contains("最多 2 个字符"));

        route.short_name = OFFICIAL_ROUTE_SHORT_NAME.into();
        assert!(route.validate().unwrap_err().contains("官方账号专属"));

        route.short_name.clear();
        let migrated = CodeyConfig {
            active_profile_id: route.id.clone(),
            profiles: vec![route],
            initial_route_import_completed: true,
            ..CodeyConfig::default()
        }
        .normalize();
        assert_eq!(migrated.profiles[0].short_name, "中转");
        assert!(migrated.profiles[0].validate().is_ok());
    }

    #[test]
    fn legacy_short_name_migration_keeps_route_prefixes_unique() {
        let mut first = ProviderProfile::new("线路 A");
        first.id = "route-a".into();
        first.short_name.clear();
        first.base_url = "https://a.example/v1".into();
        first.api_key = "a-key".into();
        let mut second = ProviderProfile::new("线路 B");
        second.id = "route-b".into();
        second.short_name.clear();
        second.base_url = "https://b.example/v1".into();
        second.api_key = "b-key".into();

        let migrated = CodeyConfig {
            active_profile_id: first.id.clone(),
            profiles: vec![first, second],
            initial_route_import_completed: true,
            ..CodeyConfig::default()
        }
        .normalize();

        assert_eq!(migrated.profiles[0].short_name, "线路");
        assert_eq!(migrated.profiles[1].short_name, "线1");
        assert!(validate_provider_profiles(&migrated.profiles).is_ok());
    }

    #[test]
    fn route_validation_rejects_duplicate_short_names() {
        let mut first = ProviderProfile::new("First");
        first.id = "first".into();
        first.short_name = "同".into();
        first.base_url = "https://first.example/v1".into();
        first.api_key = "first-key".into();
        first.normalize();
        let mut second = ProviderProfile::new("Second");
        second.id = "second".into();
        second.short_name = "同".into();
        second.base_url = "https://second.example/v1".into();
        second.api_key = "second-key".into();
        second.normalize();

        let error = validate_provider_profiles(&[first, second]).unwrap_err();
        assert!(error.contains("相同的短名称：同"));
    }

    #[test]
    fn official_routes_always_use_the_official_short_name() {
        let mut route = ProviderProfile::new("Official");
        route.short_name = "自定".into();
        route.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
        route.normalize();

        assert_eq!(route.short_name, OFFICIAL_ROUTE_SHORT_NAME);
        assert!(route.validate().is_ok());
    }

    #[test]
    fn route_validation_rejects_duplicate_runtime_provider_ids() {
        let mut first = ProviderProfile::new("First");
        first.id = "first".into();
        first.base_url = "https://first.example/v1".into();
        first.api_key = "first-key".into();
        first.source_provider_id = Some("shared-provider".into());
        first.normalize();

        let mut second = ProviderProfile::new("Second");
        second.id = "second".into();
        second.base_url = "https://second.example/v1".into();
        second.api_key = "second-key".into();
        second.source_provider_id = Some("shared-provider".into());
        second.normalize();

        let error = validate_provider_profiles(&[first, second]).unwrap_err();
        assert!(error.contains("shared-provider"));
    }

    #[test]
    fn runtime_catalog_combines_models_from_every_registered_route() {
        let mut official = ProviderProfile::new("Official");
        official.id = "official-profile".into();
        official.source_provider_id = Some("openai".into());
        official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
        official.normalize();

        let mut relay = ProviderProfile::new("Relay");
        relay.id = "relay".into();
        relay.base_url = "https://relay.example/v1".into();
        relay.api_key = "relay-key".into();
        relay.normalize();

        let mut config = CodeyConfig {
            active_profile_id: official.id.clone(),
            profiles: vec![official, relay],
            official_account_available_this_launch: true,
            ..CodeyConfig::default()
        };
        config.upstream_models_by_provider.insert(
            "relay".into(),
            vec!["relay-a".into(), "shared-model".into()],
        );
        config.selected_models_by_provider.insert(
            "relay".into(),
            vec!["shared-model".into(), "manual-model".into()],
        );

        let (upstream, selected) = config.runtime_catalog_models();

        assert!(upstream.iter().any(|model| model == "openai/gpt-5.6-sol"));
        assert!(upstream.iter().any(|model| model == "relay/relay-a"));
        assert!(upstream.iter().any(|model| model == "relay/manual-model"));
        assert_eq!(
            upstream
                .iter()
                .filter(|model| model.as_str() == "relay/shared-model")
                .count(),
            1
        );
        assert_eq!(
            selected,
            [
                "openai/gpt-5.6-sol",
                "openai/gpt-5.6-terra",
                "openai/gpt-5.6-luna",
                "openai/gpt-5.5",
                "openai/gpt-5.4",
                "openai/gpt-5.4-mini",
                "openai/gpt-5.3-codex-spark",
                "relay/shared-model",
                "relay/manual-model",
            ]
        );
    }

    #[test]
    fn websocket_model_aliases_are_scoped_to_the_declaring_route() {
        let mut websocket_route = ProviderProfile::new("WS Route");
        websocket_route.id = "route-ws".into();
        websocket_route.base_url = "https://ws.example/v1".into();
        websocket_route.api_key = "ws-key".into();
        websocket_route.supports_websockets = true;
        websocket_route.normalize();

        let mut http_route = ProviderProfile::new("HTTP Route");
        http_route.id = "route-http".into();
        http_route.base_url = "https://http.example/v1".into();
        http_route.api_key = "http-key".into();
        http_route.normalize();

        let mut config = CodeyConfig {
            active_profile_id: websocket_route.id.clone(),
            profiles: vec![websocket_route, http_route],
            ..CodeyConfig::default()
        }
        .normalize();
        config.selected_models_by_provider.insert(
            "route-ws".into(),
            vec!["shared-model".into(), "ws-only".into()],
        );
        config
            .selected_models_by_provider
            .insert("route-http".into(), vec!["shared-model".into()]);

        assert!(config.runtime_supports_websockets());
        assert_eq!(
            config.runtime_websocket_model_aliases(),
            vec![
                local_router::model_alias("route-ws", "shared-model"),
                local_router::model_alias("route-ws", "ws-only"),
            ]
        );
    }

    #[test]
    fn native_web_search_model_aliases_require_an_explicit_responses_route() {
        let mut search_route = ProviderProfile::new("Search Route");
        search_route.id = "route-search".into();
        search_route.base_url = "https://search.example/v1".into();
        search_route.api_key = "search-key".into();
        search_route.supports_native_web_search = true;
        search_route.normalize();

        let mut regular_route = ProviderProfile::new("Regular Route");
        regular_route.id = "route-regular".into();
        regular_route.base_url = "https://regular.example/v1".into();
        regular_route.api_key = "regular-key".into();
        regular_route.normalize();

        let mut chat_route = ProviderProfile::new("Chat Route");
        chat_route.id = "route-chat".into();
        chat_route.base_url = "https://chat.example/v1".into();
        chat_route.api_key = "chat-key".into();
        chat_route.upstream_protocol = UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
        chat_route.supports_native_web_search = true;
        chat_route.normalize();
        assert!(!chat_route.supports_native_web_search);

        let mut config = CodeyConfig {
            active_profile_id: search_route.id.clone(),
            profiles: vec![search_route, regular_route, chat_route],
            ..CodeyConfig::default()
        }
        .normalize();
        for provider_id in ["route-search", "route-regular", "route-chat"] {
            config
                .selected_models_by_provider
                .insert(provider_id.into(), vec!["gpt-5.6-sol".into()]);
        }

        assert_eq!(
            config.runtime_native_web_search_model_aliases(),
            vec![local_router::model_alias("route-search", "gpt-5.6-sol")]
        );
    }

    #[test]
    fn third_party_remote_compaction_is_advertised_only_when_every_runtime_route_supports_it() {
        let mut capable = ProviderProfile::new("Responses Route");
        capable.id = "route-capable".into();
        capable.base_url = "https://responses.example/v1".into();
        capable.api_key = "responses-key".into();
        capable.supports_remote_compaction = true;
        capable.normalize();

        let mut config = CodeyConfig {
            active_profile_id: capable.id.clone(),
            profiles: vec![capable],
            ..CodeyConfig::default()
        }
        .normalize();
        assert!(config.runtime_supports_remote_compaction());

        let mut unsupported = ProviderProfile::new("Chat Route");
        unsupported.id = "route-chat".into();
        unsupported.base_url = "https://chat.example/v1".into();
        unsupported.api_key = "chat-key".into();
        unsupported.upstream_protocol = UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
        unsupported.supports_remote_compaction = true;
        unsupported.normalize();
        config.profiles.push(unsupported);
        assert!(!config.runtime_supports_remote_compaction());
    }

    #[test]
    fn official_remote_compaction_requires_the_account_this_launch() {
        let mut official = ProviderProfile::new("OpenAI 官方直登");
        official.id = DERIVED_OFFICIAL_PROFILE_ID.into();
        official.source_provider_id = Some("openai".into());
        official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
        official.normalize();
        let mut config = CodeyConfig {
            active_profile_id: official.id.clone(),
            profiles: vec![official],
            official_account_available_this_launch: true,
            ..CodeyConfig::default()
        }
        .normalize();

        assert!(config.runtime_supports_remote_compaction());

        let mut chat = ProviderProfile::new("Chat Relay");
        chat.id = "chat-route".into();
        chat.base_url = "https://chat.example/v1".into();
        chat.api_key = "chat-key".into();
        chat.upstream_protocol = UPSTREAM_PROTOCOL_OPENAI_CHAT_COMPLETIONS.into();
        chat.normalize();
        config.profiles.push(chat);
        assert!(
            !config.runtime_supports_remote_compaction(),
            "a shared provider must not advertise native compaction to an adapted Chat route"
        );

        config.official_account_available_this_launch = false;
        assert!(!config.runtime_supports_remote_compaction());
    }

    #[test]
    fn official_websocket_models_enable_automatically_only_when_login_is_available() {
        let mut official = ProviderProfile::new("OpenAI 官方直登");
        official.id = DERIVED_OFFICIAL_PROFILE_ID.into();
        official.source_provider_id = Some("openai".into());
        official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
        official.normalize();

        let mut config = CodeyConfig {
            active_profile_id: official.id.clone(),
            profiles: vec![official],
            official_account_available_this_launch: true,
            ..CodeyConfig::default()
        }
        .normalize();
        config
            .selected_models_by_provider
            .insert("openai".into(), vec!["gpt-5.6-sol".into()]);

        assert!(config.runtime_supports_websockets());
        assert_eq!(
            config.runtime_websocket_model_aliases(),
            vec![local_router::model_alias("openai", "gpt-5.6-sol")]
        );

        config.official_account_available_this_launch = false;
        assert!(!config.runtime_supports_websockets());
        assert!(config.runtime_websocket_model_aliases().is_empty());
    }

    #[test]
    fn official_native_web_search_models_require_login_this_launch() {
        let mut official = ProviderProfile::new("OpenAI 官方直登");
        official.id = DERIVED_OFFICIAL_PROFILE_ID.into();
        official.source_provider_id = Some("openai".into());
        official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
        official.normalize();

        let mut config = CodeyConfig {
            active_profile_id: official.id.clone(),
            profiles: vec![official],
            official_account_available_this_launch: true,
            ..CodeyConfig::default()
        }
        .normalize();
        config
            .selected_models_by_provider
            .insert("openai".into(), vec!["gpt-5.6-sol".into()]);

        assert_eq!(
            config.runtime_native_web_search_model_aliases(),
            vec![local_router::model_alias("openai", "gpt-5.6-sol")]
        );
        config.official_account_available_this_launch = false;
        assert!(config.runtime_native_web_search_model_aliases().is_empty());
    }

    #[test]
    fn effective_proxy_disables_runtime_websocket_route_advertising() {
        let mut official = ProviderProfile::new("OpenAI 官方直登");
        official.id = DERIVED_OFFICIAL_PROFILE_ID.into();
        official.source_provider_id = Some("openai".into());
        official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
        official.normalize();
        let config = CodeyConfig {
            active_profile_id: official.id.clone(),
            profiles: vec![official],
            official_account_available_this_launch: true,
            ..CodeyConfig::default()
        }
        .normalize();

        assert!(
            !config.route_supports_websockets_this_launch_with_proxy(&config.profiles[0], true)
        );
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
    fn migrates_provider_defaults_to_one_route_aware_global_default() {
        let mut route_a = ProviderProfile::new("Route A");
        route_a.id = "route-a".into();
        route_a.base_url = "https://route-a.example/v1".into();
        route_a.api_key = "route-a-key".into();
        route_a.normalize();
        let mut route_b = ProviderProfile::new("Route B");
        route_b.id = "route-b".into();
        route_b.base_url = "https://route-b.example/v1".into();
        route_b.api_key = "route-b-key".into();
        route_b.normalize();

        let mut config = CodeyConfig {
            active_profile_id: route_b.id.clone(),
            profiles: vec![route_a.clone(), route_b.clone()],
            initial_route_import_completed: true,
            ..CodeyConfig::default()
        };
        for provider_id in [route_a.provider_id(), route_b.provider_id()] {
            config
                .selected_models_by_provider
                .insert(provider_id.to_string(), vec!["shared-model".into()]);
            config
                .default_model_by_provider
                .insert(provider_id.to_string(), "shared-model".into());
        }

        let normalized = config.normalize();
        let serialized = serde_json::to_value(&normalized).unwrap();
        let target = normalized.effective_runtime_default_target().unwrap();

        assert_eq!(normalized.default_model, "route-b/shared-model");
        assert!(normalized.default_model_by_provider.is_empty());
        assert_eq!(serialized["defaultModel"], "route-b/shared-model");
        assert!(serialized.get("defaultModelByProvider").is_none());
        assert_eq!(normalized.default_model_for_profile(&route_a), None,);
        assert_eq!(
            normalized.default_model_for_profile(&route_b).as_deref(),
            Some("shared-model"),
        );
        assert_eq!(target.route_id, "route-b");
        assert_eq!(target.provider_id, "route-b");
        assert_eq!(target.request_provider_id, local_router::ROUTER_PROVIDER_ID);
        assert_eq!(target.request_model, "shared-model");
        assert_eq!(target.upstream_model, "shared-model");
        assert!(!target.official);
    }

    #[test]
    fn non_empty_legacy_configs_are_marked_as_imported() {
        let mut route = ProviderProfile::new("Relay");
        route.id = "relay".into();
        route.base_url = "https://relay.example/v1".into();
        route.api_key = "sk-relay".into();
        route.normalize();
        let config = CodeyConfig {
            active_profile_id: route.id.clone(),
            profiles: vec![route],
            initial_route_import_completed: false,
            ..CodeyConfig::default()
        }
        .normalize();

        assert!(config.initial_route_import_completed);
    }

    #[test]
    fn launch_official_profile_is_first_without_stealing_active_third_party_route() {
        let mut relay = ProviderProfile::new("Relay");
        relay.id = "relay".into();
        relay.base_url = "https://relay.example/v1".into();
        relay.api_key = "sk-relay".into();
        relay.normalize();
        let mut official = ProviderProfile::new("OpenAI 官方直登");
        official.id = "openai-source".into();
        official.source_provider_id = Some("openai".into());
        official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
        official.normalize();
        let mut config = CodeyConfig {
            active_profile_id: relay.id.clone(),
            profiles: vec![relay],
            initial_route_import_completed: true,
            ..CodeyConfig::default()
        };
        config
            .default_model_by_provider
            .insert("openai".into(), "gpt-5.6-sol".into());

        config.apply_launch_official_profile(Some(official));
        config = config.normalize();

        assert_eq!(config.profiles[0].id, DERIVED_OFFICIAL_PROFILE_ID);
        assert_eq!(config.active_profile_id, "relay");
        assert_eq!(config.profiles[0].provider_id(), "openai");
        assert_eq!(config.default_model, "openai/gpt-5.6-sol");
        assert!(config.default_model_by_provider.is_empty());
        assert_eq!(
            config.selected_models_by_provider["openai"],
            model_catalog::default_official_model_slugs(),
        );

        config
            .selected_models_by_provider
            .insert("openai".into(), vec!["gpt-5.6-sol".into()]);
        config = config.normalize();
        assert_eq!(
            config.selected_models_by_provider["openai"],
            ["gpt-5.6-sol"],
        );
    }

    #[test]
    fn launch_official_profile_migrates_legacy_official_provider_state() {
        let mut legacy = ProviderProfile::new("OpenAI 官方直登");
        legacy.id = DERIVED_OFFICIAL_PROFILE_ID.into();
        legacy.source_provider_id = Some("local-official".into());
        legacy.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
        legacy.normalize();

        let mut launched = ProviderProfile::new("OpenAI 官方直登");
        launched.id = "launch-openai".into();
        launched.source_provider_id = Some("openai".into());
        launched.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
        launched.normalize();

        let mut config = CodeyConfig {
            active_profile_id: legacy.id.clone(),
            profiles: vec![legacy],
            initial_route_import_completed: true,
            default_model: "local-official/gpt-5.6-terra".into(),
            subagent_model: "local-official/gpt-5.6-luna".into(),
            subagent_reasoning_effort: "high".into(),
            subagent_roles: uniform_subagent_roles("local-official/gpt-5.6-luna", "high"),
            ..CodeyConfig::default()
        };
        config
            .selected_models_by_provider
            .insert("local-official".into(), vec!["gpt-5.6-terra".into()]);
        config
            .manual_third_party_models_by_provider
            .insert("local-official".into(), vec!["manual-model".into()]);
        config
            .declared_official_models_by_provider
            .insert("local-official".into(), vec!["gpt-5.6-luna".into()]);
        config
            .upstream_models_by_provider
            .insert("local-official".into(), vec!["gpt-5.6-terra".into()]);
        config
            .default_model_by_provider
            .insert("local-official".into(), "gpt-5.6-terra".into());

        config.apply_launch_official_profile(Some(launched));

        assert_eq!(config.profiles.len(), 1);
        assert_eq!(config.profiles[0].id, DERIVED_OFFICIAL_PROFILE_ID);
        assert_eq!(config.profiles[0].provider_id(), "openai");
        assert_eq!(config.active_profile_id, DERIVED_OFFICIAL_PROFILE_ID);
        assert_eq!(
            config.selected_models_by_provider["openai"],
            ["gpt-5.6-terra"]
        );
        assert_eq!(
            config.manual_third_party_models_by_provider["openai"],
            ["manual-model"]
        );
        assert_eq!(
            config.declared_official_models_by_provider["openai"],
            ["gpt-5.6-luna"]
        );
        assert_eq!(
            config.upstream_models_by_provider["openai"],
            ["gpt-5.6-terra"]
        );
        assert_eq!(config.default_model_by_provider["openai"], "gpt-5.6-terra");
        assert_eq!(config.default_model, "openai/gpt-5.6-terra");
        assert_eq!(config.subagent_model, "openai/gpt-5.6-luna");
        assert!(
            config
                .subagent_roles
                .values()
                .all(|selection| selection.model == "openai/gpt-5.6-luna")
        );
        assert!(
            !config
                .selected_models_by_provider
                .contains_key("local-official")
        );
        assert!(
            !config
                .manual_third_party_models_by_provider
                .contains_key("local-official")
        );
        assert!(
            !config
                .declared_official_models_by_provider
                .contains_key("local-official")
        );
        assert!(
            !config
                .upstream_models_by_provider
                .contains_key("local-official")
        );
        assert!(
            !config
                .default_model_by_provider
                .contains_key("local-official")
        );
    }

    #[test]
    fn api_key_launch_removes_derived_official_route_and_falls_back_to_saved_route() {
        let mut official = ProviderProfile::new("OpenAI 官方直登");
        official.id = DERIVED_OFFICIAL_PROFILE_ID.into();
        official.source_provider_id = Some("openai".into());
        official.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.into();
        official.normalize();
        let mut relay = ProviderProfile::new("Relay");
        relay.id = "relay".into();
        relay.base_url = "https://relay.example/v1".into();
        relay.api_key = "sk-relay".into();
        relay.normalize();
        let mut config = CodeyConfig {
            active_profile_id: official.id.clone(),
            profiles: vec![official, relay],
            initial_route_import_completed: true,
            ..CodeyConfig::default()
        };
        config
            .default_model_by_provider
            .insert("openai".into(), "gpt-5.6-sol".into());

        config.apply_launch_official_profile(None);
        config = config.normalize();

        assert_eq!(config.profiles.len(), 1);
        assert_eq!(config.profiles[0].id, "relay");
        assert_eq!(config.active_profile_id, "relay");
        assert_eq!(config.default_model, "gpt-5.6-sol");
        assert!(config.default_model_by_provider.is_empty());
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
            ["UPSTREAM-A", "gpt-5.6-sol"]
        );
        assert_eq!(
            normalized.declared_official_models_by_provider[&provider_id],
            ["GPT-5.6-SOL"]
        );
    }

    #[test]
    fn legacy_official_models_are_reclassified_and_survive_persistence() {
        let mut config = CodeyConfig::default();
        let provider_id = config.current_provider_id().unwrap().to_string();
        config.selected_models_by_provider.insert(
            provider_id.clone(),
            vec![
                "GPT-5.6-Luna".into(),
                "provider-custom".into(),
                "gpt-5.6-sol".into(),
            ],
        );
        config.manual_third_party_models_by_provider.insert(
            provider_id.clone(),
            vec![
                "GPT-5.6-Terra".into(),
                "provider-custom".into(),
                "manual-only".into(),
            ],
        );
        config
            .declared_official_models_by_provider
            .insert(provider_id.clone(), vec!["GPT-5.6-SOL".into()]);
        config
            .upstream_models_by_provider
            .insert(provider_id.clone(), Vec::new());

        let normalized = config.normalize();

        assert_eq!(
            normalized.selected_models_by_provider[&provider_id],
            ["provider-custom"]
        );
        assert_eq!(
            normalized.manual_third_party_models_by_provider[&provider_id],
            ["provider-custom", "manual-only"]
        );
        assert_eq!(
            normalized.declared_official_models_by_provider[&provider_id],
            ["GPT-5.6-SOL", "gpt-5.6-luna", "gpt-5.6-terra"]
        );
        assert_eq!(
            normalized.upstream_models_by_provider[&provider_id],
            ["gpt-5.6-sol", "gpt-5.6-luna", "gpt-5.6-terra"]
        );

        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(directory.path().join("config.json"));
        store.save(&normalized).unwrap();
        let reloaded = store.load().unwrap();
        assert_eq!(
            reloaded.declared_official_models_by_provider[&provider_id],
            ["GPT-5.6-SOL", "gpt-5.6-luna", "gpt-5.6-terra"]
        );
        assert_eq!(
            reloaded.upstream_models_by_provider[&provider_id],
            ["gpt-5.6-sol", "gpt-5.6-luna", "gpt-5.6-terra"]
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
    fn local_router_defaults_to_enabled_for_existing_configs() {
        let legacy = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();
        let disabled = serde_json::from_str::<CodeyConfig>(
            r#"{"activeProfileId":"","profiles":[],"localRouterEnabled":false}"#,
        )
        .unwrap()
        .normalize();

        assert!(legacy.local_router_enabled);
        assert!(!disabled.local_router_enabled);
        assert_eq!(
            serde_json::to_value(disabled).unwrap()["localRouterEnabled"],
            serde_json::json!(false)
        );
    }

    #[test]
    fn route_request_log_is_opt_in_and_normalizes_resource_bounds() {
        let legacy = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();
        assert!(!legacy.route_request_log.enabled);

        let configured = serde_json::from_str::<CodeyConfig>(
            r#"{
                "activeProfileId":"",
                "profiles":[],
                "routeRequestLog":{
                    "enabled":true,
                    "backend":"sqlite",
                    "queueCapacity":1,
                    "batchSize":999999,
                    "flushIntervalMs":1,
                    "shutdownFlushTimeoutMs":999999,
                    "sampleRatePerMillion":9999999,
                    "maxFileBytes":1,
                    "retainedFiles":0,
                    "retentionDays":0
                }
            }"#,
        )
        .unwrap()
        .normalize();
        assert!(configured.route_request_log.enabled);
        assert_eq!(
            configured.route_request_log.backend,
            RouteRequestLogBackend::Sqlite
        );
        assert_eq!(configured.route_request_log.queue_capacity, 128);
        assert_eq!(configured.route_request_log.batch_size, 128);
        assert_eq!(configured.route_request_log.flush_interval_ms, 50);
        assert_eq!(
            configured.route_request_log.shutdown_flush_timeout_ms,
            10_000
        );
        assert_eq!(
            configured.route_request_log.sample_rate_per_million,
            1_000_000
        );
        assert_eq!(configured.route_request_log.max_file_bytes, 1024 * 1024);
        assert_eq!(configured.route_request_log.retained_files, 1);
        assert_eq!(configured.route_request_log.retention_days, 1);
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
    fn retired_fast_startup_setting_is_ignored_and_removed_on_serialize() {
        let config = serde_json::from_str::<CodeyConfig>(
            r#"{"activeProfileId":"","profiles":[],"fastCodexStartup":true}"#,
        )
        .unwrap()
        .normalize();

        let serialized = serde_json::to_value(config).unwrap();
        assert!(serialized.get("fastCodexStartup").is_none());
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
                .all(|selection| selection.enabled && selection.model == DEFAULT_SUBAGENT_MODEL)
        );
    }

    #[test]
    fn fresh_subagent_defaults_keep_the_original_role_preset() {
        let config = CodeyConfig::default();

        assert!(
            config
                .subagent_roles
                .values()
                .all(|selection| selection.model == DEFAULT_SUBAGENT_MODEL)
        );
        assert_eq!(
            config.subagent_roles[SUBAGENT_ROLE_WORKER].reasoning_effort,
            "medium"
        );
        assert_eq!(
            config.subagent_roles[SUBAGENT_ROLE_VISUAL_WORKER].reasoning_effort,
            "high"
        );
    }

    #[test]
    fn legacy_subagent_roles_default_to_enabled_and_explicit_disables_survive_normalization() {
        let config = serde_json::from_str::<CodeyConfig>(
            r#"{
                "activeProfileId":"",
                "profiles":[],
                "subagentRoles":{
                    "codey_worker":{"enabled":false,"model":"worker-model","reasoningEffort":"high"},
                    "default":{"model":"fallback-model","reasoningEffort":"medium"}
                }
            }"#,
        )
        .unwrap()
        .normalize();

        assert!(!config.subagent_roles[SUBAGENT_ROLE_WORKER].enabled);
        assert!(config.subagent_roles[SUBAGENT_ROLE_QUICK_SCAN].enabled);
        assert!(config.subagent_roles[SUBAGENT_ROLE_DEFAULT].enabled);
    }

    #[test]
    fn subagent_models_use_unique_router_aliases_without_changing_explicit_routes() {
        for (router_enabled, duplicate, requested, expected) in [
            (true, false, " worker-model ", "route-ws/worker-model"),
            (true, false, "WORKER-MODEL", "route-ws/worker-model"),
            (false, false, "worker-model", "worker-model"),
            (true, true, "worker-model", "worker-model"),
            (
                true,
                true,
                "route-http/worker-model",
                "route-http/worker-model",
            ),
            (true, false, "unknown-model", "unknown-model"),
            (true, false, "vendor/model", "route-ws/vendor/model"),
        ] {
            let mut websocket_route = ProviderProfile::new("WS Route");
            websocket_route.id = "route-ws".into();
            websocket_route.supports_websockets = true;
            let mut http_route = ProviderProfile::new("HTTP Route");
            http_route.id = "route-http".into();
            let config = CodeyConfig {
                local_router_enabled: router_enabled,
                active_profile_id: http_route.id.clone(),
                profiles: vec![websocket_route, http_route],
                selected_models_by_provider: BTreeMap::from([
                    (
                        "route-ws".into(),
                        vec!["worker-model".into(), "vendor/model".into()],
                    ),
                    (
                        "route-http".into(),
                        vec![
                            if duplicate {
                                "worker-model"
                            } else {
                                "main-model"
                            }
                            .into(),
                        ],
                    ),
                ]),
                default_model: "route-http/main-model".into(),
                subagent_model: requested.into(),
                subagent_roles: uniform_subagent_roles(requested, "high"),
                ..CodeyConfig::default()
            }
            .normalize();

            assert_eq!(config.subagent_model, expected, "requested: {requested}");
            assert!(config.subagent_roles.values().all(|selection| {
                selection.model == expected && selection.reasoning_effort == "high"
            }));
            assert_eq!(config.clone().normalize(), config);
        }
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
    fn subagent_config_is_global_and_obsolete_provider_entries_are_ignored() {
        let mut provider_a = ProviderProfile::new("A");
        provider_a.id = "provider-a".into();
        let mut provider_b = ProviderProfile::new("B");
        provider_b.id = "provider-b".into();
        let mut config = CodeyConfig {
            active_profile_id: provider_a.id.clone(),
            profiles: vec![provider_a, provider_b],
            selected_models_by_provider: BTreeMap::from([
                ("provider-a".into(), vec!["model-a".into()]),
                ("provider-b".into(), vec!["model-b".into()]),
            ]),
            subagent_model: "provider-a/model-a".into(),
            subagent_reasoning_effort: "high".into(),
            subagent_roles: uniform_subagent_roles("provider-a/model-a", "high"),
            ..CodeyConfig::default()
        }
        .normalize();

        assert_eq!(config.subagent_model, "provider-a/model-a");

        config.active_profile_id = "provider-b".into();
        config = config.normalize();
        assert_eq!(config.subagent_model, "provider-a/model-a");
        assert!(
            config
                .subagent_roles
                .values()
                .all(|selection| selection.model == "provider-a/model-a")
        );

        let serialized = serde_json::to_value(&config).unwrap();
        assert!(serialized.get("subagentConfigByProvider").is_none());

        let obsolete = serde_json::from_value::<CodeyConfig>(serde_json::json!({
            "activeProfileId": "",
            "profiles": [],
            "subagentModel": "global-model",
            "subagentReasoningEffort": "high",
            "subagentConfigByProvider": {
                "provider-b": {
                    "model": "provider-b/model-b",
                    "reasoningEffort": "low",
                    "roles": {}
                }
            }
        }))
        .unwrap()
        .normalize();
        assert_eq!(obsolete.subagent_model, "global-model");
        assert!(serde_json::to_value(obsolete).unwrap()["subagentConfigByProvider"].is_null());
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
    fn prompt_optimization_defaults_to_disabled_for_existing_configs() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[]}"#)
            .unwrap()
            .normalize();

        assert!(!config.prompt_optimization.enabled);
        assert!(config.prompt_optimization.api_key.is_empty());
    }

    #[test]
    fn prompt_optimization_round_trips_without_persisting_clear_flag() {
        let config = serde_json::from_str::<CodeyConfig>(r#"{"activeProfileId":"","profiles":[],"promptOptimization":{"enabled":true,"mode":"manual","baseUrl":" https://api.example.com/v1/ ","apiKey":"sk-secret","model":" gpt-x ","upstreamProtocol":"anthropicMessages","instruction":" 保持简洁 "}}"#)
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
            config.prompt_optimization.mode,
            PROMPT_OPTIMIZATION_MODE_MANUAL
        );
        assert_eq!(
            config.prompt_optimization.upstream_protocol,
            UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES
        );
        assert_eq!(config.prompt_optimization.instruction, "保持简洁");
        assert_eq!(
            serialized["promptOptimization"]["upstreamProtocol"],
            UPSTREAM_PROTOCOL_ANTHROPIC_MESSAGES
        );
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
