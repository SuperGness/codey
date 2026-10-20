use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::WebhookConfig;
use super::normalize::*;
use crate::local_router;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RemoteCompactionProtocol {
    #[default]
    Responses,
    CompactEndpoint,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoteCompactionBlocker {
    pub(crate) route_id: String,
    pub(crate) route_name: String,
    pub(crate) reason: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfile {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub name: String,
    #[serde(default)]
    pub short_name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_upstream_protocol")]
    pub upstream_protocol: String,
    #[serde(default)]
    pub auth_mode: String,
    #[serde(default)]
    pub api_key_configured: bool,
    #[serde(default, skip_serializing)]
    pub clear_api_key: bool,
    /// Per-route request headers editable in the local router settings.
    #[serde(default)]
    pub model_request_headers: BTreeMap<String, String>,
    /// Optional per-route outbound proxy URL (http/https/socks5/socks5h)。
    /// 设置后该线路的上游流量改走此代理而非系统代理，并禁用上游 WebSocket。
    #[serde(default)]
    pub upstream_proxy: String,
    /// Stable id of the provider in the source Codex configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_provider_id: Option<String>,
    /// 这条线路由启用中的原生插件登记。用户填写密钥并改动线路后会清空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_owner_id: Option<String>,
    /// 上次由插件提交并写入的线路描述，用来判断用户是否改过线路本身。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_route_spec: Option<crate::codey_plugins::PluginRouteSpec>,
    #[serde(default)]
    pub official_account: bool,
    /// 官方账号线路的最高并发数；0 表示不限制。
    #[serde(default)]
    pub max_concurrency: u32,
    /// Codey account id when this route is derived from one stored official
    /// account. Every stored account owns exactly one derived official route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official_account_id: Option<String>,
    /// 该官方账号当前持有可用的 SIWC（官方直连）凭据；由账号同步写入。
    /// 直连线路只走 OpenAI 公开 API，backend-api 专属能力（WS 传输、远程
    /// 压缩等）随之关闭，凭据失效时回落到 Codex 通道。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub official_siwc: bool,
    /// Preserve the exact Codex provider identity required for remote
    /// compaction when it was explicitly enabled by the source configuration.
    #[serde(default)]
    pub supports_remote_compaction: bool,
    /// 上游压缩协议；旧配置保持原生 Responses 行为，兼容端点由用户明确选择。
    #[serde(default)]
    pub remote_compaction_protocol: RemoteCompactionProtocol,
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
/// 官方账号默认线路名的前缀，完整形式是「官方账号1」这类编号。
pub const OFFICIAL_ROUTE_NAME_PREFIX: &str = "官方账号";
/// 短名称在第 10 个账号之后使用的字母编号表，顺序对应编号 10 起的取值。
pub const OFFICIAL_ROUTE_SHORT_NAME_LETTERS: &str =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
pub const MAX_ROUTE_SHORT_NAME_CHARS: usize = 4;
/// 线路名在界面、线路列表和模型选择器里都要能完整显示，前后端共用同一个上限。
/// 旧配置里超过上限的名称仍然可以加载，但保存时的改动会被拒绝。
pub const MAX_ROUTE_NAME_CHARS: usize = 15;

/// 官方账号默认线路名按添加顺序编号，第一个账号是「官方账号1」。
pub fn default_official_route_name(index: usize) -> String {
    format!("{OFFICIAL_ROUTE_NAME_PREFIX}{index}")
}

/// Stable profile id of the derived official route that belongs to one stored
/// account. The id doubles as the route-scoped provider id, so several
/// accounts can be selected side by side in one conversation.
pub fn official_profile_id(account_id: &str) -> String {
    let cleaned = account_id
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!("{DERIVED_OFFICIAL_PROFILE_ID}-{cleaned}")
}

/// Whether one route-scoped provider id belongs to a derived official account
/// route.
pub fn is_official_profile_id(provider_id: &str) -> bool {
    let provider_id = provider_id.trim();
    provider_id == DERIVED_OFFICIAL_PROFILE_ID
        || provider_id
            .strip_prefix(DERIVED_OFFICIAL_PROFILE_ID)
            .is_some_and(|suffix| suffix.starts_with('-'))
}

impl ProviderProfile {
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            id: Uuid::new_v4().to_string(),
            enabled: true,
            short_name: String::new(),
            name,
            base_url: String::new(),
            api_key: String::new(),
            upstream_protocol: default_upstream_protocol(),
            auth_mode: default_auth_mode(),
            api_key_configured: false,
            clear_api_key: false,
            model_request_headers: BTreeMap::new(),
            upstream_proxy: String::new(),
            source_provider_id: None,
            plugin_owner_id: None,
            plugin_route_spec: None,
            official_account: false,
            max_concurrency: 0,
            official_account_id: None,
            official_siwc: false,
            supports_remote_compaction: false,
            remote_compaction_protocol: RemoteCompactionProtocol::default(),
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

    pub(crate) fn display_short_name(&self) -> String {
        let short_name = self.short_name.trim();
        if short_name.is_empty() {
            default_route_short_name(&self.name)
        } else {
            short_name.to_string()
        }
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

    /// 旧配置没有认证类型，仅为无 API Key 的官方端点恢复账号线路。
    pub(super) fn resolves_to_official_account_route(&self) -> bool {
        self.official_account
            || self.auth_mode.trim() == AUTH_MODE_OFFICIAL_ACCOUNT
            || (self.auth_mode.trim().is_empty()
                && self.api_key.is_empty()
                && !self.api_key_configured
                && crate::codex_provider::is_official_base_url(&self.base_url))
    }

    pub(crate) fn normalize(&mut self) {
        self.id = self.id.trim().to_string();
        self.name = self.name.trim().to_string();
        self.short_name = self.short_name.trim().to_string();
        self.base_url = self.base_url.trim().trim_end_matches('/').to_string();
        self.api_key = self.api_key.trim().to_string();
        self.upstream_proxy = self.upstream_proxy.trim().to_string();
        self.source_provider_id = self
            .source_provider_id
            .take()
            .map(|provider_id| provider_id.trim().to_string())
            .filter(|provider_id| !provider_id.is_empty());
        self.official_account_id = self
            .official_account_id
            .take()
            .map(|account_id| account_id.trim().to_string())
            .filter(|account_id| !account_id.is_empty());
        self.upstream_protocol = normalize_upstream_protocol(&self.upstream_protocol);
        if self.resolves_to_official_account_route() {
            self.official_account = true;
        }
        self.auth_mode = normalize_auth_mode(&self.auth_mode, self.official_account);
        if self.auth_mode == AUTH_MODE_OFFICIAL_ACCOUNT {
            self.official_account = true;
            self.api_key.clear();
            self.supports_remote_compaction = true;
            self.remote_compaction_protocol = RemoteCompactionProtocol::Responses;
            self.supports_websockets = true;
            self.supports_native_web_search = true;
            self.supports_auto_review = true;
            self.upstream_protocol = UPSTREAM_PROTOCOL_OFFICIAL.to_string();
            self.auth_mode = AUTH_MODE_OFFICIAL_ACCOUNT.to_string();
        } else {
            self.official_account = false;
            self.official_siwc = false;
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

    pub(crate) fn validate_upstream_protocol_input(&self) -> Result<(), String> {
        if recognized_upstream_protocol(&self.upstream_protocol).is_none() {
            return Err(format!(
                "线路「{}」使用了不支持的上游协议：{}",
                self.name.trim(),
                self.upstream_protocol.trim()
            ));
        }
        Ok(())
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
        self.validate_upstream_protocol_input()?;
        if self.id.trim().is_empty() {
            return Err("线路 ID 不能为空".to_string());
        }
        if local_router::is_router_provider(self.provider_id()) {
            return Err(format!(
                "线路不能使用 Codey 内部 Provider ID「{}」",
                self.provider_id()
            ));
        }
        let short_name = self.display_short_name();
        if short_name.is_empty() {
            return Err("请输入短名称".to_string());
        }
        let name = if self.name.trim().is_empty() {
            short_name.as_str()
        } else {
            self.name.trim()
        };
        local_router::prepare_upstream_headers(
            self,
            local_router::UpstreamProtocol::from_profile(
                self.official_account,
                &self.upstream_protocol,
            ),
        )?;
        if self.supports_websockets
            && !self.official_account
            && self.upstream_protocol != UPSTREAM_PROTOCOL_OPENAI_RESPONSES
        {
            return Err(format!(
                "线路「{name}」只有 OpenAI Responses 协议可以启用 WebSocket"
            ));
        }
        if !self.upstream_proxy.trim().is_empty() {
            validate_outbound_proxy_url(
                self.upstream_proxy.trim(),
                &format!("线路「{name}」的上游代理"),
            )?;
        }
        let short_name = self.short_name.trim();
        if short_name.chars().count() > MAX_ROUTE_SHORT_NAME_CHARS {
            return Err(format!(
                "线路「{name}」的短名称最多 {MAX_ROUTE_SHORT_NAME_CHARS} 个字符"
            ));
        }
        // Official routes may keep any short name; the third-party routes must
        // leave the default official prefix to them.
        if self.auth_mode == AUTH_MODE_OFFICIAL_ACCOUNT || self.official_account {
            return Ok(());
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
        if self.api_key.trim().is_empty() && self.plugin_owner_id.is_none() {
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

pub(super) fn default_prompt_optimization_mode() -> String {
    PROMPT_OPTIMIZATION_MODE_MANUAL.to_string()
}

pub(super) fn default_prompt_optimization_upstream_protocol() -> String {
    UPSTREAM_PROTOCOL_OPENAI_RESPONSES.to_string()
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
#[serde(rename_all = "camelCase", default)]
pub struct RouteRequestLogConfig {
    pub enabled: bool,
    pub backend: RouteRequestLogBackend,
    pub queue_capacity: usize,
    pub batch_size: usize,
    pub flush_interval_ms: u64,
    pub shutdown_flush_timeout_ms: u64,
    /// Deterministic process-local sampling in parts per million.
    pub sample_rate_per_million: u32,
    pub max_file_bytes: u64,
    pub retained_files: usize,
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
    pub(super) fn normalize(&mut self) {
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

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CodeyUpdatePolicy {
    Off,
    #[default]
    Stable,
    Experimental,
}

impl<'de> Deserialize<'de> for CodeyUpdatePolicy {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::Bool(false) => Ok(Self::Off),
            serde_json::Value::Bool(true) => Ok(Self::Stable),
            serde_json::Value::String(value) => match value.as_str() {
                "off" => Ok(Self::Off),
                "stable" => Ok(Self::Stable),
                "experimental" => Ok(Self::Experimental),
                _ => Err(serde::de::Error::custom("Codey 更新策略无效")),
            },
            _ => Err(serde::de::Error::custom("Codey 更新策略无效")),
        }
    }
}

impl CodeyUpdatePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Stable => "stable",
            Self::Experimental => "experimental",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodeyConfig {
    #[serde(default)]
    pub settings_revision: u64,
    #[serde(default, alias = "autoCheckCodeyUpdates")]
    pub codey_update_policy: CodeyUpdatePolicy,
    /// Controls whether Codey installs and uses its process-local multi-route
    /// gateway. Missing values default to enabled so existing installations
    /// keep their current behavior after upgrading.
    #[serde(default = "default_true")]
    pub local_router_enabled: bool,
    /// Structured, best-effort request observations for the built-in router.
    /// Disabled by default so existing installations pay no producer cost.
    #[serde(default)]
    pub route_request_log: RouteRequestLogConfig,
    /// Number of times Codex retries a dropped streaming session.
    #[serde(default = "default_stream_max_retries")]
    pub stream_max_retries: u32,
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
    #[serde(default)]
    pub model_context_by_provider: BTreeMap<String, BTreeMap<String, ModelContextConfig>>,
    /// User-declared thinking levels for third-party route models, keyed by
    /// provider then model. An entry replaces the automatic template list; the
    /// value of each level is the string declared to Codex and sent upstream.
    #[serde(default)]
    pub model_reasoning_efforts_by_provider:
        BTreeMap<String, BTreeMap<String, Vec<ModelReasoningEffort>>>,
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
    /// Previously published selectors mapped to upstream ids. Retained across
    /// route removal and router-mode changes; contains no connection secrets.
    #[serde(default)]
    pub model_alias_history: BTreeMap<String, String>,
    /// One route-aware default model for the entire Codey model catalog.
    /// Official models keep their raw model id; third-party models use the
    /// local-router alias (`provider/model`) so equal upstream ids remain
    /// unambiguous across suppliers.
    #[serde(default)]
    pub default_model: String,
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
    /// Optional launch proxy (http/https/socks5/socks5h, credentials allowed)
    /// applied to the next Codey-managed Codex launch: `--proxy-server` for
    /// the client and proxy environment variables for the app-server
    /// processes it spawns. Credentialed upstreams go through the local auth
    /// relay so Chromium never receives credentials.
    #[serde(default)]
    pub codex_launch_proxy: String,
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
    /// One route-aware model for Codex housekeeping calls: conversation
    /// naming, Git commit and pull-request message generation, and the
    /// automatic review fallback when no route serves `codex-auto-review`.
    /// Empty keeps the native Codex behavior untouched.
    #[serde(default)]
    pub misc_model: String,
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
    /// Optional release-admin Worker endpoint for device-aware publishing.
    /// This is build-time configuration, never a user setting.
    #[serde(
        default = "default_release_admin_url",
        skip_serializing,
        skip_deserializing
    )]
    pub release_admin_url: String,
}

/// User-declared operating budget, never proof of upstream model capacity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelContextConfig {
    pub context_window_tokens: u64,
    #[serde(default)]
    pub auto_compact_token_limit: Option<u64>,
    #[serde(default)]
    pub reserve_output_tokens: Option<u64>,
}

impl ModelContextConfig {
    pub fn validate(&self) -> Result<(), String> {
        let window = self.context_window_tokens;
        if !(1_024..=10_000_000).contains(&window) {
            return Err("上下文窗口必须是 1024 到 10000000 之间的整数 Token".into());
        }
        let reserve = self.reserve_output_tokens.unwrap_or(0);
        if self.reserve_output_tokens == Some(0)
            || reserve >= window
            || (window - reserve) * 100 / window == 0
        {
            return Err("输出预留必须为正整数，并至少保留 1% 的上下文输入空间".into());
        }
        let effective = window * ((window - reserve) * 100 / window) / 100;
        if self
            .auto_compact_token_limit
            .is_some_and(|limit| limit == 0 || limit > effective.min(window * 9 / 10))
        {
            return Err(
                "压缩阈值必须为正整数，且不超过窗口的 90% 和扣除输出预留后的有效窗口".into(),
            );
        }
        Ok(())
    }
}

/// Selectable thinking levels for third-party models. The level identifies the
/// user's intent; `value` is what Codex declares and sends upstream.
pub(crate) const MODEL_REASONING_EFFORT_LEVELS: [&str; 6] =
    ["low", "medium", "high", "xhigh", "max", "ultra"];
pub(crate) const MAX_MODEL_REASONING_EFFORT_VALUE_BYTES: usize = 32;
pub(crate) const MAX_MODEL_REASONING_EFFORTS: usize = 6;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelReasoningEffort {
    pub level: String,
    pub value: String,
}

impl Default for CodeyConfig {
    fn default() -> Self {
        let profile = ProviderProfile::new("默认配置");
        Self {
            settings_revision: 0,
            codey_update_policy: CodeyUpdatePolicy::Stable,
            local_router_enabled: true,
            route_request_log: RouteRequestLogConfig::default(),
            stream_max_retries: default_stream_max_retries(),
            active_profile_id: profile.id.clone(),
            profiles: vec![profile],
            webhook: WebhookConfig::default(),
            prompt_optimization: PromptOptimizationConfig::default(),
            codex_app_path: String::new(),
            user_scripts: Vec::new(),
            selected_models_by_provider: BTreeMap::new(),
            model_context_by_provider: BTreeMap::new(),
            model_reasoning_efforts_by_provider: BTreeMap::new(),
            manual_third_party_models_by_provider: BTreeMap::new(),
            declared_official_models_by_provider: BTreeMap::new(),
            upstream_models_by_provider: BTreeMap::new(),
            model_alias_history: BTreeMap::new(),
            default_model: String::new(),
            disable_trace_log_writes: true,
            protect_crashpad_pending: true,
            slim_codex_pet: true,
            gpu_launch_mode: GpuLaunchMode::Off,
            codex_launch_proxy: String::new(),
            fast_context_tools: false,
            subagent_optimization: false,
            subagent_model: default_subagent_model(),
            subagent_reasoning_effort: default_subagent_reasoning_effort(),
            subagent_roles: default_subagent_roles(),
            misc_model: String::new(),
            initial_route_import_completed: false,
            hide_full_access_warning: false,
            show_account_usage_in_header: true,
            official_account_available_this_launch: false,
            official_account_status_this_launch: LaunchOfficialAccountStatus::Unauthenticated,
            update_manifest_url: default_update_manifest_url(),
            release_admin_url: default_release_admin_url(),
        }
    }
}

pub(super) fn default_stream_max_retries() -> u32 {
    5
}

pub(super) fn default_true() -> bool {
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

pub(super) fn default_upstream_protocol() -> String {
    UPSTREAM_PROTOCOL_OPENAI_RESPONSES.to_string()
}

pub(super) fn default_auth_mode() -> String {
    AUTH_MODE_API_KEY.to_string()
}

pub(super) fn default_subagent_model() -> String {
    DEFAULT_SUBAGENT_MODEL.to_string()
}

pub(super) fn default_subagent_reasoning_effort() -> String {
    DEFAULT_SUBAGENT_REASONING_EFFORT.to_string()
}

pub(super) const DEFAULT_UPDATE_BASE_URL: &str =
    "https://pub-2d17a6a8bc22426a92e297a59f55ccc3.r2.dev";
const DEFAULT_RELEASE_ADMIN_URL: &str = "https://codey-release-admin.kimzane9991.workers.dev";

pub(super) fn update_manifest_url_from_base(configured_base_url: Option<&str>) -> String {
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

pub fn default_release_admin_url() -> String {
    option_env!("CODEY_RELEASE_ADMIN_URL")
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .unwrap_or(DEFAULT_RELEASE_ADMIN_URL)
        .trim_end_matches('/')
        .to_string()
}
