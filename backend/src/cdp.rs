use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use codey_runtime_core::bridge::{
    BridgeHandler, BridgePumpHandle, bridge_health_check_script, install_bridge,
};
use codey_runtime_core::cdp::{CdpTarget, list_targets, pick_injectable_codex_page_target};
use serde::{Deserialize, Serialize};

use crate::error_log;

const SETTINGS_OVERLAY_LOAD_PATH: &str = "/internal/codey/settings-overlay/load";
const SESSION_TOOLS_LOAD_PATH: &str = "/internal/codey/session-tools/load";
const CDP_INJECTION_TIMEOUT: Duration = Duration::from_secs(30);
const INJECTION_STATUS_READ_TIMEOUT: Duration = Duration::from_secs(1);
const INJECTION_DEADLINE_MARGIN: Duration = Duration::from_millis(100);
const CODEY_BRIDGE_SCRIPT: &str = include_str!("../../dist-overlay/inject/codey-bridge.js");
const GIT_REQUEST_GUARD_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/git-request-guard.js");
const WINDOWS_WMI_SAMPLER_GUARD_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/windows-wmi-sampler-guard.js");
const MODEL_WHITELIST_INJECT_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/model-whitelist-inject.js");
const RENDERER_INJECT_SCRIPT: &str = concat!(
    include_str!("../../dist-overlay/inject/default-chinese-locale.js"),
    "\n",
    include_str!("../../dist-overlay/inject/renderer-inject.js")
);
const CODEY_SESSION_TOOLS_SCRIPT: &str = include_str!("../../dist-overlay/inject/codey-inject.js");
const PET_CONTROL_SHIELD_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/pet-control-shield.js");
const SECURITY_WARNING_SHIELD_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/security-warning-shield.js");
const SETTINGS_OVERLAY_SCRIPT: &str = include_str!("../../dist-overlay/codey-overlay.js");
const PLUGIN_MARKETPLACE_FIX_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/plugin-marketplace-fix.js");
const PROMPT_OPTIMIZE_SCRIPT: &str = include_str!("../../dist-overlay/inject/prompt-optimize.js");
const MAX_INJECTION_ERROR_CHARS: usize = 500;
static SETTINGS_OVERLAY_LOAD_SCRIPT: OnceLock<Arc<str>> = OnceLock::new();
static SESSION_TOOLS_LOAD_SCRIPT: OnceLock<Arc<str>> = OnceLock::new();

#[derive(Clone, Copy)]
#[repr(u8)]
enum InjectionPhase {
    DiscoverTargets,
    SelectTarget,
    InstallBridge,
    VerifyOverlay,
    ReadStatuses,
}

impl InjectionPhase {
    fn from_raw(value: u8) -> Self {
        match value {
            value if value == Self::SelectTarget as u8 => Self::SelectTarget,
            value if value == Self::InstallBridge as u8 => Self::InstallBridge,
            value if value == Self::VerifyOverlay as u8 => Self::VerifyOverlay,
            value if value == Self::ReadStatuses as u8 => Self::ReadStatuses,
            _ => Self::DiscoverTargets,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::DiscoverTargets => "枚举 CDP 页面",
            Self::SelectTarget => "选择 Codex renderer",
            Self::InstallBridge => "安装 CDP bridge 与注入脚本",
            Self::VerifyOverlay => "验证 Codey 浮层",
            Self::ReadStatuses => "读取注入状态",
        }
    }
}

#[derive(Clone)]
struct InjectionScriptDescriptor {
    id: String,
    name: String,
    source: &'static str,
    visibility: InjectionScriptVisibility,
    probe: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InjectionScriptVisibility {
    Feature,
    Internal,
}

impl InjectionScriptVisibility {
    fn as_str(self) -> &'static str {
        match self {
            Self::Feature => "feature",
            Self::Internal => "internal",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InjectionHostPlatform {
    Windows,
    Other,
}

impl InjectionHostPlatform {
    fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

#[derive(Clone, Copy)]
enum InjectionScriptApplicability {
    All,
    WindowsOnly,
}

impl InjectionScriptApplicability {
    fn supports(self, platform: InjectionHostPlatform) -> bool {
        match self {
            Self::All => true,
            Self::WindowsOnly => platform == InjectionHostPlatform::Windows,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InjectionScriptStatus {
    pub id: String,
    pub name: String,
    pub source: String,
    pub visibility: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct PreparedInjectionScripts {
    scripts: Arc<[String]>,
    descriptors: Arc<[InjectionScriptDescriptor]>,
}

pub struct InjectedTarget {
    websocket_url: Arc<str>,
    pump: BridgePumpHandle,
    injection_statuses: Arc<[InjectionScriptStatus]>,
}

#[derive(Debug)]
pub struct InjectionRetryFailure {
    error: anyhow::Error,
}

impl InjectionRetryFailure {
    pub fn into_error(self) -> anyhow::Error {
        self.error
    }
}

impl std::fmt::Display for InjectionRetryFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for InjectionRetryFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

impl InjectedTarget {
    pub fn websocket_url(&self) -> &str {
        &self.websocket_url
    }

    pub fn injection_statuses(&self) -> Arc<[InjectionScriptStatus]> {
        self.injection_statuses.clone()
    }

    pub fn websocket_url_arc(&self) -> Arc<str> {
        self.websocket_url.clone()
    }

    pub async fn close(self) {
        self.pump.close().await;
    }
}

pub fn prepare_injection_scripts(
    local_router_enabled: bool,
    slim_codex_pet: bool,
    hide_full_access_warning: bool,
    user_scripts: &[String],
) -> PreparedInjectionScripts {
    prepare_injection_scripts_for_platform(
        local_router_enabled,
        slim_codex_pet,
        hide_full_access_warning,
        user_scripts,
        InjectionHostPlatform::current(),
    )
}

fn prepare_injection_scripts_for_platform(
    local_router_enabled: bool,
    slim_codex_pet: bool,
    hide_full_access_warning: bool,
    user_scripts: &[String],
    platform: InjectionHostPlatform,
) -> PreparedInjectionScripts {
    use InjectionScriptApplicability::{All, WindowsOnly};
    use InjectionScriptVisibility::{Feature, Internal};

    let builtin_scripts = [
        (
            "bridge-helpers",
            "桥接辅助",
            CODEY_BRIDGE_SCRIPT,
            r#"typeof window.__codexSessionDeleteBridge === "function"
              && typeof window.__codeyCall === "function"
              ? "桥接函数可调用" : """#
                .to_string(),
            Internal,
            All,
        ),
        (
            "git-request-guard",
            "Windows Git 请求保护",
            GIT_REQUEST_GUARD_SCRIPT,
            r#"(() => {
              const guard = window.__codeyGitRequestGuard;
              if (!guard || typeof guard.snapshot !== "function") return "";
              guard.ensureInstalled?.();
              const snapshot = guard.snapshot();
              if (snapshot.enabled === false && snapshot.installed === true) {
                return "Git 请求保护已就绪，当前平台无需启用";
              }
              if (snapshot.enabled === true && snapshot.mainProcessProtected === true) {
                return `Windows Git 请求限流已由主进程接管（持续速率 ${Math.round(60000 / snapshot.mainProcessSnapshot.tokenRefillMs)} 次/分钟）`;
              }
              if (snapshot.enabled === true && snapshot.bridgePatched === true) {
                return `Windows Git 请求限流已由 Renderer 接管（持续速率 ${Math.round(60000 / snapshot.tokenRefillMs)} 次/分钟）`;
              }
              const bridge = window.electronBridge;
              const workerMethod = typeof bridge?.sendWorkerMessageFromView;
              const statusMethod = typeof bridge?.sendMessageFromView;
              const reason = snapshot.mainProcessProbeError || "等待主进程保护注册";
              return {
                effective: false,
                detail: `Git 保护待确认：${reason}（workerBridge=${workerMethod}，statusBridge=${statusMethod}）`,
              };
            })()"#
                .to_string(),
            Feature,
            WindowsOnly,
        ),
        (
            "windows-wmi-sampler",
            "Windows WMI 周期采样保护",
            WINDOWS_WMI_SAMPLER_GUARD_SCRIPT,
            r#"(() => {
              const guard = window.__codeyWindowsWmiSamplerGuard;
              if (!guard || typeof guard.snapshot !== "function") return "";
              guard.requestProbe?.();
              const snapshot = guard.snapshot();
              if (snapshot.enabled === false && snapshot.installed === true) {
                return "WMI 周期采样保护已就绪，当前平台无需启用";
              }
              if (snapshot.blocked > 0) {
                const matchReason = snapshot.mainProcessSnapshot?.lastMatchReason;
                const matchDetail = matchReason === "source-signature"
                  ? "（通过 Worker 源码特征识别）"
                  : matchReason === "worker-option-name"
                    ? "（通过 Worker 语义名称识别）"
                    : "";
                return `已阻止 ${snapshot.blocked} 次 WMI 周期进程采样${matchDetail}`;
              }
              if (snapshot.mainProcessSnapshot?.selfTestError) {
                return {
                  effective: false,
                  detail: `WMI 周期采样保护自检失败：${snapshot.mainProcessSnapshot.selfTestError}`,
                };
              }
              if (snapshot.installed === true && snapshot.selfTestConfirmed === true) {
                const workersObserved =
                  Number(snapshot.mainProcessSnapshot?.workersObserved) || 0;
                let detail = "WMI Worker 拦截器已安装且完整自检通过";
                if (snapshot.sourceReadFailures > 0) {
                  detail += `；有 ${snapshot.sourceReadFailures} 个 Worker 源码无法检查，尚未观察到实际 WMI 采样`;
                } else if (snapshot.sourceInspections > 0) {
                  detail += `；已检查 ${snapshot.sourceInspections} 个 Worker，尚未观察到实际 WMI 采样`;
                } else if (workersObserved > 0) {
                  detail += `；已观察 ${workersObserved} 个 Worker，尚未触发实际 WMI 采样`;
                } else {
                  detail += "；尚未触发实际 WMI 采样";
                }
                return detail;
              }
              if (snapshot.sourceReadFailures > 0) {
                return {
                  effective: false,
                  detail: `有 ${snapshot.sourceReadFailures} 个 Worker 源码无法检查，WMI 周期采样保护尚不能确认`,
                };
              }
              if (snapshot.installed === true) {
                const observationComplete =
                  snapshot.observationMs >= snapshot.observationWindowMs;
                const detail = observationComplete
                  ? snapshot.sourceInspections > 0
                    ? `已检查 ${snapshot.sourceInspections} 个 Worker，尚未命中完整 WMI 周期采样特征；若 WMI 仍高占用，当前来源尚未被识别`
                    : "WMI 周期采样保护已安装，但观察窗内未匹配到可识别的目标 Worker"
                  : snapshot.selfTestPassed === true
                    ? "旧版 WMI Worker 拦截器自检通过，等待实际目标采样确认"
                    : `WMI 周期采样保护已安装，等待首次采样确认（已观察 ${Math.floor(snapshot.observationMs / 1000)} 秒）`;
                return {
                  effective: false,
                  detail,
                };
              }
              return {
                effective: false,
                detail: `WMI 周期采样保护待确认：${snapshot.probeError || "等待主进程保护注册"}`,
              };
            })()"#
                .to_string(),
            Feature,
            WindowsOnly,
        ),
        (
            "model-whitelist",
            "模型白名单",
            MODEL_WHITELIST_INJECT_SCRIPT,
            r#"(() => {
              const patch = window.__codeyModelWhitelistPatch;
              if (!patch || typeof patch.snapshot !== "function") return "";
              const snapshot = patch.snapshot();
              return snapshot?.loaded === true
                ? `模型目录已加载（${Array.isArray(snapshot.models) ? snapshot.models.length : 0} 个模型）`
                : "";
            })()"#
                .to_string(),
            Internal,
            All,
        ),
        (
            "pet-control-shield",
            "宠物控制精简",
            PET_CONTROL_SHIELD_SCRIPT,
            format!(
                r#"window.__codeyPetControlShield?.enabled === {slim_codex_pet}
                  && typeof window.__codeyPetControlShield?.block === "function"
                  ? {} : """#,
                if slim_codex_pet {
                    serde_json::to_string("宠物控制精简已启用")
                        .expect("pet probe detail should serialize")
                } else {
                    format!(
                        "{{ effective: false, inactive: true, detail: {} }}",
                        serde_json::to_string("控制器已就绪，当前精简策略关闭")
                            .expect("pet inactive detail should serialize")
                    )
                }
            ),
            Feature,
            All,
        ),
        (
            "security-warning-shield",
            "安全提示控制",
            SECURITY_WARNING_SHIELD_SCRIPT,
            format!(
                r#"window.__codeySecurityWarningShieldInstalled === true
                  && window.__codeySecurityWarningShield?.enabled === {hide_full_access_warning}
                  && typeof window.__codeySecurityWarningShield?.dismissWarnings === "function"
                  ? {} : """#,
                if hide_full_access_warning {
                    serde_json::to_string("安全提示屏蔽已启用")
                        .expect("security probe detail should serialize")
                } else {
                    format!(
                        "{{ effective: false, inactive: true, detail: {} }}",
                        serde_json::to_string("控制器已就绪，当前屏蔽策略关闭")
                            .expect("security inactive detail should serialize")
                    )
                }
            ),
            Feature,
            All,
        ),
        (
            "settings-overlay-loader",
            "配置面板加载器",
            lazy_settings_overlay_loader_script(),
            r#"typeof window.__codeySettingsOverlay?.toggle === "function"
              ? (window.__codeySettingsOverlay.__codeyLazyLoader
                ? "配置面板按需加载器可用" : "配置面板已加载")
              : """#
                .to_string(),
            Internal,
            All,
        ),
        (
            "renderer-controls",
            "渲染器控制",
            RENDERER_INJECT_SCRIPT,
            r#"(() => {
              if (window.__codeyRendererCoreLoaded !== true
                || typeof window.__codeyRendererScan !== "function"
                || typeof window.__codeyLoadSessionTools !== "function") return "";
              const locale = window.__codeyDefaultChineseLocale?.snapshot?.();
              return locale?.locale === "zh-CN"
                ? `渲染器控制、默认中文与按需加载 API 可用（Statsig client ${locale.statsigClientsPatched} 个）`
                : "渲染器控制与按需加载 API 可用";
            })()"#
                .to_string(),
            Internal,
            All,
        ),
        (
            "plugin-marketplace-compatibility",
            "插件市场兼容",
            PLUGIN_MARKETPLACE_FIX_SCRIPT,
            r#"window.__codeyPluginMarketplaceFixInstalled === true
              && typeof window.__codeyEnsurePluginBridge === "function"
              && window.electronBridge?.sendMessageFromView?.__codeyPatched === true
              ? "插件市场桥接已接管" : """#
                .to_string(),
            Internal,
            All,
        ),
        (
            "prompt-optimize",
            "提示词优化",
            PROMPT_OPTIMIZE_SCRIPT,
            r#"(() => {
              const optimizer = window.__codeyPromptOptimize;
              if (!optimizer || typeof optimizer.snapshot !== "function") return "";
              const snapshot = optimizer.snapshot();
              if (snapshot.ready !== true) return "";
              return snapshot.enabled === true
                ? "提示词优化按钮已就绪"
                : { effective: false, inactive: true, detail: "提示词优化已关闭" };
            })()"#
                .to_string(),
            Feature,
            All,
        ),
    ];
    let mut core_bundle = String::with_capacity(
        CODEY_BRIDGE_SCRIPT.len()
            + GIT_REQUEST_GUARD_SCRIPT.len()
            + WINDOWS_WMI_SAMPLER_GUARD_SCRIPT.len()
            + MODEL_WHITELIST_INJECT_SCRIPT.len()
            + RENDERER_INJECT_SCRIPT.len()
            + PET_CONTROL_SHIELD_SCRIPT.len()
            + SECURITY_WARNING_SHIELD_SCRIPT.len()
            + PLUGIN_MARKETPLACE_FIX_SCRIPT.len()
            + PROMPT_OPTIMIZE_SCRIPT.len()
            + 4096,
    );
    let mut descriptors = Vec::with_capacity(builtin_scripts.len() + user_scripts.len());
    for (id, name, script, probe, visibility, applicability) in builtin_scripts {
        if !applicability.supports(platform) {
            continue;
        }
        let descriptor = InjectionScriptDescriptor {
            id: id.to_string(),
            name: name.to_string(),
            source: "builtin",
            visibility,
            probe: Some(probe),
        };
        let prepared = if id == "model-whitelist" {
            Cow::Owned(format!(
                "window.__codeyNativeModelSelectionOnly = {};\n{script}",
                !local_router_enabled,
            ))
        } else {
            prepare_script(script, slim_codex_pet)
        };
        append_guarded_script(&mut core_bundle, &descriptor, prepared.as_ref());
        descriptors.push(descriptor);
    }

    let mut scripts = Vec::with_capacity(1 + user_scripts.len());
    scripts.push(core_bundle);
    for (index, script) in user_scripts
        .iter()
        .filter(|script| !script.trim().is_empty())
        .enumerate()
    {
        let descriptor = InjectionScriptDescriptor {
            id: format!("user-script-{}", index + 1),
            name: format!("用户脚本 {}", index + 1),
            source: "user",
            visibility: Feature,
            probe: None,
        };
        let mut guarded = String::with_capacity(script.len() + 512);
        append_guarded_script(&mut guarded, &descriptor, script);
        scripts.push(guarded);
        descriptors.push(descriptor);
    }
    PreparedInjectionScripts {
        scripts: Arc::from(scripts),
        descriptors: Arc::from(descriptors),
    }
}

fn prepare_script(script: &str, slim_codex_pet: bool) -> Cow<'_, str> {
    if !script.contains("__CODEY_SLIM_PET__") {
        return Cow::Borrowed(script);
    }
    Cow::Owned(script.replace(
        "__CODEY_SLIM_PET__",
        if slim_codex_pet { "true" } else { "false" },
    ))
}

fn append_guarded_script(
    bundle: &mut String,
    descriptor: &InjectionScriptDescriptor,
    script: &str,
) {
    let id = serde_json::to_string(&descriptor.id).expect("script id should serialize");
    let name = serde_json::to_string(&descriptor.name).expect("script name should serialize");
    let source = serde_json::to_string(descriptor.source).expect("script source should serialize");
    bundle.push_str("\n(window.__codeyInjectionStatus ||= Object.create(null))[");
    bundle.push_str(&id);
    bundle.push_str("] = { id: ");
    bundle.push_str(&id);
    bundle.push_str(", name: ");
    bundle.push_str(&name);
    bundle.push_str(", source: ");
    bundle.push_str(&source);
    bundle.push_str(", status: \"pending\", detail: null, error: null };\n");
    bundle.push_str("try {\n");
    bundle.push_str(script);
    bundle.push_str("\n  const completedEntry = window.__codeyInjectionStatus[");
    bundle.push_str(&id);
    bundle.push_str("];\n");
    bundle.push_str(
        "  if (completedEntry.status === \"pending\") completedEntry.status = \"executed\";\n",
    );
    bundle.push_str("} catch (error) {\n");
    bundle.push_str(
        "  const message = error instanceof Error\n    ? `${error.name}: ${error.message}${error.stack ? `\\n${error.stack}` : \"\"}`\n    : String(error || \"未知错误\");\n",
    );
    bundle.push_str("  const registry = window.__codeyInjectionStatus ||= Object.create(null);\n");
    bundle.push_str("  const entry = registry[");
    bundle.push_str(&id);
    bundle.push_str("] ||= { id: ");
    bundle.push_str(&id);
    bundle.push_str(", name: ");
    bundle.push_str(&name);
    bundle.push_str(", source: ");
    bundle.push_str(&source);
    bundle.push_str(" };\n");
    bundle.push_str("  entry.status = \"failed\";\n");
    bundle.push_str("  entry.error = message.slice(0, ");
    bundle.push_str(&MAX_INJECTION_ERROR_CHARS.to_string());
    bundle.push_str(");\n  console.error(\"[Codey] ");
    bundle.push_str(&descriptor.name);
    bundle.push_str(" injection failed\", error);\n}\n");
}

pub async fn retry_inject_with_scripts(
    debug_port: u16,
    handler: BridgeHandler,
    scripts: &PreparedInjectionScripts,
) -> std::result::Result<InjectedTarget, InjectionRetryFailure> {
    // Renderer asset preparation on newer Windows Codex builds can consume
    // more than ten seconds before the first injectable page appears. Keep
    // enough budget for the bridge commands after discovery while retaining a
    // hard startup deadline.
    let deadline = tokio::time::Instant::now() + CDP_INJECTION_TIMEOUT;
    let mut delay = Duration::from_millis(100);
    let phase = Arc::new(AtomicU8::new(InjectionPhase::DiscoverTargets as u8));
    let mut previous_error = None;
    let last_error = loop {
        phase.store(InjectionPhase::DiscoverTargets as u8, Ordering::Release);
        match tokio::time::timeout_at(
            deadline,
            inject_with_scripts(debug_port, handler.clone(), scripts, &phase, deadline),
        )
        .await
        {
            Ok(Ok(target)) => return Ok(target),
            Ok(Err(error)) => {
                let current_phase = InjectionPhase::from_raw(phase.load(Ordering::Acquire));
                if tokio::time::Instant::now() + delay > deadline {
                    break anyhow::anyhow!(
                        "Codex CDP bridge 注入失败（阶段：{}；{}）",
                        current_phase.label(),
                        safe_injection_error_summary(&error)
                    );
                }
                previous_error = Some(safe_injection_error_summary(&error));
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(2));
            }
            Err(_) => {
                let current_phase = InjectionPhase::from_raw(phase.load(Ordering::Acquire));
                let previous_error = previous_error
                    .as_deref()
                    .map(|error| format!("；最近一次失败：{error}"))
                    .unwrap_or_default();
                break anyhow::anyhow!(
                    "等待 Codex CDP bridge 注入超时（{} ms，阶段：{}{}）",
                    CDP_INJECTION_TIMEOUT.as_millis(),
                    current_phase.label(),
                    previous_error
                );
            }
        }
    };
    Err(InjectionRetryFailure { error: last_error })
}

fn summarize_cdp_targets(targets: &[CdpTarget]) -> String {
    if targets.is_empty() {
        return "[]".to_string();
    }
    let visible = targets
        .iter()
        .take(6)
        .map(|target| {
            format!(
                "{{type:{},url:{:?},ws:{}}}",
                truncate_chars(target.target_type.clone(), 20),
                safe_target_url_shape(&target.url),
                target.web_socket_debugger_url.is_some()
            )
        })
        .collect::<Vec<_>>();
    let omitted = targets.len().saturating_sub(visible.len());
    if omitted == 0 {
        format!("[{}]", visible.join(","))
    } else {
        format!("[{},+{omitted}]", visible.join(","))
    }
}

fn safe_target_url_shape(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.to_ascii_lowercase().starts_with("app://") {
        let end = trimmed
            .char_indices()
            .find_map(|(index, character)| matches!(character, '?' | '#').then_some(index))
            .unwrap_or(trimmed.len());
        return truncate_chars(trimmed[..end].to_string(), 100);
    }
    trimmed
        .split_once(':')
        .filter(|(scheme, _)| {
            !scheme.is_empty()
                && scheme.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
                })
        })
        .map(|(scheme, _)| format!("{}:", scheme.to_ascii_lowercase()))
        .unwrap_or_else(|| "<unknown>".to_string())
}

fn safe_injection_error_summary(error: &anyhow::Error) -> String {
    let message = format!("{error:#}");
    if let Some((_, targets)) = message.split_once("CDP targets=") {
        let targets = targets
            .split_once(']')
            .map(|(targets, _)| format!("{targets}]"))
            .unwrap_or_else(|| "[]".to_string());
        return truncate_chars(
            format!("未发现匹配的 Codex renderer；CDP targets={targets}"),
            MAX_INJECTION_ERROR_CHARS,
        );
    }
    let normalized = message.to_ascii_lowercase();
    if normalized.contains("failed to query cdp targets")
        || normalized.contains("枚举 codex cdp 页面失败")
    {
        "CDP 页面列表尚不可用".to_string()
    } else if normalized.contains("timed out connecting cdp websocket")
        || normalized.contains("failed to connect")
    {
        "CDP WebSocket 连接失败".to_string()
    } else if normalized.contains("timed out waiting for cdp command") {
        "CDP 命令未在单次响应预算内完成".to_string()
    } else if normalized.contains("codey 内嵌配置面板注入失败") {
        "Codey 浮层未就绪".to_string()
    } else {
        "内部注入尝试失败".to_string()
    }
}

fn injection_status_read_budget(remaining: Duration) -> Option<Duration> {
    let budget = remaining
        .saturating_sub(INJECTION_DEADLINE_MARGIN)
        .min(INJECTION_STATUS_READ_TIMEOUT);
    (!budget.is_zero()).then_some(budget)
}

async fn inject_with_scripts(
    debug_port: u16,
    handler: BridgeHandler,
    scripts: &PreparedInjectionScripts,
    phase: &AtomicU8,
    deadline: tokio::time::Instant,
) -> Result<InjectedTarget> {
    phase.store(InjectionPhase::DiscoverTargets as u8, Ordering::Release);
    let targets = list_targets(debug_port)
        .await
        .context("枚举 Codex CDP 页面失败")?;
    phase.store(InjectionPhase::SelectTarget as u8, Ordering::Release);
    let target = pick_injectable_codex_page_target(&targets).with_context(|| {
        format!(
            "没有找到可注入的 Codex renderer；CDP targets={}",
            summarize_cdp_targets(&targets)
        )
    })?;
    let websocket_url: Arc<str> = Arc::from(
        target
            .web_socket_debugger_url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Codex 页面没有 CDP WebSocket 地址"))?,
    );
    let handler = with_lazy_loaders(handler, websocket_url.clone());
    phase.store(InjectionPhase::InstallBridge as u8, Ordering::Release);
    let pump = install_bridge(
        &websocket_url,
        codey_runtime_core::bridge::BRIDGE_BINDING_NAME,
        handler,
        &scripts.scripts,
    )
    .await
    .with_context(|| format!("向 Codex renderer {} 安装 CDP bridge 失败", target.id))?;
    phase.store(InjectionPhase::VerifyOverlay as u8, Ordering::Release);
    ensure_settings_overlay_ready(&websocket_url)
        .await
        .with_context(|| format!("验证 Codex renderer {} 的 Codey 浮层失败", target.id))?;
    phase.store(InjectionPhase::ReadStatuses as u8, Ordering::Release);
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    let injection_statuses = match injection_status_read_budget(remaining) {
        Some(status_budget) => match tokio::time::timeout(
            status_budget,
            read_injection_statuses(&websocket_url, scripts),
        )
        .await
        {
            Ok(Ok(statuses)) => statuses,
            Ok(Err(_)) => scripts.statuses_with_error("读取注入状态失败，将在运行期复核"),
            Err(_) => scripts.statuses_with_error("读取注入状态超时，将在运行期复核"),
        },
        None => scripts.statuses_with_error("启动预算即将结束，注入状态将在运行期复核"),
    };
    Ok(InjectedTarget {
        websocket_url,
        pump,
        injection_statuses,
    })
}

impl PreparedInjectionScripts {
    pub fn statuses_with_error(&self, error: impl Into<String>) -> Arc<[InjectionScriptStatus]> {
        let error = truncate_chars(error.into(), MAX_INJECTION_ERROR_CHARS);
        Arc::from(
            self.descriptors
                .iter()
                .map(|descriptor| InjectionScriptStatus {
                    id: descriptor.id.clone(),
                    name: descriptor.name.clone(),
                    source: descriptor.source.to_string(),
                    visibility: descriptor.visibility.as_str().to_string(),
                    status: "unknown".to_string(),
                    detail: None,
                    error: Some(error.clone()),
                })
                .collect::<Vec<_>>(),
        )
    }
}

#[derive(Deserialize)]
struct RuntimeInjectionStatus {
    id: String,
    status: String,
    detail: Option<String>,
    error: Option<String>,
}

pub async fn read_injection_statuses(
    websocket_url: &str,
    scripts: &PreparedInjectionScripts,
) -> Result<Arc<[InjectionScriptStatus]>> {
    let result: Result<Arc<[InjectionScriptStatus]>> = async {
        let response = codey_runtime_core::bridge::evaluate_script_with_await_promise(
            websocket_url,
            &injection_status_snapshot_script(&scripts.descriptors),
            true,
        )
        .await
        .context("查询脚本注入状态失败")?;
        let payload = runtime_value(&response)
            .and_then(serde_json::Value::as_str)
            .context("脚本注入状态未返回可解析结果")?;
        let reported = serde_json::from_str::<Vec<RuntimeInjectionStatus>>(payload)
            .context("解析脚本注入状态失败")?;
        Ok(reconcile_injection_statuses(&scripts.descriptors, reported))
    }
    .await;

    match result {
        Ok(statuses) => {
            record_failed_injection_statuses(websocket_url, &statuses).await;
            Ok(statuses)
        }
        Err(error) => {
            error_log::record_failure_async(
                "injection_status_failed",
                "read_injection_statuses",
                format!("{error:#}"),
                serde_json::json!({
                    "websocketUrl": websocket_url,
                }),
            )
            .await;
            Err(error)
        }
    }
}

async fn record_failed_injection_statuses(websocket_url: &str, statuses: &[InjectionScriptStatus]) {
    for status in statuses
        .iter()
        .filter(|status| status.status == "failed" || status.error.is_some())
    {
        error_log::record_failure_async(
            "injection_script_failed",
            status.id.clone(),
            status
                .error
                .clone()
                .unwrap_or_else(|| "注入脚本报告执行失败".to_string()),
            serde_json::json!({
                "name": status.name.as_str(),
                "source": status.source.as_str(),
                "detail": status.detail.as_deref(),
                "websocketUrl": websocket_url,
            }),
        )
        .await;
    }
}

#[derive(Debug)]
pub struct ModelWhitelistRefresh {
    /// The catalog was accepted and the transport patch guarantees future
    /// `model/list` responses carry it, but no live model query existed yet
    /// to patch in place (cold renderer, picker not mounted).
    pub deferred: bool,
}

pub async fn refresh_model_whitelist(
    websocket_url: &str,
    expected_catalog: &serde_json::Value,
) -> Result<ModelWhitelistRefresh> {
    let response = codey_runtime_core::bridge::evaluate_script_with_await_promise(
        websocket_url,
        &model_whitelist_refresh_script(expected_catalog),
        true,
    )
    .await
    .context("请求 Codex 刷新模型列表失败")?;
    verify_model_whitelist_refresh_response(&response)
}

fn model_whitelist_refresh_script(expected_catalog: &serde_json::Value) -> String {
    let expected_catalog =
        serde_json::to_string(expected_catalog).expect("model catalog should serialize");
    format!(
        r#"(async () => {{
  const expectedCatalog = {expected_catalog};
  const expectedModels = Array.isArray(expectedCatalog.models)
    ? expectedCatalog.models
    : [];
  const expectedDefaultModel = typeof expectedCatalog.default_model === "string"
    ? expectedCatalog.default_model
    : typeof expectedCatalog.model === "string"
      ? expectedCatalog.model
      : "";
  const matchesExpected = (snapshot) => (
    snapshot?.loaded === true
    && Array.isArray(snapshot.models)
    && snapshot.models.length === expectedModels.length
    && snapshot.models.every((model, index) => model === expectedModels[index])
    && snapshot.defaultModel === expectedDefaultModel
  );
  // The response patch rewrites every future `model/list` bridge reply and
  // the scheduled/interaction passes keep patching the query cache, so a
  // renderer whose model picker has not mounted yet still receives the
  // catalog — just lazily. That counts as a deferred delivery, not a
  // failure; only a missing Statsig or response patch is a real failure.
  const catalogAccepted = (delivery) => (
    delivery?.responsePatchInstalled === true
    && Number(delivery.statsigClients) > 0
    && Number(delivery.notifiedClients) > 0
  );
  const reachedActiveModelPicker = (delivery) => (
    catalogAccepted(delivery)
    && Number(delivery.queryClients) > 0
    && Number(delivery.queryEntries) > 0
  );
  const deliverySummary = (delivery) => delivery
    ? `（statsigClients=${{Number(delivery.statsigClients)}}, notifiedClients=${{Number(delivery.notifiedClients)}}, queryClients=${{Number(delivery.queryClients)}}, queryEntries=${{Number(delivery.queryEntries)}}）`
    : "";
  let snapshot = null;
  let delivery = null;
  let lastError = "模型白名单补丁尚未就绪";
  for (const delay of [0, 80, 200, 500, 1000, 2000]) {{
    if (delay > 0) {{
      await new Promise((resolve) => window.setTimeout(resolve, delay));
    }}
    const patch = window.__codeyModelWhitelistPatch;
    if (
      !patch
      || typeof patch.setCatalog !== "function"
      || typeof patch.delivery !== "function"
      || typeof patch.snapshot !== "function"
    ) {{
      lastError = "模型白名单补丁尚未就绪";
      continue;
    }}
    try {{
      const updated = await patch.setCatalog(expectedCatalog);
      snapshot = patch.snapshot();
      delivery = patch.delivery();
      if (updated !== true) {{
        lastError = "模型白名单拒绝了后端推送的目录";
      }} else if (!matchesExpected(snapshot)) {{
        lastError = "模型白名单快照与已保存配置不一致";
      }} else if (reachedActiveModelPicker(delivery)) {{
        return JSON.stringify({{ ok: true, delivered: "active", snapshot, delivery }});
      }} else if (catalogAccepted(delivery)) {{
        // Catalog is on the transport patch; waiting for a mounted picker
        // would only upgrade deferred to active while holding evaluate open.
        return JSON.stringify({{ ok: true, delivered: "deferred", snapshot, delivery }});
      }} else if (delivery?.responsePatchInstalled !== true) {{
        lastError = "模型响应补丁未安装";
      }} else if (Number(delivery.statsigClients) < 1) {{
        lastError = "未找到 Codex 的 Statsig 客户端";
      }} else {{
        lastError = "未能通知 Codex 的 Statsig 客户端";
      }}
    }} catch (error) {{
      lastError = error instanceof Error ? error.message : String(error);
    }}
  }}
  return JSON.stringify({{ ok: false, error: `${{lastError}}${{deliverySummary(delivery)}}`, snapshot, delivery }});
}})()"#
    )
}

fn verify_model_whitelist_refresh_response(
    response: &serde_json::Value,
) -> Result<ModelWhitelistRefresh> {
    let payload = runtime_value(response)
        .and_then(serde_json::Value::as_str)
        .context("Codex 模型列表热更新未返回可解析结果")?;
    let report = serde_json::from_str::<serde_json::Value>(payload)
        .context("解析 Codex 模型列表热更新结果失败")?;
    if report.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(ModelWhitelistRefresh {
            deferred: report.get("delivered").and_then(serde_json::Value::as_str)
                == Some("deferred"),
        });
    }
    let error = report
        .get("error")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("模型白名单刷新结果未通过校验");
    anyhow::bail!("Codex 模型列表热更新失败：{error}")
}

fn injection_status_snapshot_script(descriptors: &[InjectionScriptDescriptor]) -> String {
    let mut probes = String::from("{\n");
    for descriptor in descriptors {
        let Some(probe) = descriptor.probe.as_deref() else {
            continue;
        };
        probes.push_str(&serde_json::to_string(&descriptor.id).expect("probe id should serialize"));
        probes.push_str(": () => (");
        probes.push_str(probe);
        probes.push_str("),\n");
    }
    probes.push('}');
    format!(
        r#"(async () => {{
  const registry = window.__codeyInjectionStatus || Object.create(null);
  const probes = {probes};
  const verify = () => {{
    for (const [id, probe] of Object.entries(probes)) {{
      const entry = registry[id];
      if (!entry || !["executed", "effective", "inactive"].includes(entry.status)) continue;
      try {{
        const evidence = probe();
        const structured = evidence && typeof evidence === "object"
          && Object.prototype.hasOwnProperty.call(evidence, "effective");
        const effective = structured ? evidence.effective === true : Boolean(evidence);
        const inactive = structured && evidence.inactive === true;
        const detail = structured ? evidence.detail : evidence;
        if (effective) {{
          entry.status = "effective";
        }} else if (inactive) {{
          entry.status = "inactive";
        }}
        if (detail) entry.detail = String(detail);
      }} catch (error) {{
        entry.status = "failed";
        entry.error = String(error instanceof Error
          ? `${{error.name}}: ${{error.message}}`
          : error || "生效自检失败").slice(0, {MAX_INJECTION_ERROR_CHARS});
      }}
    }}
  }};
  const hasPendingProbe = () => Object.keys(probes)
    .some((id) => registry[id]?.status === "executed");
  verify();
  for (const delay of [50, 200, 750]) {{
    if (!hasPendingProbe()) break;
    await new Promise((resolve) => setTimeout(resolve, delay));
    verify();
  }}
  return JSON.stringify(Object.values(registry));
}})()"#
    )
}

fn reconcile_injection_statuses(
    descriptors: &[InjectionScriptDescriptor],
    reported: Vec<RuntimeInjectionStatus>,
) -> Arc<[InjectionScriptStatus]> {
    let mut reported = reported
        .into_iter()
        .map(|status| (status.id.clone(), status))
        .collect::<HashMap<_, _>>();
    Arc::from(
        descriptors
            .iter()
            .map(|descriptor| {
                let Some(status) = reported.remove(&descriptor.id) else {
                    return InjectionScriptStatus {
                        id: descriptor.id.clone(),
                        name: descriptor.name.clone(),
                        source: descriptor.source.to_string(),
                        visibility: descriptor.visibility.as_str().to_string(),
                        status: "unknown".to_string(),
                        detail: None,
                        error: Some("脚本未返回注入状态".to_string()),
                    };
                };
                let RuntimeInjectionStatus {
                    id: _,
                    status: reported_status,
                    detail,
                    error,
                } = status;
                let valid_status = matches!(
                    reported_status.as_str(),
                    "effective" | "executed" | "inactive" | "failed"
                );
                let normalized_detail = if valid_status {
                    detail
                        .map(|detail| truncate_chars(detail, MAX_INJECTION_ERROR_CHARS))
                        .or_else(|| {
                            (reported_status == "executed").then(|| {
                                if descriptor.source == "user" {
                                    "脚本已执行，但未提供生效自检".to_string()
                                } else {
                                    "脚本已执行，但生效探针尚未通过".to_string()
                                }
                            })
                        })
                } else {
                    None
                };
                InjectionScriptStatus {
                    id: descriptor.id.clone(),
                    name: descriptor.name.clone(),
                    source: descriptor.source.to_string(),
                    visibility: descriptor.visibility.as_str().to_string(),
                    status: if valid_status {
                        reported_status
                    } else {
                        "unknown".to_string()
                    },
                    detail: normalized_detail,
                    error: if valid_status {
                        error.map(|error| truncate_chars(error, MAX_INJECTION_ERROR_CHARS))
                    } else {
                        Some("脚本返回了未知注入状态".to_string())
                    },
                }
            })
            .collect::<Vec<_>>(),
    )
}

fn truncate_chars(value: String, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn with_lazy_loaders(handler: BridgeHandler, websocket_url: Arc<str>) -> BridgeHandler {
    Arc::new(move |path, payload| {
        if path == SETTINGS_OVERLAY_LOAD_PATH {
            let websocket_url = websocket_url.clone();
            return Box::pin(async move {
                let settings_overlay_load_script = prepared_settings_overlay_load_script();
                let response = codey_runtime_core::bridge::evaluate_script(
                    &websocket_url,
                    &settings_overlay_load_script,
                )
                .await
                .context("按需加载 Codey 内嵌配置面板失败")?;
                let message = runtime_value(&response)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("配置面板加载脚本未返回状态");
                if !message.is_empty() {
                    anyhow::bail!("Codey 内嵌配置面板加载失败：{message}");
                }
                Ok(serde_json::json!({ "status": "ok" }))
            });
        }

        if path == SESSION_TOOLS_LOAD_PATH {
            let websocket_url = websocket_url.clone();
            return Box::pin(async move {
                let session_tools_load_script = prepared_session_tools_load_script();
                let response = codey_runtime_core::bridge::evaluate_script(
                    &websocket_url,
                    &session_tools_load_script,
                )
                .await
                .context("按需加载 Codey 会话工具失败")?;
                let message = runtime_value(&response)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("会话工具加载脚本未返回状态");
                if !message.is_empty() {
                    anyhow::bail!("Codey 会话工具加载失败：{message}");
                }
                Ok(serde_json::json!({ "status": "ok" }))
            });
        }

        handler(path, payload)
    })
}

fn prepared_settings_overlay_load_script() -> Arc<str> {
    SETTINGS_OVERLAY_LOAD_SCRIPT
        .get_or_init(|| Arc::from(settings_overlay_load_script(SETTINGS_OVERLAY_SCRIPT)))
        .clone()
}

fn prepared_session_tools_load_script() -> Arc<str> {
    SESSION_TOOLS_LOAD_SCRIPT
        .get_or_init(|| Arc::from(session_tools_load_script(CODEY_SESSION_TOOLS_SCRIPT)))
        .clone()
}

fn lazy_settings_overlay_loader_script() -> &'static str {
    r#"(() => {
  const loadPath = "/internal/codey/settings-overlay/load";
  const existing = window.__codeySettingsOverlay;
  if (existing && typeof existing.toggle === "function" && !existing.__codeyLazyLoader) {
    return;
  }
  if (existing?.__codeyLazyLoader) return;

  let loading = null;
  const formatError = (error) => error instanceof Error
    ? `${error.name}: ${error.message}`
    : String(error || "未知错误");
  const proxy = {
    __codeyLazyLoader: true,
    close() {},
    isOpen() { return false; },
    load() {
      if (loading) return loading;
      if (typeof window.__codexSessionDeleteBridge !== "function") {
        return Promise.reject(new Error("Codey bridge 尚未就绪"));
      }
      loading = Promise.resolve(
        window.__codexSessionDeleteBridge(loadPath, {}),
      ).then((result) => {
        if (!result || result.status !== "ok") {
          throw new Error(result?.message || "配置面板加载请求失败");
        }
        const overlay = window.__codeySettingsOverlay;
        if (!overlay || overlay === proxy || typeof overlay.toggle !== "function") {
          throw new Error(window.__codeyOverlayError || "未生成浮层控制器");
        }
        return overlay;
      });
      return loading;
    },
    open() {
      this.toggle();
    },
    toggle() {
      if (loading) return;
      void this.load().then((overlay) => {
        if (typeof overlay.open === "function") overlay.open();
        else overlay.toggle();
      }).catch((error) => {
        const message = formatError(error);
        window.__codeyOverlayError = message;
        loading = null;
        window.alert(`Codey 内嵌配置面板加载失败：${message}`);
      });
    },
  };
  window.__codeySettingsOverlay = proxy;
})()"#
}

fn settings_overlay_load_script(script: &str) -> String {
    let wrapped = wrap_settings_overlay(script);
    format!(
        r#"(() => {{
  const current = window.__codeySettingsOverlay;
  if (current && typeof current.toggle === "function" && !current.__codeyLazyLoader) {{
    return "";
  }}
  if (current?.__codeyLazyLoader) delete window.__codeySettingsOverlay;
  {wrapped}
  const ready = typeof window.__codeySettingsOverlay === "object"
    && typeof window.__codeySettingsOverlay.toggle === "function"
    && !window.__codeySettingsOverlay.__codeyLazyLoader;
  if (ready) return "";
  if (current?.__codeyLazyLoader) window.__codeySettingsOverlay = current;
  return String(window.__codeyOverlayError || "未生成浮层控制器");
}})()"#
    )
}

fn wrap_settings_overlay(script: &str) -> String {
    let mut wrapped = String::from(
        r#"(() => {
  window.__codeyOverlayError = "";
  try {
"#,
    );
    wrapped.push_str(script);
    wrapped.push_str(
        r#"
  } catch (error) {
    const message = error instanceof Error
      ? `${error.name}: ${error.message}${error.stack ? `\n${error.stack}` : ""}`
      : String(error);
    window.__codeyOverlayError = message;
    console.error("[Codey] settings overlay failed to load", error);
  }
})();
"#,
    );
    wrapped
}

fn session_tools_load_script(script: &str) -> String {
    format!(
        r#"(() => {{
  if (window.__codeySessionToolsInjectLoaded === true) return "";
  window.__codeySessionToolsError = "";
  try {{
{script}
  }} catch (error) {{
    const message = error instanceof Error
      ? `${{error.name}}: ${{error.message}}${{error.stack ? `\n${{error.stack}}` : ""}}`
      : String(error);
    window.__codeySessionToolsError = message;
    window.__codeySessionToolsInjectLoading = false;
    window.__codeySessionToolsInjectLoaded = false;
    console.error("[Codey] session tools failed to load", error);
  }}
  return window.__codeySessionToolsInjectLoaded === true
    ? ""
    : String(window.__codeySessionToolsError || "未生成会话工具控制器");
}})()"#
    )
}

async fn ensure_settings_overlay_ready(websocket_url: &str) -> Result<()> {
    let ready = codey_runtime_core::bridge::evaluate_script(
        websocket_url,
        r#"typeof window.__codeySettingsOverlay === "object"
          && typeof window.__codeySettingsOverlay.toggle === "function""#,
    )
    .await
    .context("检查 Codey 内嵌配置面板状态失败")?;
    if runtime_value(&ready).and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(());
    }

    let error = codey_runtime_core::bridge::evaluate_script(
        websocket_url,
        r#"String(window.__codeyOverlayError || "未生成浮层控制器")"#,
    )
    .await
    .context("读取 Codey 内嵌配置面板异常失败")?;
    let message = runtime_value(&error)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("未知错误");
    anyhow::bail!("Codey 内嵌配置面板注入失败：{message}")
}

fn runtime_value(response: &serde_json::Value) -> Option<&serde_json::Value> {
    response
        .get("result")
        .and_then(|value| value.get("result"))
        .and_then(|value| value.get("value"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetHealth {
    Healthy,
    Unhealthy,
    Busy,
}

fn target_health_from_evaluate_response(response: &serde_json::Value) -> TargetHealth {
    match runtime_value(response).and_then(serde_json::Value::as_str) {
        Some("healthy") => TargetHealth::Healthy,
        Some("busy") => TargetHealth::Busy,
        _ => TargetHealth::Unhealthy,
    }
}

pub async fn is_target_healthy(websocket_url: &str) -> Result<TargetHealth> {
    let result = codey_runtime_core::bridge::evaluate_script_with_await_promise(
        websocket_url,
        bridge_health_check_script(),
        true,
    )
    .await
    .context("检查 Codey bridge 健康状态失败")?;
    Ok(target_health_from_evaluate_response(&result))
}

pub fn target_health_error_requires_rediscovery(error: &anyhow::Error) -> bool {
    // Renderer command deadlines are deliberately inconclusive: injecting more
    // work into a busy page can make it less responsive. A Tungstenite error,
    // however, means the saved page endpoint could not carry the probe at all
    // (including HTTP upgrade failures from a replaced CDP target), so the
    // watchdog must rediscover `/json` instead of retrying the stale URL.
    error
        .downcast_ref::<tokio_tungstenite::tungstenite::Error>()
        .is_some()
}

pub fn bridge_handler<F, Fut>(handler: F) -> BridgeHandler
where
    F: Fn(String, serde_json::Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = serde_json::Value> + Send + 'static,
{
    Arc::new(move |path, payload| {
        let future = handler(path, payload);
        Box::pin(async move { Ok(future.await) })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injection_deadline_leaves_time_for_slow_windows_renderer_startup() {
        assert_eq!(CDP_INJECTION_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn nonessential_status_read_never_consumes_the_injection_deadline() {
        assert_eq!(
            injection_status_read_budget(Duration::from_secs(5)),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            injection_status_read_budget(Duration::from_millis(150)),
            Some(Duration::from_millis(50))
        );
        assert_eq!(
            injection_status_read_budget(Duration::from_millis(100)),
            None
        );
    }

    #[test]
    fn cdp_target_summary_keeps_shape_but_redacts_query_and_fragment() {
        let targets = vec![CdpTarget {
            id: "page-1".to_string(),
            target_type: "page".to_string(),
            title: "private task title".to_string(),
            url: "app://-/index.html?token=secret#private".to_string(),
            web_socket_debugger_url: Some("ws://127.0.0.1/devtools/page/1".to_string()),
        }];

        let summary = summarize_cdp_targets(&targets);

        assert!(summary.contains("app://-/index.html"));
        assert!(summary.contains("ws:true"));
        assert!(!summary.contains("secret"));
        assert!(!summary.contains("private"));
        assert!(!summary.contains("task title"));
        assert!(!summary.contains("127.0.0.1"));
    }

    #[test]
    fn injection_error_summary_does_not_persist_renderer_or_script_secrets() {
        let target_error = anyhow::anyhow!(
            "没有找到 renderer；CDP targets=[{{type:page,url:\"app://-/index.html\",ws:true}}]: secret title"
        );
        let script_error = anyhow::anyhow!(
            "timed out waiting for CDP command Runtime.evaluate: token=secret stack=/private/path"
        );

        let target_summary = safe_injection_error_summary(&target_error);
        let script_summary = safe_injection_error_summary(&script_error);

        assert!(target_summary.contains("app://-/index.html"));
        assert!(!target_summary.contains("secret title"));
        assert_eq!(script_summary, "CDP 命令未在单次响应预算内完成");
        assert!(!script_summary.contains("secret"));
        assert!(!script_summary.contains("private"));
    }

    #[test]
    fn overlay_wrapper_records_runtime_errors() {
        let wrapped = wrap_settings_overlay("throw new Error('boom');");
        assert!(wrapped.contains("window.__codeyOverlayError = message"));
        assert!(wrapped.contains("throw new Error('boom');"));
    }

    #[test]
    fn extracts_runtime_evaluate_primitive_value() {
        let response = serde_json::json!({
            "result": { "result": { "type": "boolean", "value": true } }
        });
        assert_eq!(runtime_value(&response), Some(&serde_json::json!(true)));
    }

    #[test]
    fn target_health_parses_tri_state_probe_results() {
        let response = |value: serde_json::Value| {
            serde_json::json!({
                "result": { "result": { "type": "string", "value": value } }
            })
        };
        assert_eq!(
            target_health_from_evaluate_response(&response(serde_json::json!("healthy"))),
            TargetHealth::Healthy
        );
        assert_eq!(
            target_health_from_evaluate_response(&response(serde_json::json!("busy"))),
            TargetHealth::Busy
        );
        for value in ["missing", "unhealthy"] {
            assert_eq!(
                target_health_from_evaluate_response(&response(serde_json::json!(value))),
                TargetHealth::Unhealthy
            );
        }
        // Legacy boolean and missing values degrade to unhealthy, never busy,
        // so a genuinely absent bridge still triggers reinjection.
        assert_eq!(
            target_health_from_evaluate_response(&response(serde_json::json!(false))),
            TargetHealth::Unhealthy
        );
        assert_eq!(
            target_health_from_evaluate_response(&serde_json::json!({})),
            TargetHealth::Unhealthy
        );
    }

    #[test]
    fn websocket_upgrade_errors_require_target_rediscovery_but_timeouts_do_not() {
        let response = tokio_tungstenite::tungstenite::http::Response::builder()
            .status(tokio_tungstenite::tungstenite::http::StatusCode::INTERNAL_SERVER_ERROR)
            .body(Some(Vec::new()))
            .unwrap();
        let websocket_error =
            anyhow::Error::new(tokio_tungstenite::tungstenite::Error::Http(response))
                .context("failed to connect CDP websocket");
        assert!(target_health_error_requires_rediscovery(&websocket_error));

        let renderer_timeout = anyhow::anyhow!("timed out waiting for CDP command");
        assert!(!target_health_error_requires_rediscovery(&renderer_timeout));
    }

    #[test]
    fn model_whitelist_refresh_script_retries_and_verifies_the_expected_snapshot() {
        let script = model_whitelist_refresh_script(&serde_json::json!({
            "status": "ok",
            "model": "provider-\"quoted",
            "default_model": "provider-\"quoted",
            "models": ["gpt-5.6-sol", "provider-\"quoted"],
            "model_metadata": [{
                "model": "provider-\"quoted",
                "display_name": "Provider / provider-\"quoted",
                "provider_id": "provider",
                "source_model": "provider-\"quoted"
            }]
        }));

        assert!(script.contains("window.__codeyModelWhitelistPatch"));
        assert!(script.contains("await patch.setCatalog(expectedCatalog)"));
        assert!(script.contains("patch.delivery()"));
        assert!(script.contains("patch.snapshot()"));
        assert!(!script.contains("patch.refresh()"));
        assert!(!script.contains("/codex-model-catalog"));
        assert!(script.contains("[0, 80, 200, 500, 1000, 2000]"));
        assert!(script.contains("model_metadata"));
        assert!(script.contains(r#"provider-\"quoted"#));
        assert!(script.contains("snapshot.defaultModel === expectedDefaultModel"));
        assert!(script.contains("delivery.queryEntries"));
        assert!(script.contains("catalogAccepted"));
        assert!(script.contains("} else if (catalogAccepted(delivery)) {"));
        assert!(script.contains(
            "return JSON.stringify({ ok: true, delivered: \"deferred\", snapshot, delivery });"
        ));
        assert!(!script.contains("deferredDelivery"));
        assert!(!script.contains("Keep retrying"));
    }

    #[test]
    fn model_whitelist_refresh_response_requires_a_verified_result() {
        let success = serde_json::json!({
            "result": {
                "result": {
                    "type": "string",
                    "value": r#"{"ok":true,"delivered":"active","snapshot":{"loaded":true},"delivery":{"queryEntries":1}}"#
                }
            }
        });
        let outcome = verify_model_whitelist_refresh_response(&success).unwrap();
        assert!(!outcome.deferred);
        let mismatch = serde_json::json!({
            "result": {
                "result": {
                    "type": "string",
                    "value": r#"{"ok":false,"error":"模型白名单快照与已保存配置不一致（statsigClients=1, notifiedClients=1, queryClients=1, queryEntries=0）"}"#
                }
            }
        });
        let error = verify_model_whitelist_refresh_response(&mismatch).unwrap_err();
        assert!(format!("{error:#}").contains("快照与已保存配置不一致"));
        assert!(format!("{error:#}").contains("queryEntries=0"));
    }

    #[test]
    fn model_whitelist_refresh_response_reports_a_deferred_delivery() {
        let deferred = serde_json::json!({
            "result": {
                "result": {
                    "type": "string",
                    "value": r#"{"ok":true,"delivered":"deferred","snapshot":{"loaded":true},"delivery":{"queryEntries":0}}"#
                }
            }
        });
        let outcome = verify_model_whitelist_refresh_response(&deferred).unwrap();
        assert!(outcome.deferred);
    }

    #[test]
    fn core_scripts_share_one_cdp_document_script_and_user_scripts_stay_isolated() {
        let prepared = prepare_injection_scripts_for_platform(
            true,
            false,
            false,
            &["".to_string(), "window.userScriptRan = true;".to_string()],
            InjectionHostPlatform::Windows,
        );

        assert_eq!(prepared.scripts.len(), 2);
        let core = &prepared.scripts[0];
        assert!(core.contains("window.__codeyBridgeHelpersInstalled"));
        assert!(core.contains("__codeyGitRequestGuard"));
        assert!(core.contains("__codeyWindowsWmiSamplerGuard"));
        assert!(core.contains("window.__codeyModelWhitelistPatch"));
        assert!(core.contains("/codex-model-catalog"));
        let shared_runtime_offset = core
            .find("window.__codeySharedRuntime=Object.freeze")
            .expect("bridge helpers must initialize the shared runtime");
        let locale_offset = core
            .find("__codeyDefaultChineseLocale")
            .expect("locale bootstrap must be part of renderer-controls");
        let renderer_offset = core
            .find("window.__codeyRendererCoreLoaded")
            .expect("renderer bootstrap must be part of renderer-controls");
        assert!(shared_runtime_offset < locale_offset);
        assert!(locale_offset < renderer_offset);
        assert!(core.contains("window.__codeyRendererCoreLoaded"));
        assert!(core.contains(r#"["false"][0]==="true""#));
        assert!(core.contains(SETTINGS_OVERLAY_LOAD_PATH));
        assert!(core.contains(SESSION_TOOLS_LOAD_PATH));
        assert!(core.contains("__codeyLazyLoader"));
        assert!(!core.contains("codey-settings-overlay-host"));
        assert!(!core.contains("hardDeletedMessageKeys"));
        assert!(core.len() < SETTINGS_OVERLAY_SCRIPT.len());
        assert!(core.contains("插件市场兼容 injection failed"));
        assert!(core.contains("window.__codeyInjectionStatus"));
        assert!(prepared.scripts[1].contains("window.userScriptRan = true;"));
        assert!(prepared.scripts[1].contains(r#"status = "executed""#));
        assert!(prepared.scripts[1].contains("用户脚本 1 injection failed"));
        assert_eq!(prepared.descriptors.len(), 11);
        assert_eq!(prepared.descriptors[10].id, "user-script-1");
        assert_eq!(prepared.descriptors[10].source, "user");
        assert_eq!(
            prepared.descriptors[0].visibility,
            InjectionScriptVisibility::Internal
        );
        assert_eq!(prepared.descriptors[7].id, "renderer-controls");
        assert_eq!(
            prepared.descriptors[7].visibility,
            InjectionScriptVisibility::Internal
        );
        assert_eq!(
            prepared.descriptors[10].visibility,
            InjectionScriptVisibility::Feature
        );
        let snapshot_script = injection_status_snapshot_script(&prepared.descriptors);
        assert!(snapshot_script.contains("bridge-helpers"));
        assert!(snapshot_script.contains("Windows Git 请求限流已由主进程接管"));
        assert!(snapshot_script.contains("guard.ensureInstalled?.()"));
        assert!(snapshot_script.contains("snapshot.mainProcessProtected === true"));
        assert!(snapshot_script.contains("WMI 周期采样保护已安装"));
        assert!(snapshot_script.contains("snapshot.blocked > 0"));
        assert!(snapshot_script.contains("snapshot.selfTestConfirmed === true"));
        assert!(snapshot_script.contains("effective: false"));
        assert!(snapshot_script.contains("entry.status = \"inactive\""));
        assert!(snapshot_script.contains("[\"executed\", \"effective\", \"inactive\"].includes"));
        assert!(snapshot_script.contains("Object.prototype.hasOwnProperty.call"));
        assert!(snapshot_script.contains("模型目录已加载"));
        assert!(snapshot_script.contains("插件市场桥接已接管"));
        assert!(snapshot_script.contains("for (const delay of [50, 200, 750])"));
        assert!(!snapshot_script.contains("user-script-1\": () =>"));
        let overlay_load_script = prepared_settings_overlay_load_script();
        assert!(overlay_load_script.contains("codey-settings-overlay-host"));
        assert!(overlay_load_script.contains("data-mantine-color-scheme"));
        assert!(overlay_load_script.contains("--button-bg"));
        assert!(overlay_load_script.contains("--mantine-color-blue-6:"));
        assert!(overlay_load_script.contains("delete window.__codeySettingsOverlay"));
        assert!(
            overlay_load_script.contains("window.__codeySettingsOverlay = current"),
            "a failed bundle evaluation must restore the lazy loader for retry"
        );
        let session_tools_load_script = prepared_session_tools_load_script();
        assert!(session_tools_load_script.contains("window.__codeySessionToolsInjectLoaded"));
        assert!(
            session_tools_load_script.contains("window.__codeySessionToolsInjectLoading = false")
        );
        // 压缩会改写内部标识符，锚点必须用不会被改名的 window 属性。
        assert!(session_tools_load_script.contains("__codeyDeleteSelectedMessages"));
    }

    #[test]
    fn windows_only_scripts_are_excluded_from_non_windows_injection() {
        let user_scripts = ["window.userScriptRan = true;".to_string()];
        let non_windows = prepare_injection_scripts_for_platform(
            true,
            false,
            false,
            &user_scripts,
            InjectionHostPlatform::Other,
        );
        let windows = prepare_injection_scripts_for_platform(
            true,
            false,
            false,
            &user_scripts,
            InjectionHostPlatform::Windows,
        );

        for windows_only_id in ["git-request-guard", "windows-wmi-sampler"] {
            assert!(
                non_windows
                    .descriptors
                    .iter()
                    .all(|descriptor| descriptor.id != windows_only_id)
            );
            assert!(
                windows
                    .descriptors
                    .iter()
                    .any(|descriptor| descriptor.id == windows_only_id)
            );
        }
        assert!(!non_windows.scripts[0].contains("__codeyGitRequestGuard"));
        assert!(!non_windows.scripts[0].contains("__codeyWindowsWmiSamplerGuard"));
        assert_eq!(
            non_windows
                .descriptors
                .last()
                .map(|descriptor| descriptor.id.as_str()),
            Some("user-script-1")
        );

        let current = prepare_injection_scripts(true, false, false, &[]);
        let current_has_windows_scripts = current
            .descriptors
            .iter()
            .any(|descriptor| descriptor.id == "git-request-guard");
        assert_eq!(current_has_windows_scripts, cfg!(windows));
    }

    #[test]
    fn disabled_local_router_installs_only_native_model_selection_mode() {
        let prepared = prepare_injection_scripts_for_platform(
            false,
            false,
            false,
            &[],
            InjectionHostPlatform::Other,
        );

        assert!(
            prepared
                .descriptors
                .iter()
                .any(|descriptor| descriptor.id == "model-whitelist")
        );
        assert!(prepared.scripts[0].contains("window.__codeyNativeModelSelectionOnly = true;"));
        assert!(prepared.scripts[0].contains("window.__codeyBridgeHelpersInstalled"));
        assert!(
            prepared
                .descriptors
                .iter()
                .any(|descriptor| descriptor.id == "settings-overlay-loader")
        );
    }

    #[test]
    fn injection_statuses_preserve_script_order_and_report_missing_entries() {
        let prepared = prepare_injection_scripts_for_platform(
            true,
            false,
            false,
            &["window.userScriptRan = true;".to_string()],
            InjectionHostPlatform::Windows,
        );
        let reported = vec![
            RuntimeInjectionStatus {
                id: "user-script-1".to_string(),
                status: "failed".to_string(),
                detail: None,
                error: Some("boom".repeat(200)),
            },
            RuntimeInjectionStatus {
                id: "bridge-helpers".to_string(),
                status: "effective".to_string(),
                detail: Some("桥接函数可调用".to_string()),
                error: None,
            },
            RuntimeInjectionStatus {
                id: "security-warning-shield".to_string(),
                status: "inactive".to_string(),
                detail: Some("控制器已就绪，当前屏蔽策略关闭".to_string()),
                error: None,
            },
        ];

        let statuses = reconcile_injection_statuses(&prepared.descriptors, reported);

        assert_eq!(statuses.len(), prepared.descriptors.len());
        assert_eq!(statuses[0].id, "bridge-helpers");
        assert_eq!(statuses[0].status, "effective");
        assert_eq!(statuses[0].detail.as_deref(), Some("桥接函数可调用"));
        assert_eq!(statuses[1].id, "git-request-guard");
        assert_eq!(statuses[1].status, "unknown");
        assert_eq!(statuses[2].id, "windows-wmi-sampler");
        assert_eq!(statuses[2].status, "unknown");
        assert_eq!(statuses[3].id, "model-whitelist");
        assert_eq!(statuses[3].status, "unknown");
        assert_eq!(statuses[5].id, "security-warning-shield");
        assert_eq!(statuses[5].status, "inactive");
        assert_eq!(
            statuses.last().map(|status| status.id.as_str()),
            Some("user-script-1")
        );
        assert_eq!(
            statuses.last().map(|status| status.status.as_str()),
            Some("failed")
        );
        assert_eq!(
            statuses
                .last()
                .and_then(|status| status.error.as_deref())
                .map(str::chars)
                .map(Iterator::count),
            Some(MAX_INJECTION_ERROR_CHARS)
        );
    }

    #[test]
    fn failed_settings_overlay_bundle_restores_the_lazy_loader() {
        let script = settings_overlay_load_script("throw new Error('bundle failed');");

        let delete_index = script
            .find("delete window.__codeySettingsOverlay")
            .expect("lazy loader should be removed before evaluating the bundle");
        let restore_index = script
            .find("window.__codeySettingsOverlay = current")
            .expect("lazy loader should be restored when the bundle is not ready");

        assert!(restore_index > delete_index);
        assert!(script.contains("if (ready) return \"\""));
    }
}
