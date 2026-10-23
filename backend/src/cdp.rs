use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use codey_runtime_core::bridge::{
    BridgeHandler, BridgeInstallOptions, BridgePumpHandle, bridge_health_check_script,
    install_bridge_with_options,
};
use codey_runtime_core::cdp::{CdpTarget, list_targets, pick_injectable_codex_page_target};
use serde::{Deserialize, Serialize};

use crate::error_log;

#[cfg(any(windows, test))]
mod app_quit;
#[cfg(windows)]
pub(crate) use app_quit::request_app_quit;

const SETTINGS_OVERLAY_LOAD_PATH: &str = "/internal/codey/settings-overlay/load";
const SESSION_TOOLS_LOAD_PATH: &str = "/internal/codey/session-tools/load";
const CDP_INJECTION_TIMEOUT: Duration = Duration::from_secs(30);
const INITIAL_RETRY_INTERVAL: Duration = Duration::from_millis(100);
// 发现与选择阶段的失败只付一次 loopback 请求的代价，既不触碰 renderer，也不
// 留下 CDP 副作用，因此可以用更密的节奏跟随 renderer 就绪。校验阶段的失败
// 复用已装好的桥接，但每次探测仍要在页面里跑一轮资产导入与 React 遍历；
// 桥接安装失败则必须重建 WebSocket 并重复注册持久注入脚本。两类失败都保持
// 原有退避，避免在 Codex 冷启动的 CPU 高峰窗口里叠加负担。
const DISCOVERY_RETRY_STEP: Duration = Duration::from_millis(50);
const DISCOVERY_RETRY_MAX_INTERVAL: Duration = Duration::from_millis(200);
// 发现阶段单次失败超过这个预算就不再算廉价探测：正常情况是一次被拒绝或返回空
// 列表的 loopback 请求，远低于此值。
const DISCOVERY_RETRY_COST_BUDGET: Duration = Duration::from_millis(100);
const BRIDGE_RETRY_MAX_INTERVAL: Duration = Duration::from_millis(500);
// 门禁要等 Codex 自己建好 request client（实测导航后 6-9 秒），且只是增强：
// 启动注入完全不碰它，就绪由后台轮询确认；未就绪期间请求路由由 CLI 包装层的
// stdin 改写兜底。
const REQUEST_GATE_SETTLE_ROUNDS: u32 = 3;
const REQUEST_GATE_CDP_MARGIN: Duration = Duration::from_secs(1);
// 就绪探针是一次同步求值；忙碌的 renderer 也会排队执行并回包，单次预算给足，
// 超时只保留桥接重探，不触发重装。
const READINESS_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const READINESS_PROBE_MIN_BUDGET: Duration = Duration::from_millis(500);
const VERIFY_RESERVED: Duration = Duration::from_secs(3);
const INJECTION_FINISH_MARGIN: Duration = Duration::from_millis(100);
const MINIMUM_VERIFICATION_BUDGET: Duration =
    VERIFY_RESERVED.saturating_add(INJECTION_FINISH_MARGIN);
const SESSION_TOOLS_INJECT_TIMEOUT: Duration = Duration::from_secs(8);
const MCP_RELOAD_EVALUATE_TIMEOUT: Duration = Duration::from_secs(20);
const CODEY_BRIDGE_SCRIPT: &str = include_str!("../../dist-overlay/inject/codey-bridge.js");
const MODEL_WHITELIST_INJECT_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/model-whitelist-inject.js");
const RENDERER_INJECT_SCRIPT: &str = include_str!("../../dist-overlay/inject/renderer-inject.js");
const CODEY_SESSION_TOOLS_SCRIPT: &str = include_str!("../../dist-overlay/inject/codey-inject.js");
const PET_CONTROL_SHIELD_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/pet-control-shield.js");
const SECURITY_WARNING_SHIELD_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/security-warning-shield.js");
const QUOTA_UNLOCK_SCRIPT: &str = include_str!("../../dist-overlay/inject/quota-unlock.js");
pub(crate) const SETTINGS_OVERLAY_SCRIPT: &str =
    include_str!("../../dist-overlay/codey-overlay.js");
const PLUGIN_MARKETPLACE_FIX_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/plugin-marketplace-fix.js");
const PROMPT_OPTIMIZE_SCRIPT: &str = include_str!("../../dist-overlay/inject/prompt-optimize.js");
const CONVERSATION_STATS_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/conversation-stats-inject.js");
const MAX_INJECTION_ERROR_CHARS: usize = 500;
// 面向用户的是归类后的摘要；完整错误链只进诊断与错误日志，单独给一个更大预算。
const MAX_INJECTION_DETAIL_CHARS: usize = 2000;
static SETTINGS_OVERLAY_LOAD_SCRIPT: OnceLock<Arc<str>> = OnceLock::new();
static SESSION_TOOLS_LOAD_SCRIPT: OnceLock<Arc<str>> = OnceLock::new();

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
enum InjectionPhase {
    DiscoverTargets,
    SelectTarget,
    InstallBridge,
    VerifyReadiness,
    ReadStatuses,
}

impl InjectionPhase {
    fn from_raw(value: u8) -> Self {
        match value {
            value if value == Self::SelectTarget as u8 => Self::SelectTarget,
            value if value == Self::InstallBridge as u8 => Self::InstallBridge,
            value if value == Self::VerifyReadiness as u8 => Self::VerifyReadiness,
            value if value == Self::ReadStatuses as u8 => Self::ReadStatuses,
            _ => Self::DiscoverTargets,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::DiscoverTargets => "枚举 CDP 页面",
            Self::SelectTarget => "选择 Codex renderer",
            Self::InstallBridge => "安装 CDP bridge 与注入脚本",
            Self::VerifyReadiness => "确认页面注入就绪",
            Self::ReadStatuses => "读取注入状态",
        }
    }

    /// 诊断日志用的稳定标识，避免把面向用户的中文文案写进可检索字段。
    fn key(self) -> &'static str {
        match self {
            Self::DiscoverTargets => "discoverTargets",
            Self::SelectTarget => "selectTarget",
            Self::InstallBridge => "installBridge",
            Self::VerifyReadiness => "verifyReadiness",
            Self::ReadStatuses => "readStatuses",
        }
    }

    /// 该阶段的失败是否只花掉一次廉价探测：没有建立 CDP 会话，因而可以安全地
    /// 用更短的间隔重试。
    fn is_discovery(self) -> bool {
        matches!(self, Self::DiscoverTargets | Self::SelectTarget)
    }
}

const INJECTION_PHASES: [InjectionPhase; 5] = [
    InjectionPhase::DiscoverTargets,
    InjectionPhase::SelectTarget,
    InjectionPhase::InstallBridge,
    InjectionPhase::VerifyReadiness,
    InjectionPhase::ReadStatuses,
];
const INJECTION_PHASE_COUNT: usize = INJECTION_PHASES.len();

/// 触发这一轮注入重试的入口：首次启动等待渲染进程就绪，与看门狗在运行期按
/// 健康判定重建，失败画像的含义完全不同，日志里必须能分开。
#[derive(Clone, Copy)]
pub enum InjectionRetrySource {
    Startup,
    Watchdog,
}

impl InjectionRetrySource {
    fn key(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Watchdog => "watchdog",
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
    disable_pet: bool,
}

pub struct InjectedTarget {
    websocket_url: Arc<str>,
    pump: BridgePumpHandle,
    injection_statuses: Arc<[InjectionScriptStatus]>,
}

#[derive(Debug)]
pub struct InjectionRetryFailure {
    error: anyhow::Error,
    detail: Option<String>,
    /// 重试终止时所处的阶段：一直停在发现阶段意味着调试端点本身已不可达，
    /// 看门狗据此把「页面修不好」与「Codex 已在托管外重启」分开处理。
    final_phase: InjectionPhase,
    /// 进入过桥接安装及之后阶段的失败轮数：非零说明端点曾经可达。
    reached_bridge_stage: u32,
}

impl InjectionRetryFailure {
    pub fn into_error(self) -> anyhow::Error {
        self.error
    }

    /// 最后一次注入尝试的完整错误链（已截断）。对外错误只保留归类摘要，
    /// 排查需要这条原始信息，调用方把它写进错误日志上下文。
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// 整轮重试都停在枚举/选择目标阶段：调试端口已随旧进程消失，或当前
    /// 进程里没有任何可注入页面——在这条会话上继续重试不可能恢复。
    /// 只要有一轮摸到过桥接阶段就说明端点活着，不再适用。
    pub fn debug_endpoint_unreachable(&self) -> bool {
        self.final_phase.is_discovery() && self.reached_bridge_stage == 0
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

    /// 常驻桥接连接的消息泵是否已经结束。
    /// 结束时页面内的桥接调用再也无法送达后端，重新注入是唯一出路。
    pub fn pump_finished(&self) -> bool {
        self.pump.is_finished()
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
    use InjectionScriptVisibility::{Feature, Internal};

    let mut builtin_scripts = vec![
        (
            "bridge-helpers",
            "桥接辅助",
            CODEY_BRIDGE_SCRIPT,
            r#"typeof window.__codexSessionDeleteBridge === "function"
              && typeof window.__codeyCall === "function"
              ? "桥接函数可调用" : """#
                .to_string(),
            Internal,
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
        ),
        (
            "renderer-controls",
            "渲染器控制",
            RENDERER_INJECT_SCRIPT,
            r#"(() => {
              if (window.__codeyRendererCoreLoaded !== true
                || typeof window.__codeyRendererScan !== "function"
                || typeof window.__codeyLoadSessionTools !== "function") return "";
              return "渲染器控制与按需加载 API 可用";
            })()"#
                .to_string(),
            Internal,
        ),
        (
            "conversation-stats",
            "会话状态条",
            CONVERSATION_STATS_SCRIPT,
            r#"window.__codeyConversationStats?.installed === true
              ? "会话状态条已就绪" : """#
                .to_string(),
            Internal,
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
        ),
    ];
    if local_router_enabled {
        builtin_scripts.push((
            "quota-unlock",
            "额度状态兼容",
            QUOTA_UNLOCK_SCRIPT,
            r#"(() => {
              const helper = window.__codeyQuotaUnlock;
              const status = helper?.status?.();
              if (!status) return "";
              if (typeof helper.allowsLocalSend !== "function")
                return { effective: false, detail: "本机第三方模型发送额度判断未就绪" };
              return status.enabled === true
                ? { effective: true, detail: "本机第三方模型发送额度判断已启用" }
                : { effective: false, inactive: true, detail: "额度状态兼容已关闭" };
            })()"#
                .to_string(),
            Feature,
        ));
    }
    let mut core_bundle = String::with_capacity(
        CODEY_BRIDGE_SCRIPT.len()
            + MODEL_WHITELIST_INJECT_SCRIPT.len()
            + RENDERER_INJECT_SCRIPT.len()
            + PET_CONTROL_SHIELD_SCRIPT.len()
            + SECURITY_WARNING_SHIELD_SCRIPT.len()
            + QUOTA_UNLOCK_SCRIPT.len()
            + PLUGIN_MARKETPLACE_FIX_SCRIPT.len()
            + PROMPT_OPTIMIZE_SCRIPT.len()
            + CONVERSATION_STATS_SCRIPT.len()
            + 4096,
    );
    let mut descriptors = Vec::with_capacity(builtin_scripts.len() + user_scripts.len());
    for (id, name, script, probe, visibility) in builtin_scripts {
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
        disable_pet: slim_codex_pet,
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
    // Built-ins repair their own state on reconnect; user scripts may install
    // listeners or timers and should run only once per document after success.
    if descriptor.source == "user" {
        bundle.push_str("\nif (window.__codeyInjectionStatus?.[");
        bundle.push_str(&id);
        bundle.push_str("]?.status !== \"executed\") {\n");
    }
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
    if descriptor.source == "user" {
        bundle.push_str("}\n");
    }
}

/// 预取的内部预算与注入总期限同级。
pub const PREFETCH_BUDGET: Duration = CDP_INJECTION_TIMEOUT;
// 预取仅占启动预算的一部分，后续枚举、桥接安装和校验仍有机会完成。
const PREFETCH_HANDOFF_BUDGET: Duration = Duration::from_secs(8);

/// 在 Codex 进程拉起与 CLI 包装器握手窗口内预取页面目标：调试端口在
/// Chromium 主进程就绪后先于握手完成可用。找不到目标返回 None，注入主流程
/// 照常走发现重试。
pub fn spawn_injectable_target_prefetch(debug_port: u16) -> crate::target_prefetch::PrefetchTask {
    crate::target_prefetch::spawn_target_prefetch(debug_port, PREFETCH_BUDGET)
}

pub async fn retry_inject_with_scripts(
    debug_port: u16,
    handler: BridgeHandler,
    scripts: &PreparedInjectionScripts,
    source: InjectionRetrySource,
    prefetched_target: Option<crate::target_prefetch::PrefetchTask>,
) -> std::result::Result<InjectedTarget, InjectionRetryFailure> {
    // Renderer asset preparation on newer Windows Codex builds can consume
    // more than ten seconds before the first injectable page appears. Keep
    // enough budget for the bridge commands after discovery while retaining a
    // hard startup deadline.
    let started = tokio::time::Instant::now();
    let deadline = started + CDP_INJECTION_TIMEOUT;
    let mut stats = InjectionRetryStats::default();
    // 先限时交接预取，再运行常规枚举，避免重复轮询与预取独占总预算。
    let mut prefetched_target = match prefetched_target {
        Some(prefetch) => {
            let (target, diagnostics) = prefetch
                .wait(prefetch_handoff_deadline(started, deadline))
                .await;
            stats.prefetch = Some(diagnostics);
            target
        }
        None => None,
    };
    let mut delay = INITIAL_RETRY_INTERVAL;
    let phase = Arc::new(AtomicU8::new(InjectionPhase::DiscoverTargets as u8));
    let mut previous_error = stats
        .prefetch
        .as_ref()
        .and_then(|prefetch| prefetch.last_discovery_error.clone());
    // 确认失败时桥接本身仍然可用：把它跨轮保留，下一轮只在原桥接上重新确认，
    // 不再重连并重放全部注入脚本。
    let mut retained_bridge: Option<EstablishedInjection> = None;
    let (last_error, detail) = loop {
        stats.attempts += 1;
        let attempt_started = tokio::time::Instant::now();
        match tokio::time::timeout_at(
            deadline,
            run_injection_attempt(
                retained_bridge.take(),
                prefetched_target.take(),
                debug_port,
                handler.clone(),
                scripts,
                &phase,
                deadline,
            ),
        )
        .await
        {
            Ok(AttemptOutcome::Injected { target, readiness }) => {
                stats.readiness = Some(readiness);
                stats.report(source, "injected", started.elapsed(), None);
                return Ok(target);
            }
            Ok(AttemptOutcome::Retry { error, retained }) => {
                retained_bridge = retained;
                let current_phase = InjectionPhase::from_raw(phase.load(Ordering::Acquire));
                let failure_cost = attempt_started.elapsed();
                stats.record_failure(current_phase, failure_cost);
                let detail = injection_failure_detail(&error);
                let retry_budget = if matches!(current_phase, InjectionPhase::VerifyReadiness) {
                    delay + MINIMUM_VERIFICATION_BUDGET
                } else {
                    delay
                };
                if tokio::time::Instant::now() + retry_budget > deadline {
                    // 桥接与脚本已装好，只差确认（页面持续忙碌或仍在首次导航）：
                    // 交给看门狗复核，不为确认未完成停掉 Codex。
                    if let Some(bridge) = retained_bridge
                        .take()
                        .filter(|bridge| !bridge.pump.is_finished())
                    {
                        stats.report(
                            source,
                            "acceptedUnverified",
                            started.elapsed(),
                            Some(&detail),
                        );
                        return Ok(bridge.into_injected(
                            scripts.statuses_with_error("启动确认未完成，等待后台复核注入状态"),
                        ));
                    }
                    stats.report(source, "noRetryBudget", started.elapsed(), Some(&detail));
                    break (
                        anyhow::anyhow!(
                            "Codex CDP bridge 注入失败（阶段：{}；{}）",
                            current_phase.label(),
                            safe_injection_error_summary(&error)
                        ),
                        detail,
                    );
                }
                previous_error = Some(safe_injection_error_summary(&error));
                tokio::time::sleep(delay).await;
                delay = next_injection_retry_delay(delay, current_phase, failure_cost);
            }
            Err(_) => {
                let current_phase = InjectionPhase::from_raw(phase.load(Ordering::Acquire));
                stats.record_failure(current_phase, attempt_started.elapsed());
                let detail = injection_deadline_detail(
                    current_phase,
                    started.elapsed(),
                    attempt_started.elapsed(),
                    deadline.saturating_duration_since(tokio::time::Instant::now()),
                );
                stats.report(source, "deadlineExceeded", started.elapsed(), Some(&detail));
                let previous_error = previous_error
                    .as_deref()
                    .map(|error| format!("；最近一次失败：{error}"))
                    .unwrap_or_default();
                break (
                    anyhow::anyhow!(
                        "等待 Codex CDP bridge 注入超时（{} ms，阶段：{}{}）",
                        CDP_INJECTION_TIMEOUT.as_millis(),
                        current_phase.label(),
                        previous_error
                    ),
                    detail,
                );
            }
        }
    };
    Err(InjectionRetryFailure {
        error: last_error,
        detail: Some(detail),
        final_phase: InjectionPhase::from_raw(phase.load(Ordering::Acquire)),
        reached_bridge_stage: stats.bridge_failures(),
    })
}

fn prefetch_handoff_deadline(
    started: tokio::time::Instant,
    deadline: tokio::time::Instant,
) -> tokio::time::Instant {
    let remaining = deadline.saturating_duration_since(started);
    started + PREFETCH_HANDOFF_BUDGET.min(remaining.saturating_sub(MINIMUM_VERIFICATION_BUDGET))
}

/// 下一次重试前的等待时长。发现与选择阶段的失败通常只付一次 loopback 请求的
/// 代价，既不触碰 renderer 也不留 CDP 副作用，用小步长逼近 renderer 就绪即可。
/// 但同一阶段的失败也可能突然变贵（例如调试端口被别的服务占用而让请求挂到
/// 超时），因此还要看刚结束的那次失败的实付代价：一旦不再廉价就立刻退回保守
/// 退避。桥接安装失败要重建 WebSocket 并重放脚本，校验失败虽然复用桥接却仍要
/// 驱动一轮页面内探测，两者无论代价高低都沿用原来的指数退避。
fn next_injection_retry_delay(
    current: Duration,
    phase: InjectionPhase,
    last_failure_cost: Duration,
) -> Duration {
    if phase.is_discovery() && last_failure_cost <= DISCOVERY_RETRY_COST_BUDGET {
        return (current + DISCOVERY_RETRY_STEP).min(DISCOVERY_RETRY_MAX_INTERVAL);
    }
    (current * 2).min(BRIDGE_RETRY_MAX_INTERVAL)
}

/// 单次注入重试的失败画像：按阶段分别计数，不再把桥接安装与后续校验压缩成
/// 同一个 bucket——「注入慢」的归因直接读 failures 即可。每次启动只写一条
/// 汇总，不随重试次数增长。
#[derive(Default)]
struct InjectionRetryStats {
    prefetch: Option<crate::target_prefetch::PrefetchDiagnostics>,
    /// 成功轮次的就绪探针摘要（页面地址形态、模型补丁是否在位）。
    readiness: Option<serde_json::Value>,
    attempts: u32,
    phase_failures: [u32; INJECTION_PHASE_COUNT],
    failure_expense_ms: u64,
    last_failure_phase: Option<InjectionPhase>,
}

impl InjectionRetryStats {
    fn record_failure(&mut self, phase: InjectionPhase, cost: Duration) {
        self.phase_failures[phase as usize] += 1;
        self.failure_expense_ms += cost.as_millis() as u64;
        self.last_failure_phase = Some(phase);
    }

    fn discovery_failures(&self) -> u32 {
        self.phase_failures[..=InjectionPhase::SelectTarget as usize]
            .iter()
            .sum()
    }

    fn bridge_failures(&self) -> u32 {
        self.phase_failures[InjectionPhase::InstallBridge as usize..]
            .iter()
            .sum()
    }

    fn report(
        &self,
        source: InjectionRetrySource,
        outcome: &str,
        elapsed: Duration,
        last_error: Option<&str>,
    ) {
        let failures: serde_json::Map<String, serde_json::Value> = INJECTION_PHASES
            .iter()
            .filter(|phase| self.phase_failures[**phase as usize] > 0)
            .map(|phase| {
                let index = *phase as usize;
                (
                    phase.key().to_string(),
                    serde_json::json!(self.phase_failures[index]),
                )
            })
            .collect();
        let mut detail = serde_json::json!({
            "source": source.key(),
            "outcome": outcome,
            "attempts": self.attempts,
            "discoveryFailures": self.discovery_failures(),
            "bridgeFailures": self.bridge_failures(),
            "failures": failures,
            "failureExpenseMs": self.failure_expense_ms,
            "lastFailurePhase": self.last_failure_phase.map(InjectionPhase::key),
            "totalMs": elapsed.as_millis() as u64,
        });
        // 汇总文案只保留归类结果，完整错误链随最后一次尝试一起进诊断记录。
        if let Some(last_error) = last_error {
            detail["lastError"] = serde_json::json!(last_error);
        }
        if let Some(prefetch) = &self.prefetch {
            detail["prefetch"] = serde_json::json!(prefetch);
        }
        if let Some(readiness) = &self.readiness {
            detail["readiness"] = readiness.clone();
        }
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "cdp.injection_retry_summary",
            detail,
        );
    }
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
                truncate_chars(&target.target_type, 20),
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
        return truncate_chars(&trimmed[..end], 100);
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
            &format!("未发现匹配的 Codex renderer；CDP targets={targets}"),
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
    } else if normalized.contains("codex 辅助窗口") {
        "注入目标是 Codex 辅助窗口，已改选主窗口".to_string()
    } else if normalized.contains("尚未完成首次导航") {
        "Codex 页面尚未完成首次导航".to_string()
    } else if normalized.contains("codex 页面没有 cdp websocket 地址") {
        "Codex 页面缺少调试连接地址".to_string()
    } else if normalized.contains("cdp websocket closed before response") {
        let method = message
            .split_once("CDP websocket closed before response for ")
            .and_then(|(_, rest)| rest.split_once(" id ").map(|(method, _)| method.trim()));
        method.map_or_else(
            || "CDP 连接被 Codex 关闭".to_string(),
            |method| format!("CDP 连接被 Codex 关闭（{method}）"),
        )
    } else if normalized.contains("failed to send cdp command")
        || normalized.contains("failed to read cdp websocket message")
        || normalized.contains("failed to parse cdp message")
    {
        "CDP 连接中断".to_string()
    } else if normalized.contains("cdp command ") && normalized.contains(" failed: ") {
        cdp_command_failure_summary(&message).unwrap_or_else(|| "CDP 命令被 Codex 拒绝".to_string())
    } else if normalized.contains("模型请求校验未通过：") {
        // 校验脚本以 JSON 返回页面侧失败原因（"当前 Codex 暂不支持模型请求
        // 校验"、"模型请求补丁未注入"等），文案本身就是可用的归类摘要。
        message
            .lines()
            .next()
            .map(|line| truncate_chars(line, MAX_INJECTION_ERROR_CHARS))
            .unwrap_or_else(|| "模型请求校验未通过".to_string())
    } else if normalized.contains("安装 codex 模型请求校验失败")
        || normalized.contains("校验模型请求入口失败")
    {
        // 校验入口失败时页面抛出的错误（"模型请求校验尚未就绪"、"当前 Codex
        // 暂不支持模型请求校验"等）藏在 exceptionDetails 里，取出来才有定位价值。
        evaluate_exception_detail(&message).map_or_else(
            || "模型请求校验未通过".to_string(),
            |detail| format!("模型请求校验未通过：{detail}"),
        )
    } else if normalized.contains("runtime.evaluate raised an exception") {
        evaluate_exception_detail(&message).map_or_else(
            || "页面脚本执行异常".to_string(),
            |detail| format!("页面脚本执行异常：{detail}"),
        )
    } else {
        "内部注入尝试失败".to_string()
    }
}

/// 完整错误链（含 anyhow 上下文）只进诊断与错误日志，不进用户可见文案。
fn injection_failure_detail(error: &anyhow::Error) -> String {
    truncate_chars(&format!("{error:#}"), MAX_INJECTION_DETAIL_CHARS)
}

fn injection_deadline_detail(
    phase: InjectionPhase,
    total: Duration,
    attempt: Duration,
    remaining: Duration,
) -> String {
    format!(
        "injection deadline exceeded: phase={}; totalMs={}; attemptMs={}; remainingMs={}",
        phase.key(),
        total.as_millis(),
        attempt.as_millis(),
        remaining.as_millis(),
    )
}

/// CDP 命令错误响应形如 `{"code":-32601,"message":"..."}`，摘出方法与
/// 协议消息比笼统的"内部注入尝试失败"更能定位是哪一步被拒绝。
fn cdp_command_failure_summary(message: &str) -> Option<String> {
    let (_, rest) = message.split_once("CDP command ")?;
    let (method, rest) = rest.split_once(" id ")?;
    let (_, error_json) = rest.split_once(" failed: ")?;
    let payload = serde_json::from_str::<serde_json::Value>(error_json.trim()).ok()?;
    let reason = payload.get("message").and_then(serde_json::Value::as_str)?;
    Some(truncate_chars(
        &format!(
            "CDP 命令 {} 被 Codex 拒绝：{}",
            method.trim(),
            reason.trim()
        ),
        MAX_INJECTION_ERROR_CHARS,
    ))
}

/// Runtime.evaluate 的 exceptionDetails 里带页面侧错误首行（注入脚本或
/// Codex 内部抛出的错误），取它比只说"执行异常"更有定位价值。
fn evaluate_exception_detail(message: &str) -> Option<String> {
    let (_, details) = message.split_once("Runtime.evaluate raised an exception: ")?;
    let payload = serde_json::from_str::<serde_json::Value>(details.trim_end()).ok()?;
    let exception = payload.get("exception")?;
    let description = exception
        .get("description")
        .and_then(serde_json::Value::as_str)?;
    let first_line = description.lines().next()?.trim();
    if first_line.is_empty() {
        return None;
    }
    // "Error: 描述" / "TypeError: 描述" 的首行去掉类型前缀，只留可读内容。
    let detail = first_line
        .split_once(": ")
        .filter(|(head, _)| {
            !head.is_empty()
                && head
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '$' | '_' | '.'))
        })
        .map(|(_, rest)| rest.trim())
        .filter(|rest| !rest.is_empty())
        .unwrap_or(first_line);
    Some(truncate_chars(detail, MAX_INJECTION_ERROR_CHARS / 2))
}

/// 已成功安装桥接的 CDP 会话。确认阶段失败时保留它，下一轮重试只在原
/// 桥接上重新确认，不重连也不重放注入脚本。
struct EstablishedInjection {
    target_id: String,
    websocket_url: Arc<str>,
    pump: BridgePumpHandle,
    /// 装桥会话上顺带执行的就绪探针结果，只供首轮确认使用。
    install_probe: Option<serde_json::Value>,
}

impl EstablishedInjection {
    /// 注入完成后才启动请求入口的后台确认：它要等 Codex 自建 request client，
    /// 不属于注入本身的成败。
    fn into_injected(self, injection_statuses: Arc<[InjectionScriptStatus]>) -> InjectedTarget {
        spawn_request_gate_settle(self.target_id, self.websocket_url.clone());
        InjectedTarget {
            websocket_url: self.websocket_url,
            pump: self.pump,
            injection_statuses,
        }
    }
}

/// 确认阶段的失败需要区分恢复方式：页面暂时无法应答或仍在首次导航时只要保留
/// 桥接继续确认；注入脚本缺失、装到辅助窗口或 CDP 连接失效时才需要整轮重装。
#[derive(Debug)]
struct VerifyFailure {
    reinstall: bool,
    error: anyhow::Error,
}

impl VerifyFailure {
    fn retain(error: anyhow::Error) -> Self {
        Self {
            reinstall: false,
            error,
        }
    }

    fn reinstall(error: anyhow::Error) -> Self {
        Self {
            reinstall: true,
            error,
        }
    }
}

/// 装桥后的就绪探针：同步读取，不等待页面里的任何 Promise。
const READINESS_PROBE_SCRIPT: &str = r#"JSON.stringify({
  href: String(location.href),
  bridge: typeof window.__codexSessionDeleteBridge === "function",
  overlay: typeof window.__codeySettingsOverlay?.toggle === "function",
  modelPatch: typeof window.__codeyModelWhitelistPatch?.ensureRequestGate === "function",
  overlayError: String(window.__codeyOverlayError || "").slice(0, 300),
})"#;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ReadinessProbe {
    href: String,
    bridge: bool,
    overlay: bool,
    model_patch: bool,
    overlay_error: String,
}

impl ReadinessProbe {
    fn from_response(response: &serde_json::Value) -> Result<Self> {
        let payload = runtime_value(response)
            .and_then(serde_json::Value::as_str)
            .context("就绪探针未返回可解析结果")?;
        serde_json::from_str(payload).context("解析就绪探针结果失败")
    }

    fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "url": safe_target_url_shape(&self.href),
            "modelPatch": self.model_patch,
        })
    }
}

/// 根据探针结果判定这轮注入：辅助窗口与脚本缺失都需要重装，尚未导航只需等待。
fn judge_readiness(probe: ReadinessProbe) -> std::result::Result<ReadinessProbe, VerifyFailure> {
    if codey_runtime_core::cdp::is_auxiliary_codex_window_url(&probe.href) {
        return Err(VerifyFailure::reinstall(anyhow::anyhow!(
            "注入目标是 Codex 辅助窗口（{}），需要重新选择主窗口",
            safe_target_url_shape(&probe.href)
        )));
    }
    let href = probe.href.trim();
    if !(probe.bridge && probe.overlay) {
        if href.is_empty() || href.eq_ignore_ascii_case("about:blank") {
            return Err(VerifyFailure::retain(anyhow::anyhow!(
                "Codex 页面尚未完成首次导航"
            )));
        }
        let message = if probe.overlay_error.is_empty() {
            "未生成浮层控制器"
        } else {
            probe.overlay_error.as_str()
        };
        // 浮层由加载器脚本同步生成，缺失说明注入脚本没在当前文档执行，只能重装。
        return Err(VerifyFailure::reinstall(anyhow::anyhow!(
            "Codey 内嵌配置面板注入失败：{message}"
        )));
    }
    if href.is_empty() || href.eq_ignore_ascii_case("about:blank") {
        // 脚本已在初始空白文档执行，文档脚本会覆盖随后提交的应用文档；但目标
        // 是否主窗口要等真实地址出现才能确认。
        return Err(VerifyFailure::retain(anyhow::anyhow!(
            "Codex 页面尚未完成首次导航"
        )));
    }
    Ok(probe)
}

/// 校验脚本返回 JSON 而不是抛异常：页面侧能分清「还没就绪」与「补丁根本没
/// 装上」，宿主据此决定轮询还是重装。waitedMs/polls/sourceGate 是页内等待的
/// 运行画像：门禁通常要等 Codex 自己把 request client 建好，逐次 evaluate +
/// 指数退避只会放大检出延迟，所以等待被放进单次 evaluate 里完成。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestGateVerdict {
    ready: bool,
    #[serde(default)]
    reinstall: bool,
    message: Option<String>,
    #[serde(default)]
    waited_ms: u64,
    #[serde(default)]
    polls: u32,
    #[serde(default)]
    source_gate: bool,
    #[serde(default)]
    module_gate: bool,
    #[serde(default)]
    patch_present: bool,
}

/// 后台每轮的页内等待上限。测试或诊断可用 `window.__codeyRequestGateWaitMs` 再缩短。
const REQUEST_GATE_WAIT_BUDGET: Duration = Duration::from_secs(10);
/// 单次 ensureRequestGate 调用的页内上限：会话工具加载与模块探测各有自己的
/// 预算，卡住的调用不拖累整轮等待的终止判定。
const REQUEST_GATE_POLL_STEP_MS: u64 = 200;
const REQUEST_GATE_POLL_CALL_MS: u64 = 2_000;

// Runtime.evaluate 未开 returnByValue 时只有原始类型回传 value，判定结果
// 必须用 JSON 字符串承载，与注入状态快照同一约定。预算等数值由宿主注入，
// 页面默认值与降级阈值由 request_gate_verify_script() 统一保持同步。
const REQUEST_GATE_VERIFY_SCRIPT_TEMPLATE: &str = r#"(async () => {
  const started = Date.now();
  const hostBudgetMs = __CODEY_GATE_BUDGET_MS__;
  const overrideBudget = Number(window.__codeyRequestGateWaitMs);
  const budgetMs = Number.isFinite(overrideBudget) && overrideBudget >= 0
    ? Math.min(overrideBudget, hostBudgetMs)
    : hostBudgetMs;
  const deadline = started + budgetMs;
  const patch = window.__codeyModelWhitelistPatch;
  const patchPresent = () => !!patch && typeof patch.ensureRequestGate === "function"
    && window.__codeyModelWhitelistPatch === patch;
  let polls = 0;
  let lastMessage = "";
  const verdict = (ready, reinstall, message, budgetExhausted = false) => JSON.stringify({
    ready, reinstall, message,
    patchPresent: patchPresent(), budgetExhausted,
    waitedMs: Date.now() - started,
    polls,
    sourceGate: window.__codeyModelRequestSourceGateInstalled === true,
    moduleGate: window.__codeyModelRequestModuleGateInstalled === true,
  });
  for (;;) {
    if (!patchPresent()) {
      return verdict(false, true, "模型请求补丁缺失或已替换");
    }
    const remaining = deadline - Date.now();
    if (remaining <= 0) {
      return verdict(false, false, lastMessage || "模型请求校验未就绪", true);
    }
    polls += 1;
    const timedOut = Symbol("timeout");
    const outcome = await Promise.race([
      Promise.resolve().then(() => patch.ensureRequestGate()).then(
        (ready) => ({ ready }),
        (error) => ({ error }),
      ),
      new Promise((resolve) => setTimeout(
        () => resolve(timedOut),
        Math.min(__CODEY_GATE_CALL_MS__, remaining),
      )),
    ]);
    // 任一返回路径都要确认补丁仍是本轮检查的实例，包括成功和探测超时。
    if (!patchPresent()) {
      return verdict(false, true, "模型请求补丁缺失或已替换");
    }
    if (outcome === timedOut) {
      // 单次调用超时不覆盖已拿到的真实失败原因——耗尽预算时诊断更需要后者。
      lastMessage = lastMessage || "模型请求校验探测超时";
    } else if (outcome.error !== undefined) {
      lastMessage = String(
        (outcome.error && outcome.error.message) || outcome.error || "模型请求校验失败",
      );
    } else if (outcome.ready === true) {
      return verdict(true, false, "");
    } else {
      lastMessage = "模型请求校验未就绪";
    }
    await new Promise((resolve) => setTimeout(
      resolve,
      Math.min(__CODEY_GATE_STEP_MS__, Math.max(0, deadline - Date.now())),
    ));
  }
})()"#;

fn request_gate_verify_script(wait_budget_ms: u64) -> String {
    REQUEST_GATE_VERIFY_SCRIPT_TEMPLATE
        .replace("__CODEY_GATE_BUDGET_MS__", &wait_budget_ms.to_string())
        .replace(
            "__CODEY_GATE_CALL_MS__",
            &REQUEST_GATE_POLL_CALL_MS.to_string(),
        )
        .replace(
            "__CODEY_GATE_STEP_MS__",
            &REQUEST_GATE_POLL_STEP_MS.to_string(),
        )
}

#[derive(Debug)]
struct RequestGateBudget {
    deadline: tokio::time::Instant,
    call_timeout: Duration,
    wait_ms: u64,
}

impl RequestGateBudget {
    fn with_wait(wait: Duration) -> Self {
        let call_timeout = wait + REQUEST_GATE_CDP_MARGIN;
        Self {
            deadline: tokio::time::Instant::now() + call_timeout,
            call_timeout,
            wait_ms: wait.as_millis() as u64,
        }
    }

    fn settle() -> Self {
        Self::with_wait(REQUEST_GATE_WAIT_BUDGET)
    }

    fn cdp_failure(&self, error: anyhow::Error) -> VerifyFailure {
        // 两层超时可能在同一次轮询中到期，诊断不能依赖定时器的返回顺序。
        let error = if error.is::<tokio::time::error::Elapsed>() {
            error.context(format!(
                "模型请求校验未通过：CDP 调用未在响应预算内完成（callBudgetMs={}；pageBudgetMs={}）",
                self.call_timeout.as_millis(),
                self.wait_ms,
            ))
        } else {
            error.context("校验模型请求入口失败")
        };
        VerifyFailure::reinstall(error)
    }
}

async fn verify_request_gate(
    websocket_url: &str,
    budget: RequestGateBudget,
) -> std::result::Result<RequestGateVerdict, VerifyFailure> {
    let script = request_gate_verify_script(budget.wait_ms);
    // runtime 的命令超时不含连接阶段；这里对握手、发送与响应统一限时，确保
    // 先返回明确的 CDP 失败，再由调用方处理重试。
    let response = tokio::time::timeout_at(
        budget.deadline,
        codey_runtime_core::bridge::evaluate_script_with_await_promise_timeout(
            websocket_url,
            &script,
            true,
            budget.call_timeout,
        ),
    )
    .await
    .map_err(anyhow::Error::from)
    .and_then(|response| response)
    .map_err(|error| budget.cdp_failure(error))?;
    let payload = runtime_value(&response)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            VerifyFailure::reinstall(anyhow::anyhow!("模型请求校验未通过：校验响应不可解析"))
        })?;
    let verdict: RequestGateVerdict = serde_json::from_str(payload).map_err(|error| {
        VerifyFailure::reinstall(anyhow::anyhow!(
            "模型请求校验未通过：校验结果反序列化失败（{error}）"
        ))
    })?;
    if !verdict.patch_present || verdict.polls == 0 || verdict.reinstall {
        let message = verdict
            .message
            .as_deref()
            .filter(|message| !message.is_empty())
            .unwrap_or("未取得有效的模型请求校验结果");
        let error = anyhow::anyhow!("模型请求校验未通过：{message}");
        return Err(if verdict.reinstall || !verdict.patch_present {
            VerifyFailure::reinstall(error)
        } else {
            VerifyFailure::retain(error)
        });
    }
    Ok(verdict)
}

fn record_request_gate_verdict(
    event: &str,
    target_id: &str,
    verdict: &RequestGateVerdict,
    settle_ms: Option<u64>,
) {
    // sourceGate/moduleGate 区分源码补丁、模块级 DI 直取与 React 遍历三条
    // 就绪路径；patchSession 记录补丁服务是否赶在首个文档前挂上该页面，
    // attachedBeforeLoad 为 false 时主 bundle 没有补丁，属于预期内的降级。
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
        event,
        serde_json::json!({
            "targetId": target_id,
            "message": verdict.message.as_deref().filter(|message| !message.is_empty()),
            "waitedMs": verdict.waited_ms,
            "polls": verdict.polls,
            "settleMs": settle_ms,
            "sourceGate": verdict.source_gate,
            "moduleGate": verdict.module_gate,
            "patchSession": crate::renderer_asset_patch::page_stats(target_id),
        }),
    );
}

#[derive(Debug, PartialEq, Eq)]
enum RequestGateSettle {
    Ready,
    Exhausted,
    Abandoned,
}

/// 页面消失或补丁被替换时放弃：重新注入由看门狗负责，并会带来新的后台轮询。
async fn settle_request_gate(
    websocket_url: &str,
) -> (RequestGateSettle, Option<RequestGateVerdict>) {
    let mut last = None;
    for _ in 0..REQUEST_GATE_SETTLE_ROUNDS {
        match verify_request_gate(websocket_url, RequestGateBudget::settle()).await {
            Ok(verdict) if verdict.ready => return (RequestGateSettle::Ready, Some(verdict)),
            Ok(verdict) => last = Some(verdict),
            Err(_) => return (RequestGateSettle::Abandoned, None),
        }
    }
    (RequestGateSettle::Exhausted, last)
}

fn spawn_request_gate_settle(target_id: String, websocket_url: Arc<str>) {
    tokio::spawn(async move {
        let started = tokio::time::Instant::now();
        let (settle, verdict) = settle_request_gate(&websocket_url).await;
        let event = match settle {
            RequestGateSettle::Ready => "cdp.request_gate_ready",
            RequestGateSettle::Exhausted => "cdp.request_gate_degraded",
            RequestGateSettle::Abandoned => return,
        };
        if let Some(verdict) = verdict {
            record_request_gate_verdict(
                event,
                &target_id,
                &verdict,
                Some(started.elapsed().as_millis() as u64),
            );
        }
    });
}

enum AttemptOutcome {
    Injected {
        target: InjectedTarget,
        readiness: serde_json::Value,
    },
    Retry {
        error: anyhow::Error,
        retained: Option<EstablishedInjection>,
    },
}

async fn run_injection_attempt(
    retained: Option<EstablishedInjection>,
    prefetched_target: Option<CdpTarget>,
    debug_port: u16,
    handler: BridgeHandler,
    scripts: &PreparedInjectionScripts,
    phase: &AtomicU8,
    deadline: tokio::time::Instant,
) -> AttemptOutcome {
    let mut bridge = match retained.filter(|bridge| !bridge.pump.is_finished()) {
        Some(bridge) => bridge,
        // 没有可复用的桥接（首轮，或上一轮留下的桥接泵已随页面导航/目标关闭
        // 而结束）时回到发现阶段整轮重建。
        None => {
            match establish_injection(debug_port, prefetched_target, handler, scripts, phase).await
            {
                Ok(bridge) => bridge,
                Err(error) => {
                    return AttemptOutcome::Retry {
                        error,
                        retained: None,
                    };
                }
            }
        }
    };
    match verify_injection(&mut bridge, phase, deadline).await {
        Ok(probe) => {
            phase.store(InjectionPhase::ReadStatuses as u8, Ordering::Release);
            // 状态探测只用于诊断，不进启动关键路径：桥接与浮层已证明注入可用，
            // 真实状态由界面请求时的 refresh_injection_statuses 补齐。
            AttemptOutcome::Injected {
                target: bridge.into_injected(scripts.statuses_with_error("启动后复核注入状态")),
                readiness: probe.summary(),
            }
        }
        Err(failure) if failure.reinstall => {
            // 确认端明确要求重建：显式关掉这条会话（其文档脚本随会话注销），
            // 下一轮从头装桥。
            if deadline.saturating_duration_since(tokio::time::Instant::now()) >= VERIFY_RESERVED {
                bridge.pump.close().await;
            } else {
                // 临近整轮截止时直接释放泵，避免关闭握手吞掉已取得的确认错误。
                drop(bridge.pump);
            }
            AttemptOutcome::Retry {
                error: failure.error,
                retained: None,
            }
        }
        Err(failure) => AttemptOutcome::Retry {
            error: failure.error,
            retained: Some(bridge),
        },
    }
}

async fn establish_injection(
    debug_port: u16,
    prefetched_target: Option<CdpTarget>,
    handler: BridgeHandler,
    scripts: &PreparedInjectionScripts,
    phase: &AtomicU8,
) -> Result<EstablishedInjection> {
    // 补丁服务通常在 Codex 启动前已拉起；看门狗重注入或 Codex 自行重启后
    // 由这里补上。服务独立于桥接，失败不阻断注入。
    crate::renderer_asset_patch::ensure_service(debug_port, scripts.disable_pet);
    phase.store(InjectionPhase::DiscoverTargets as u8, Ordering::Release);
    // 握手窗口内已锁定目标：两个发现阶段视作已过，直接建连。
    let target = match prefetched_target {
        Some(target) => {
            phase.store(InjectionPhase::SelectTarget as u8, Ordering::Release);
            target
        }
        None => {
            let targets = list_targets(debug_port)
                .await
                .context("枚举 Codex CDP 页面失败")?;
            phase.store(InjectionPhase::SelectTarget as u8, Ordering::Release);
            pick_injectable_codex_page_target(&targets).with_context(|| {
                format!(
                    "没有找到可注入的 Codex renderer；CDP targets={}",
                    summarize_cdp_targets(&targets)
                )
            })?
        }
    };
    let websocket_url: Arc<str> = Arc::from(
        target
            .web_socket_debugger_url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Codex 页面没有 CDP WebSocket 地址"))?,
    );
    let handler = with_lazy_loaders(handler, websocket_url.clone());
    phase.store(InjectionPhase::InstallBridge as u8, Ordering::Release);
    let (pump, install_probe) = install_bridge_with_options(
        &websocket_url,
        codey_runtime_core::bridge::BRIDGE_BINDING_NAME,
        handler,
        &scripts.scripts,
        BridgeInstallOptions {
            readiness_probe: Some(READINESS_PROBE_SCRIPT),
        },
    )
    .await
    .with_context(|| format!("向 Codex renderer {} 安装 CDP bridge 失败", target.id))?;
    Ok(EstablishedInjection {
        target_id: target.id,
        websocket_url,
        pump,
        install_probe,
    })
}

/// 启动只确认三件事：桥接与浮层在当前文档可用、页面是主窗口。请求入口门禁
/// 等 Codex 自建 request client，交给后台轮询，不参与注入成败。
async fn verify_injection(
    bridge: &mut EstablishedInjection,
    phase: &AtomicU8,
    deadline: tokio::time::Instant,
) -> std::result::Result<ReadinessProbe, VerifyFailure> {
    phase.store(InjectionPhase::VerifyReadiness as u8, Ordering::Release);
    let response = match bridge.install_probe.take() {
        Some(response) => response,
        None => probe_readiness(&bridge.websocket_url, deadline)
            .await
            .map_err(|error| VerifyFailure::retain(error.context("确认页面注入就绪失败")))?,
    };
    let probe = ReadinessProbe::from_response(&response).map_err(VerifyFailure::retain)?;
    judge_readiness(probe)
}

/// 独立连接上的就绪探针，预算受整轮截止时间约束。超时说明页面忙或仍在
/// 导航，调用方保留桥接稍后重探，不会因此重装。
async fn probe_readiness(
    websocket_url: &str,
    deadline: tokio::time::Instant,
) -> Result<serde_json::Value> {
    let budget = deadline
        .saturating_duration_since(tokio::time::Instant::now())
        .saturating_sub(INJECTION_FINISH_MARGIN)
        .min(READINESS_PROBE_TIMEOUT);
    anyhow::ensure!(
        budget >= READINESS_PROBE_MIN_BUDGET,
        "剩余注入预算不足以完成就绪确认（remainingMs={}）",
        budget.as_millis()
    );
    tokio::time::timeout(
        budget,
        codey_runtime_core::bridge::evaluate_script_with_await_promise_timeout(
            websocket_url,
            READINESS_PROBE_SCRIPT,
            false,
            budget,
        ),
    )
    .await
    .map_err(anyhow::Error::from)
    .and_then(|response| response)
}

impl PreparedInjectionScripts {
    pub fn statuses_with_error(&self, error: impl Into<String>) -> Arc<[InjectionScriptStatus]> {
        let error: String = error.into();
        let error = truncate_chars(&error, MAX_INJECTION_ERROR_CHARS);
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

pub async fn reload_mcp_servers(websocket_url: &str) -> Result<()> {
    // 会话工具是按需注入的。若在已阻塞的 awaitPromise 里再走页面桥接去
    // Runtime.evaluate 注入脚本，部分 Chromium/Electron 会把后续 evaluate 排
    // 到当前 Promise 之后，形成死等。这里先用独立命令装好刷新入口。
    ensure_mcp_reload_function_ready(websocket_url).await;
    let response = codey_runtime_core::bridge::evaluate_script_with_await_promise_timeout(
        websocket_url,
        r#"(async () => {
  const pickClient = () => {
    const clients = window.__codeyAppServerRequestClients;
    if (!clients || typeof clients.get !== "function") return null;
    const local = clients.get("local");
    if (local && typeof local.sendRequest === "function") return local;
    if (typeof clients.values !== "function") return null;
    const all = [...clients.values()].filter((client) => typeof client?.sendRequest === "function");
    return all.length === 1 ? all[0] : null;
  };
  for (const delay of [0, 200, 500, 1000, 2000]) {
    if (delay > 0) {
      await new Promise((resolve) => window.setTimeout(resolve, delay));
    }
    try {
      const client = pickClient();
      if (client) {
        await client.sendRequest("config/mcpServer/reload", {});
        return true;
      }
      if (typeof window.__codeyReloadMcpServers === "function") {
        const result = await window.__codeyReloadMcpServers();
        if (result?.ok === true) return true;
      }
    } catch (error) {
      const message = String(error?.message || error);
      if (/unknown method/i.test(message)) return false;
    }
  }
  return false;
})()"#,
        true,
        MCP_RELOAD_EVALUATE_TIMEOUT,
    )
    .await
    .context("请求 Codex 刷新 MCP 配置失败")?;
    anyhow::ensure!(
        runtime_value(&response).and_then(serde_json::Value::as_bool) == Some(true),
        "Codex 未确认 MCP 配置刷新"
    );
    Ok(())
}

async fn mcp_reload_function_available(websocket_url: &str) -> bool {
    codey_runtime_core::bridge::evaluate_script(
        websocket_url,
        r#"typeof window.__codeyReloadMcpServers === "function"
          || typeof window.__codeyAppServerRequestClients?.get?.("local")?.sendRequest === "function""#,
    )
    .await
    .ok()
    .and_then(|response| runtime_value(&response).and_then(serde_json::Value::as_bool))
    .unwrap_or(false)
}

async fn ensure_mcp_reload_function_ready(websocket_url: &str) {
    if mcp_reload_function_available(websocket_url).await {
        return;
    }
    let _ = codey_runtime_core::bridge::evaluate_script_with_await_promise_timeout(
        websocket_url,
        &prepared_session_tools_load_script(),
        false,
        SESSION_TOOLS_INJECT_TIMEOUT,
    )
    .await;
    for delay_ms in [80_u64, 200, 500, 1000] {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        if mcp_reload_function_available(websocket_url).await {
            return;
        }
    }
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
  const catalogAccepted = (delivery) => (
    delivery?.responsePatchInstalled === true
    && Number(delivery.statsigClients) > 0
    && Number(delivery.notifiedClients) > 0
    && Number(delivery.pendingQueryEntries || 0) === 0
  );
  const reachedActiveModelPicker = (delivery) => (
    catalogAccepted(delivery)
    && Number(delivery.queryClients) > 0
    && Number(delivery.queryEntries) > 0
  );
  const deliverySummary = (delivery) => delivery
    ? `（statsigClients=${{Number(delivery.statsigClients)}}, notifiedClients=${{Number(delivery.notifiedClients)}}, queryClients=${{Number(delivery.queryClients)}}, queryEntries=${{Number(delivery.queryEntries)}}, pendingQueryEntries=${{Number(delivery.pendingQueryEntries || 0)}}）`
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
      }} else if (Number(delivery.pendingQueryEntries) > 0) {{
        lastError = "Codex 已有模型查询缓存尚未更新";
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
                        .map(|detail| truncate_chars(&detail, MAX_INJECTION_ERROR_CHARS))
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
                        error.map(|error| truncate_chars(&error, MAX_INJECTION_ERROR_CHARS))
                    } else {
                        Some("脚本返回了未知注入状态".to_string())
                    },
                }
            })
            .collect::<Vec<_>>(),
    )
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
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
    /// 探测连接可用，但页面端点没有在 CDP 命令预算内产生任何回应。
    /// 页面忙碌时命令仍会被 renderer 排队执行并回包，完全不回应只出现在
    /// renderer 已被回收或页面重载导致 page target 被替换的情况。
    Unresponsive,
}

fn target_health_from_evaluate_response(response: &serde_json::Value) -> TargetHealth {
    match runtime_value(response).and_then(serde_json::Value::as_str) {
        Some("healthy") => TargetHealth::Healthy,
        Some("busy") => TargetHealth::Busy,
        _ => TargetHealth::Unhealthy,
    }
}

pub async fn is_target_healthy(websocket_url: &str) -> Result<TargetHealth> {
    match codey_runtime_core::bridge::evaluate_script_with_await_promise(
        websocket_url,
        bridge_health_check_script(),
        true,
    )
    .await
    {
        Ok(result) => Ok(target_health_from_evaluate_response(&result)),
        Err(error) => {
            // 探测每次都新建 CDP 连接。命令超时意味着端点收下了连接却不执行
            // 命令，与"页面忙而桥接仍在"是两种状态：前者只能重新发现 target，
            // 后者保守等待。这里把超时单独标出来，交给看门狗分级处理。
            if error
                .downcast_ref::<tokio::time::error::Elapsed>()
                .is_some()
            {
                return Ok(TargetHealth::Unresponsive);
            }
            Err(error).context("检查 Codey bridge 健康状态失败")
        }
    }
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
    fn prefetch_handoff_leaves_time_for_injection() {
        let started = tokio::time::Instant::now();
        let deadline = started + CDP_INJECTION_TIMEOUT;
        let handoff = prefetch_handoff_deadline(started, deadline);
        assert_eq!(handoff - started, Duration::from_secs(8));
        let tight = started + Duration::from_secs(5);
        assert!(tight - prefetch_handoff_deadline(started, tight) >= MINIMUM_VERIFICATION_BUDGET);
    }

    #[tokio::test]
    async fn mcp_reload_requires_a_confirmed_renderer_response() {
        use futures_util::{SinkExt, StreamExt};
        use serde_json::json;

        for (payload, succeeds) in [
            (json!({"result":{"type":"boolean","value":true}}), true),
            (json!({"result":{"type":"boolean","value":false}}), false),
            (json!({"exceptionDetails":{"text":"Unknown method"}}), false),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                    let mut finished = false;
                    while let Some(message) = socket.next().await {
                        let Ok(message) = message else {
                            break;
                        };
                        let Ok(text) = message.to_text() else {
                            continue;
                        };
                        let request: serde_json::Value = serde_json::from_str(text).unwrap();
                        let evaluate = request["method"] == "Runtime.evaluate";
                        let result = if evaluate {
                            let expression = request["params"]["expression"].as_str().unwrap();
                            let await_promise = request["params"]["awaitPromise"] == true;
                            if await_promise {
                                assert!(expression.contains("__codeyReloadMcpServers"));
                                assert!(
                                    expression.contains("__codeyAppServerRequestClients"),
                                    "MCP reload must use the patched AppServerRequestClient before fiber discovery"
                                );
                                assert!(
                                    !expression.contains("__codeyLoadSessionTools"),
                                    "MCP reload must not nest session-tools loading inside awaitPromise"
                                );
                                payload.clone()
                            } else if expression
                                .contains("typeof window.__codeyReloadMcpServers === \"function\"")
                            {
                                json!({"result":{"type":"boolean","value":true}})
                            } else {
                                json!({"result":{"type":"string","value":""}})
                            }
                        } else {
                            json!({})
                        };
                        if socket
                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                json!({"id":request["id"],"result":result})
                                    .to_string()
                                    .into(),
                            ))
                            .await
                            .is_err()
                        {
                            break;
                        }
                        if evaluate && request["params"]["awaitPromise"] == true {
                            finished = true;
                            break;
                        }
                    }
                    if finished {
                        break;
                    }
                }
            });
            assert_eq!(
                reload_mcp_servers(&format!("ws://{address}")).await.is_ok(),
                succeeds
            );
            server.await.unwrap();
        }
    }

    #[test]
    fn user_scripts_run_once_per_document_and_retry_after_failure() {
        let scripts = prepare_injection_scripts(
            false,
            false,
            false,
            &["window.attempts = (window.attempts || 0) + 1; if (window.fail) throw new Error('retry');".to_string()],
        );
        let harness = r#"
const assert = require('node:assert/strict');
const vm = require('node:vm');
const script = JSON.parse(process.argv[1]);
const context = () => vm.createContext({ window: {}, console: { error() {} } });
const page = context();
page.window.fail = true;
vm.runInContext(script, page);
assert.equal(page.window.__codeyInjectionStatus['user-script-1'].status, 'failed');
page.window.fail = false;
vm.runInContext(script, page);
vm.runInContext(script, page);
assert.equal(page.window.attempts, 2);
assert.equal(page.window.__codeyInjectionStatus['user-script-1'].status, 'executed');
const nextPage = context();
vm.runInContext(script, nextPage);
vm.runInContext(script, nextPage);
assert.equal(nextPage.window.attempts, 1);
"#;
        let output = std::process::Command::new("node")
            .args([
                "-e",
                harness,
                &serde_json::to_string(&scripts.scripts[1]).unwrap(),
            ])
            .output()
            .expect("Node should run the generated user script");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn injection_deadline_leaves_time_for_slow_windows_renderer_startup() {
        assert_eq!(CDP_INJECTION_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn request_gate_verdict_parses_poll_and_reinstall_shapes() {
        let poll: RequestGateVerdict = serde_json::from_str(
            r#"{"ready":false,"reinstall":false,"message":"当前 Codex 暂不支持模型请求校验"}"#,
        )
        .unwrap();
        assert!(!poll.ready && !poll.reinstall);
        assert_eq!(
            poll.message.as_deref(),
            Some("当前 Codex 暂不支持模型请求校验")
        );
        // 旧页面可能只回 ready/message，reinstall 缺省按轮询处理。
        let legacy: RequestGateVerdict =
            serde_json::from_str(r#"{"ready":false,"message":"x"}"#).unwrap();
        assert!(!legacy.ready && !legacy.reinstall);
        assert!(
            serde_json::from_str::<RequestGateVerdict>(r#"{"ready":true}"#)
                .unwrap()
                .ready
        );
    }

    #[test]
    fn endpoint_unreachable_only_when_no_attempt_reached_the_bridge() {
        let failure = |phase: InjectionPhase, reached_bridge_stage: u32| InjectionRetryFailure {
            error: anyhow::anyhow!("x"),
            detail: None,
            final_phase: phase,
            reached_bridge_stage,
        };
        // 调试端口死掉：每轮都停在发现阶段。
        assert!(failure(InjectionPhase::DiscoverTargets, 0).debug_endpoint_unreachable());
        assert!(failure(InjectionPhase::SelectTarget, 0).debug_endpoint_unreachable());
        // 有一轮摸到过桥接阶段——端点当时活着，不算托管外重启。
        assert!(!failure(InjectionPhase::DiscoverTargets, 1).debug_endpoint_unreachable());
        // 最后一轮卡在桥接安装（端点还活着只是会话坏了）同样不适用。
        assert!(!failure(InjectionPhase::InstallBridge, 1).debug_endpoint_unreachable());
        assert!(!failure(InjectionPhase::VerifyReadiness, 2).debug_endpoint_unreachable());
    }

    async fn mock_request_gate(
        reply: Option<serde_json::Value>,
        stall_handshake: bool,
    ) -> (String, tokio::task::JoinHandle<u64>) {
        let (url, server) = mock_request_gate_sequence(vec![reply], stall_handshake).await;
        (
            url,
            tokio::spawn(async move { server.await.unwrap().pop().unwrap() }),
        )
    }

    /// 每个回复对应一次独立连接；`None` 表示接受校验后不再应答。
    async fn mock_request_gate_sequence(
        replies: Vec<Option<serde_json::Value>>,
        stall_handshake: bool,
    ) -> (String, tokio::task::JoinHandle<Vec<u64>>) {
        use futures_util::{SinkExt, StreamExt};
        use serde_json::json;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut budgets = Vec::new();
            for reply in replies {
                let (stream, _) = listener.accept().await.unwrap();
                if stall_handshake {
                    std::future::pending::<()>().await;
                }
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let message = socket.next().await.unwrap().unwrap();
                let request: serde_json::Value =
                    serde_json::from_str(message.to_text().unwrap()).unwrap();
                assert_eq!(request["method"], "Runtime.evaluate");
                assert_eq!(request["params"]["awaitPromise"], true);
                let script = request["params"]["expression"].as_str().unwrap();
                budgets.push(
                    script
                        .split_once("const hostBudgetMs = ")
                        .unwrap()
                        .1
                        .split_once(';')
                        .unwrap()
                        .0
                        .parse()
                        .unwrap(),
                );
                let Some(reply) = reply else {
                    std::future::pending::<()>().await;
                    drop(socket);
                    return budgets;
                };
                let value = if reply.is_object() {
                    json!(reply.to_string())
                } else {
                    reply
                };
                socket
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        json!({"id":request["id"],"result":{"result":{"value":value}}})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
            }
            budgets
        });
        (format!("ws://{address}"), server)
    }

    const TEST_GATE_WAIT: Duration = Duration::from_millis(200);

    #[test]
    fn background_gate_rounds_are_bounded_by_the_page_budget() {
        let settle = RequestGateBudget::settle();
        assert_eq!(settle.wait_ms, REQUEST_GATE_WAIT_BUDGET.as_millis() as u64);
        assert_eq!(
            settle.call_timeout,
            REQUEST_GATE_WAIT_BUDGET + REQUEST_GATE_CDP_MARGIN
        );
    }

    #[tokio::test]
    async fn request_gate_settles_in_background_until_ready_or_abandoned() {
        use serde_json::json;
        let not_ready = json!({"ready":false,"patchPresent":true,"polls":3,"budgetExhausted":true});
        let ready = json!({"ready":true,"patchPresent":true,"polls":1});
        let (url, server) =
            mock_request_gate_sequence(vec![Some(not_ready.clone()), Some(ready)], false).await;
        assert_eq!(settle_request_gate(&url).await.0, RequestGateSettle::Ready);
        assert_eq!(
            server.await.unwrap(),
            vec![REQUEST_GATE_WAIT_BUDGET.as_millis() as u64; 2]
        );

        let (url, server) = mock_request_gate_sequence(
            vec![Some(not_ready.clone()); REQUEST_GATE_SETTLE_ROUNDS as usize],
            false,
        )
        .await;
        assert_eq!(
            settle_request_gate(&url).await.0,
            RequestGateSettle::Exhausted
        );
        server.await.unwrap();

        // 补丁被替换说明页面已重载，重新注入归看门狗，后台不再轮询。
        let (url, server) = mock_request_gate_sequence(
            vec![Some(
                json!({"ready":false,"patchPresent":false,"polls":1,"reinstall":true}),
            )],
            false,
        )
        .await;
        assert_eq!(
            settle_request_gate(&url).await.0,
            RequestGateSettle::Abandoned
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn request_gate_ready_is_immediate_and_invalid_verdicts_require_retry() {
        use serde_json::json;
        for (reply, expected_success) in [
            (json!({"ready":true,"patchPresent":true,"polls":1}), true),
            (json!({"ready":false,"patchPresent":true,"polls":1}), true),
            (
                json!({"ready":false,"patchPresent":false,"polls":1,"budgetExhausted":true}),
                false,
            ),
            (
                json!({"ready":true,"patchPresent":true,"polls":1,"reinstall":true}),
                false,
            ),
            (
                json!({"ready":false,"patchPresent":true,"polls":0,"budgetExhausted":true}),
                false,
            ),
            (json!(true), false),
            (json!("invalid JSON"), false),
        ] {
            let (url, server) = mock_request_gate(Some(reply), false).await;
            let budget = RequestGateBudget::with_wait(TEST_GATE_WAIT);
            let result =
                tokio::time::timeout(Duration::from_secs(1), verify_request_gate(&url, budget))
                    .await
                    .expect("an immediate renderer response must not wait for the gate budget");
            assert_eq!(result.is_ok(), expected_success, "{result:?}");
            assert_eq!(server.await.unwrap(), TEST_GATE_WAIT.as_millis() as u64);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn request_gate_classifies_both_timeout_layers_without_losing_the_cause() {
        let budget = RequestGateBudget::with_wait(TEST_GATE_WAIT);
        for inner_timeout in [false, true] {
            let elapsed = tokio::time::timeout(Duration::ZERO, std::future::pending::<()>())
                .await
                .unwrap_err();
            let mut error = anyhow::Error::from(elapsed);
            if inner_timeout {
                error = error.context("timed out waiting for CDP command Runtime.evaluate");
            }
            let failure = budget.cdp_failure(error);
            assert!(failure.reinstall);
            assert!(failure.error.is::<tokio::time::error::Elapsed>());
            let detail = failure.error.to_string();
            assert!(detail.contains("CDP 调用未在响应预算内完成"), "{detail}");
            assert!(
                detail.contains("callBudgetMs=1200；pageBudgetMs=200"),
                "{detail}"
            );
        }

        let failure = budget.cdp_failure(anyhow::anyhow!("connection refused"));
        assert!(failure.reinstall);
        assert_eq!(failure.error.to_string(), "校验模型请求入口失败");
        assert_eq!(failure.error.root_cause().to_string(), "connection refused");
    }

    #[tokio::test]
    async fn request_gate_unresponsive_cdp_never_degrades_including_handshake() {
        for stall_handshake in [false, true] {
            let (url, server) = mock_request_gate(None, stall_handshake).await;
            let failure = tokio::time::timeout(
                Duration::from_secs(3),
                verify_request_gate(&url, RequestGateBudget::with_wait(TEST_GATE_WAIT)),
            )
            .await
            .expect("the whole CDP call must finish before the injection deadline")
            .unwrap_err();
            server.abort();
            assert!(failure.reinstall);
            let detail = failure.error.to_string();
            assert!(detail.contains("CDP 调用未在响应预算内完成"), "{detail}");
            assert!(detail.contains("pageBudgetMs=200"), "{detail}");
        }
    }

    fn probe(href: &str, ready: bool) -> ReadinessProbe {
        ReadinessProbe {
            href: href.to_string(),
            bridge: ready,
            overlay: ready,
            model_patch: ready,
            overlay_error: String::new(),
        }
    }

    #[test]
    fn readiness_accepts_only_a_ready_main_window() {
        let ready = judge_readiness(probe("app://-/index.html", true)).unwrap();
        assert_eq!(
            ready.summary(),
            serde_json::json!({"url": "app://-/index.html", "modelPatch": true})
        );
        // 请求入口补丁缺失只影响增强，不阻断注入。
        let mut without_patch = probe("app://-/index.html?initialRoute=%2Flocal%2Fx", true);
        without_patch.model_patch = false;
        assert!(judge_readiness(without_patch).is_ok());

        for href in [
            "app://-/detached-window.html?initialRoute=%2Fdetached-window",
            "app://-/index.html?initialRoute=%2Fhotkey-window",
        ] {
            let failure = judge_readiness(probe(href, true)).unwrap_err();
            assert!(failure.reinstall, "{href}");
            assert_eq!(
                safe_injection_error_summary(&failure.error),
                "注入目标是 Codex 辅助窗口，已改选主窗口"
            );
        }

        // 首次导航尚未提交：保留桥接等待，不重装。
        for (href, ready) in [("about:blank", true), ("", false), ("ABOUT:BLANK", false)] {
            let failure = judge_readiness(probe(href, ready)).unwrap_err();
            assert!(!failure.reinstall, "{href}");
            assert_eq!(
                safe_injection_error_summary(&failure.error),
                "Codex 页面尚未完成首次导航"
            );
        }

        let mut missing_overlay = probe("app://-/index.html", true);
        missing_overlay.overlay = false;
        missing_overlay.overlay_error = "TypeError: boom".to_string();
        let failure = judge_readiness(missing_overlay).unwrap_err();
        assert!(failure.reinstall);
        assert!(failure.error.to_string().contains("TypeError: boom"));
        assert_eq!(
            safe_injection_error_summary(&failure.error),
            "Codey 浮层未就绪"
        );
    }

    #[test]
    fn readiness_probe_script_reports_page_state_synchronously() {
        let harness = r#"
const assert = require('node:assert/strict');
const vm = require('node:vm');
const script = JSON.parse(process.argv[1]);
const run = (window, href) => JSON.parse(vm.runInContext(script, vm.createContext({
  window, location: { href },
})));
const empty = run({}, "about:blank");
assert.deepEqual(empty, {
  href: "about:blank", bridge: false, overlay: false, modelPatch: false, overlayError: "",
});
const ready = run({
  __codexSessionDeleteBridge() {},
  __codeySettingsOverlay: { toggle() {} },
  __codeyModelWhitelistPatch: { ensureRequestGate() {} },
  __codeyOverlayError: "x".repeat(400),
}, "app://-/index.html");
assert.equal(ready.bridge && ready.overlay && ready.modelPatch, true);
assert.equal(ready.overlayError.length, 300);
assert.equal(run({ __codeySettingsOverlay: null }, "app://-/index.html").overlay, false);
"#;
        let output = std::process::Command::new("node")
            .args([
                "-e",
                harness,
                &serde_json::to_string(READINESS_PROBE_SCRIPT).unwrap(),
            ])
            .output()
            .expect("Node should run the readiness probe harness");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn readiness_probe_reads_one_synchronous_evaluate() {
        use futures_util::{SinkExt, StreamExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let message = socket.next().await.unwrap().unwrap();
            let request: serde_json::Value =
                serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(request["method"], "Runtime.evaluate");
            assert_eq!(request["params"]["awaitPromise"], false);
            assert_eq!(request["params"]["expression"], READINESS_PROBE_SCRIPT);
            let value = serde_json::json!({
                "href": "app://-/index.html", "bridge": true, "overlay": true,
            })
            .to_string();
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    serde_json::json!({"id": request["id"], "result": {"result": {"value": value}}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        });
        let response = probe_readiness(
            &format!("ws://{address}"),
            tokio::time::Instant::now() + CDP_INJECTION_TIMEOUT,
        )
        .await
        .unwrap();
        server.await.unwrap();
        let probe = judge_readiness(ReadinessProbe::from_response(&response).unwrap()).unwrap();
        assert!(!probe.model_patch);
    }

    #[tokio::test]
    async fn readiness_probe_with_insufficient_budget_fails_before_connecting() {
        let error = probe_readiness(
            "ws://127.0.0.1:9/devtools/page/never",
            tokio::time::Instant::now() + READINESS_PROBE_MIN_BUDGET,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("剩余注入预算不足"), "{error:#}");
        let detail = injection_deadline_detail(
            InjectionPhase::VerifyReadiness,
            CDP_INJECTION_TIMEOUT,
            CDP_INJECTION_TIMEOUT,
            Duration::ZERO,
        );
        assert!(detail.contains("phase=verifyReadiness"));
        assert!(detail.contains("attemptMs=30000"));
        assert!(detail.contains("remainingMs=0"));
    }

    #[test]
    fn request_gate_verify_script_marks_missing_patch_for_reinstall() {
        let harness = r#"
const assert = require('node:assert/strict');
const vm = require('node:vm');
const script = JSON.parse(process.argv[1]);
const hostBudgetMs = Number(process.argv[2]);
const run = async (patch, waitMs = 40, onPending = () => {}) => {
  // 只在触发下一个定时器时推进时钟，避免 CI 调度延迟耗尽测试预算。
  let now = 0;
  const timers = [];
  const context = vm.createContext({
    window: {}, console, Date: { now: () => now },
    setTimeout(callback, delay) { timers.push({ at: now + delay, callback }); },
  });
  context.window.__codeyRequestGateWaitMs = waitMs;
  context.window.__codeyModelWhitelistPatch = patch;
  let settled = false;
  let result;
  let failure;
  const pending = vm.runInContext(script, context);
  onPending(context);
  Promise.resolve(pending).then(
    (value) => { result = JSON.parse(value); settled = true; },
    (error) => { failure = error; settled = true; },
  );
  while (!settled) {
    // 先让所有 Promise 回调完成，再运行最近的定时器。
    await new Promise(setImmediate);
    if (settled) break;
    timers.sort((left, right) => left.at - right.at);
    const timer = timers.shift();
    assert.ok(timer, 'request-gate verification stalled without a timer');
    now = timer.at;
    timer.callback();
  }
  if (failure) throw failure;
  return result;
};
(async () => {
  // 补丁未注入时，即使等待预算已耗尽，也必须要求重装。
  for (const waitMs of [0, 40]) {
    for (const patch of [null, undefined, {}]) {
      const verdict = await run(patch, waitMs);
      assert.equal(verdict.ready, false);
      assert.equal(verdict.reinstall, true);
      assert.equal(verdict.polls, 0);
      assert.equal(verdict.patchPresent, false);
    }
  }
  const ready = await run({ ensureRequestGate: () => Promise.resolve(true) });
  assert.equal(ready.ready, true);
  assert.equal(ready.polls, 1);
  assert.equal(ready.waitedMs, 0);
  assert.equal(ready.patchPresent, true);
  assert.equal(ready.budgetExhausted, false);
  // 门禁在等待窗口内就绪：页内检出即返回，不必等宿主下一轮探测。
  let calls = 0;
  const lateReady = await run({
    ensureRequestGate: () => Promise.resolve(++calls >= 2),
  }, 5_000);
  assert.equal(lateReady.ready, true);
  assert.equal(lateReady.polls, 2);
  // Codex 尚未就绪：页内等满预算后交回不就绪裁决，由宿主决定重试或降级。
  const notReady = { ensureRequestGate: () => Promise.reject(new Error("当前 Codex 暂不支持模型请求校验")) };
  const polled = await run(notReady);
  assert.equal(polled.ready, false);
  assert.equal(polled.reinstall, false);
  assert.equal(polled.message, "当前 Codex 暂不支持模型请求校验");
  assert.equal(polled.waitedMs, 40);
  assert.equal(polled.budgetExhausted, true);
  // 页面配置只能缩短等待，不能延长宿主为本轮分配的预算。
  for (const override of [hostBudgetMs * 10, undefined, NaN, -1]) {
    const capped = await run(notReady, override);
    assert.ok(capped.waitedMs <= hostBudgetMs);
  }
  const capped = await run(notReady, hostBudgetMs * 10);
  assert.equal(capped.waitedMs, hostBudgetMs);
  const notAttempted = await run(notReady, 0);
  assert.equal(notAttempted.polls, 0);
  // 调用一直挂起时，也必须在预算耗尽后返回。
  const timedOut = await run({ ensureRequestGate: () => new Promise(() => {}) });
  assert.equal(timedOut.ready, false);
  assert.equal(timedOut.reinstall, false);
  assert.equal(timedOut.waitedMs, 40);
  assert.equal(timedOut.message, "模型请求校验探测超时");
  // 补丁在成功、失败或超时返回前被替换，都不能降级或误报就绪。
  for (const ensureRequestGate of [
    () => Promise.reject(new Error("模型请求校验已重新加载")),
    () => Promise.resolve(true),
    () => Promise.resolve(false),
    () => new Promise(() => {}),
  ]) {
    const replaced = await run({ ensureRequestGate }, 40, (context) => {
      context.window.__codeyModelWhitelistPatch = { ensureRequestGate: () => Promise.resolve(true) };
    });
    assert.equal(replaced.reinstall, true);
    assert.equal(replaced.ready, false);
    assert.equal(replaced.patchPresent, false);
  }
})().catch((error) => { console.error(error); process.exit(1); });
"#;
        let output = std::process::Command::new("node")
            .args([
                "-e",
                harness,
                &serde_json::to_string(&request_gate_verify_script(1_000)).unwrap(),
                "1000",
            ])
            .output()
            .expect("Node should run the request-gate verify script harness");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// 按给定阶段的退避节奏推导探测时刻（毫秒），首项为立即执行的第一次探测。
    fn probe_times(phase: InjectionPhase, count: usize) -> Vec<u64> {
        let mut delay = INITIAL_RETRY_INTERVAL;
        let mut elapsed = 0u64;
        let mut probes = vec![0];
        for _ in 0..count {
            elapsed += delay.as_millis() as u64;
            probes.push(elapsed);
            delay = next_injection_retry_delay(delay, phase, Duration::from_millis(1));
        }
        probes
    }

    #[test]
    fn discovery_retry_stays_dense_while_probes_stay_cheap() {
        assert_eq!(
            probe_times(InjectionPhase::DiscoverTargets, 5),
            vec![0, 100, 250, 450, 650, 850]
        );
        assert_eq!(
            probe_times(InjectionPhase::SelectTarget, 5),
            vec![0, 100, 250, 450, 650, 850]
        );
    }

    #[test]
    fn discovery_retry_never_exceeds_its_dense_ceiling() {
        assert_eq!(
            next_injection_retry_delay(
                Duration::from_millis(500),
                InjectionPhase::SelectTarget,
                Duration::from_millis(1)
            ),
            DISCOVERY_RETRY_MAX_INTERVAL
        );
    }

    #[test]
    fn discovery_retry_falls_back_when_a_probe_stops_being_cheap() {
        // 廉价失败：维持密集节奏。
        assert_eq!(
            next_injection_retry_delay(
                INITIAL_RETRY_INTERVAL,
                InjectionPhase::DiscoverTargets,
                Duration::from_millis(8)
            ),
            Duration::from_millis(150)
        );
        // 同阶段但单次探测已花掉 900 ms，说明失败不再廉价（例如端口被别的
        // 服务占用导致请求挂起），立刻退回指数退避而不是继续密集轮询。
        assert_eq!(
            next_injection_retry_delay(
                Duration::from_millis(200),
                InjectionPhase::DiscoverTargets,
                Duration::from_millis(900)
            ),
            Duration::from_millis(400)
        );
    }

    #[test]
    fn bridge_retry_keeps_the_original_exponential_backoff() {
        assert_eq!(
            probe_times(InjectionPhase::InstallBridge, 6),
            vec![0, 100, 300, 700, 1200, 1700, 2200]
        );
    }

    #[test]
    fn post_discovery_phases_ignore_the_dense_interval() {
        for phase in [
            InjectionPhase::InstallBridge,
            InjectionPhase::VerifyReadiness,
            InjectionPhase::ReadStatuses,
        ] {
            assert!(!phase.is_discovery());
            assert_eq!(
                next_injection_retry_delay(INITIAL_RETRY_INTERVAL, phase, Duration::from_millis(1)),
                Duration::from_millis(200)
            );
        }
    }

    #[test]
    fn dense_discovery_retry_probes_a_late_renderer_sooner() {
        let legacy = probe_times(InjectionPhase::InstallBridge, 6);
        let dense = probe_times(InjectionPhase::DiscoverTargets, 12);

        // 旧节奏在 1200 ms 之后要等到 1700 ms 才再次探测。
        assert_eq!(legacy[4], 1200);
        assert_eq!(legacy[5], 1700);
        // 新节奏在 1200~1700 ms 之间补出三个探测点，最多提前 450 ms 命中。
        assert_eq!(
            dense
                .iter()
                .copied()
                .filter(|&t| t > 1200 && t < 1700)
                .collect::<Vec<_>>(),
            vec![1250, 1450, 1650]
        );
    }

    #[test]
    fn retry_stats_separate_cheap_discovery_failures_from_bridge_failures() {
        let mut stats = InjectionRetryStats {
            attempts: 5,
            ..Default::default()
        };
        stats.record_failure(InjectionPhase::DiscoverTargets, Duration::from_millis(12));
        stats.record_failure(InjectionPhase::SelectTarget, Duration::from_millis(18));
        stats.record_failure(InjectionPhase::VerifyReadiness, Duration::from_millis(900));

        assert_eq!(stats.attempts, 5);
        assert_eq!(stats.discovery_failures(), 2);
        assert_eq!(stats.bridge_failures(), 1);
        assert_eq!(
            stats.phase_failures[InjectionPhase::VerifyReadiness as usize],
            1
        );
        assert_eq!(stats.failure_expense_ms, 930);
        assert_eq!(
            stats.last_failure_phase.map(InjectionPhase::key),
            Some("verifyReadiness")
        );
    }

    #[test]
    fn retry_source_keys_separate_startup_from_watchdog_rebuilds() {
        assert_eq!(InjectionRetrySource::Startup.key(), "startup");
        assert_eq!(InjectionRetrySource::Watchdog.key(), "watchdog");
    }

    #[test]
    fn injection_phase_keys_cover_every_variant() {
        for (phase, key) in [
            (InjectionPhase::DiscoverTargets, "discoverTargets"),
            (InjectionPhase::SelectTarget, "selectTarget"),
            (InjectionPhase::InstallBridge, "installBridge"),
            (InjectionPhase::VerifyReadiness, "verifyReadiness"),
            (InjectionPhase::ReadStatuses, "readStatuses"),
        ] {
            assert_eq!(phase.key(), key);
            assert_eq!(InjectionPhase::from_raw(phase as u8) as u8, phase as u8);
        }
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
    fn injection_error_summary_names_the_cdp_protocol_failure() {
        // installBridge 期间的协议/连接错误必须保留方法名，不再回落到
        // 笼统的"内部注入尝试失败"。
        let refused = anyhow::anyhow!(
            "向 Codex renderer page-1 安装 CDP bridge 失败: CDP command Runtime.addBinding id 3 failed: {{\"code\":-32601,\"message\":\"'Runtime.addBinding' wasn't found\"}}"
        );
        let closed = anyhow::anyhow!(
            "安装 Codex 模型请求校验失败: CDP websocket closed before response for Runtime.evaluate id 9"
        );
        let exception = anyhow::anyhow!(
            "安装 Codex 模型请求校验失败: Runtime.evaluate raised an exception: {{\"exceptionId\":1,\"text\":\"Uncaught\",\"exception\":{{\"type\":\"object\",\"description\":\"Error: 当前 Codex 暂不支持模型请求校验\\n    at <anonymous>:1:5\"}}}}"
        );

        assert_eq!(
            safe_injection_error_summary(&refused),
            "CDP 命令 Runtime.addBinding 被 Codex 拒绝：'Runtime.addBinding' wasn't found"
        );
        assert_eq!(
            safe_injection_error_summary(&closed),
            "CDP 连接被 Codex 关闭（Runtime.evaluate）"
        );
        assert_eq!(
            safe_injection_error_summary(&exception),
            "模型请求校验未通过：当前 Codex 暂不支持模型请求校验"
        );
    }

    #[test]
    fn injection_failure_detail_keeps_the_full_bounded_error_chain() {
        let error = anyhow::anyhow!("inner cause").context("outer context");
        assert_eq!(
            injection_failure_detail(&error),
            "outer context: inner cause"
        );

        let huge = anyhow::anyhow!("x".repeat(MAX_INJECTION_DETAIL_CHARS * 2));
        assert_eq!(
            injection_failure_detail(&huge).chars().count(),
            MAX_INJECTION_DETAIL_CHARS
        );
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

    #[tokio::test]
    async fn an_endpoint_that_answers_no_cdp_command_is_unresponsive_not_busy() {
        // An endpoint can accept the WebSocket upgrade and still never execute
        // a command; that is what a replaced page target looks like. It must
        // not be reported as a busy renderer, because "busy" tells the watchdog
        // to wait instead of rediscovering the target.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _held = socket;
            std::future::pending::<()>().await;
        });

        assert_eq!(
            is_target_healthy(&format!("ws://{address}")).await.unwrap(),
            TargetHealth::Unresponsive,
        );

        server.abort();
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
        let prepared = prepare_injection_scripts(
            true,
            false,
            false,
            &["".to_string(), "window.userScriptRan = true;".to_string()],
        );

        assert_eq!(prepared.scripts.len(), 2);
        let core = &prepared.scripts[0];
        assert!(core.contains("window.__codeyBridgeHelpersInstalled"));
        assert!(!core.contains("__codeyGitRequestGuard"));
        assert!(!core.contains("__codeyWindowsWmiSamplerGuard"));
        assert!(core.contains("window.__codeyModelWhitelistPatch"));
        assert!(core.contains("/codex-model-catalog"));
        let shared_runtime_offset = core
            .find("window.__codeySharedRuntime=Object.freeze")
            .expect("bridge helpers must initialize the shared runtime");
        let renderer_offset = core
            .find("window.__codeyRendererCoreLoaded")
            .expect("renderer bootstrap must be part of renderer-controls");
        assert!(shared_runtime_offset < renderer_offset);
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
        let user_script = prepared
            .descriptors
            .iter()
            .find(|descriptor| descriptor.id == "user-script-1")
            .expect("user script descriptor should be present");
        assert_eq!(user_script.source, "user");
        assert_eq!(
            prepared.descriptors[0].visibility,
            InjectionScriptVisibility::Internal
        );
        assert_eq!(prepared.descriptors[5].id, "renderer-controls");
        assert_eq!(
            prepared.descriptors[5].visibility,
            InjectionScriptVisibility::Internal
        );
        assert!(
            prepared
                .descriptors
                .iter()
                .any(|descriptor| descriptor.id == "quota-unlock")
        );
        assert!(core.contains("__codeyQuotaUnlock"));
        assert_eq!(user_script.visibility, InjectionScriptVisibility::Feature);
        let snapshot_script = injection_status_snapshot_script(&prepared.descriptors);
        assert!(snapshot_script.contains("bridge-helpers"));
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
        assert!(overlay_load_script.contains("codey-overlay-root"));
        assert!(overlay_load_script.contains("codey-overlay-modal-container"));
        assert!(overlay_load_script.contains("@layer"));
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
    fn disabled_local_router_installs_only_native_model_selection_mode() {
        let prepared = prepare_injection_scripts(false, false, false, &[]);

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
        assert!(
            !prepared
                .descriptors
                .iter()
                .any(|descriptor| descriptor.id == "quota-unlock")
        );
    }

    #[test]
    fn injection_statuses_preserve_script_order_and_report_missing_entries() {
        let prepared = prepare_injection_scripts(
            true,
            false,
            false,
            &["window.userScriptRan = true;".to_string()],
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
        assert_eq!(statuses[1].id, "model-whitelist");
        assert_eq!(statuses[1].status, "unknown");
        assert_eq!(statuses[3].id, "security-warning-shield");
        assert_eq!(statuses[3].status, "inactive");
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
