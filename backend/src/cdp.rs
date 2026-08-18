use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use codey_runtime_core::bridge::{
    BridgeHandler, BridgePumpHandle, bridge_health_check_script, install_bridge,
};
use codey_runtime_core::cdp::{list_targets, pick_injectable_codex_page_target};
use serde::{Deserialize, Serialize};

use crate::error_log;

const SETTINGS_OVERLAY_LOAD_PATH: &str = "/internal/codey/settings-overlay/load";
const SESSION_TOOLS_LOAD_PATH: &str = "/internal/codey/session-tools/load";
const FAST_STARTUP_STATSIG_TIMEOUT_MS: u64 = 1500;
const CDP_INJECTION_TIMEOUT: Duration = Duration::from_secs(30);
const FAST_STARTUP_SHIELD_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/fast-startup-shield.js");
const CODEY_BRIDGE_SCRIPT: &str = include_str!("../../dist-overlay/inject/codey-bridge.js");
const WORKFLOW_MODE_SCRIPT: &str = include_str!("../../dist-overlay/inject/workflow-mode.js");
const GIT_REQUEST_GUARD_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/git-request-guard.js");
const WINDOWS_WMI_SAMPLER_GUARD_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/windows-wmi-sampler-guard.js");
const MODEL_WHITELIST_INJECT_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/model-whitelist-inject.js");
const RENDERER_INJECT_SCRIPT: &str = include_str!("../../dist-overlay/inject/renderer-inject.js");
const CODEY_SESSION_TOOLS_SCRIPT: &str = include_str!("../../dist-overlay/inject/codey-inject.js");
const PET_CONTROL_SHIELD_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/pet-control-shield.js");
const SECURITY_WARNING_SHIELD_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/security-warning-shield.js");
const SETTINGS_OVERLAY_SCRIPT: &str = include_str!("../../dist-overlay/codey-overlay.js");
const SETTINGS_OVERLAY_STYLES: &str = include_str!("../../dist-overlay/codey.css");
const PLUGIN_MARKETPLACE_FIX_SCRIPT: &str =
    include_str!("../../dist-overlay/inject/plugin-marketplace-fix.js");
const PROMPT_OPTIMIZE_SCRIPT: &str = include_str!("../../dist-overlay/inject/prompt-optimize.js");
const MAX_INJECTION_ERROR_CHARS: usize = 500;
static SETTINGS_OVERLAY_LOAD_SCRIPT: OnceLock<Arc<str>> = OnceLock::new();
static SESSION_TOOLS_LOAD_SCRIPT: OnceLock<Arc<str>> = OnceLock::new();

#[derive(Clone)]
struct InjectionScriptDescriptor {
    id: String,
    name: String,
    source: &'static str,
    probe: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InjectionScriptStatus {
    pub id: String,
    pub name: String,
    pub source: String,
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
    fast_codex_startup: bool,
    slim_codex_pet: bool,
    hide_full_access_warning: bool,
    user_scripts: &[String],
) -> PreparedInjectionScripts {
    let builtin_scripts = [
        (
            "bridge-helpers",
            "桥接辅助",
            CODEY_BRIDGE_SCRIPT,
            r#"typeof window.__codexSessionDeleteBridge === "function"
              && typeof window.__codeyCall === "function"
              ? "桥接函数可调用" : """#
                .to_string(),
        ),
        (
            "workflow-mode",
            "Codey 工作流接管",
            WORKFLOW_MODE_SCRIPT,
            r#"window.__codeyWorkflowModeRuntime?.version === 1
              ? "工作流 Composer 门禁已加载" : """#
                .to_string(),
        ),
        (
            "fast-startup-shield",
            "Codex 快速启动保护",
            FAST_STARTUP_SHIELD_SCRIPT,
            format!(
                r#"(() => {{
                  const shield = window.__codeyFastStartupShield;
                  if (!shield || shield.enabled !== {fast_codex_startup}
                    || typeof shield.snapshot !== "function") return "";
                  const snapshot = shield.snapshot();
                  return shield.enabled
                    ? `慢请求保护已启用（${{snapshot.timeoutMs}}ms，已降级 ${{snapshot.statsigTimeouts}} 次）`
                    : "慢请求保护已关闭";
                }})()"#
            ),
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
              if (snapshot.selfTestPassed === true) {
                const workersObserved =
                  Number(snapshot.mainProcessSnapshot?.workersObserved) || 0;
                return "WMI 周期采样保护一次性自检通过" +
                  (workersObserved > 0
                    ? `；已观察 ${workersObserved} 个其他 Worker，实际目标采样尚未触发`
                    : "；实际目标采样尚未触发");
              }
              if (snapshot.mainProcessSnapshot?.selfTestError) {
                return {
                  effective: false,
                  detail: `WMI 周期采样保护自检失败：${snapshot.mainProcessSnapshot.selfTestError}`,
                };
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
        ),
        (
            "pet-control-shield",
            "宠物控制精简",
            PET_CONTROL_SHIELD_SCRIPT,
            format!(
                r#"window.__codeyPetControlShield?.enabled === {slim_codex_pet}
                  && typeof window.__codeyPetControlShield?.block === "function"
                  ? {} : """#,
                serde_json::to_string(if slim_codex_pet {
                    "宠物控制精简已启用"
                } else {
                    "控制器已就绪，当前精简策略关闭"
                })
                .expect("pet probe detail should serialize")
            ),
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
                serde_json::to_string(if hide_full_access_warning {
                    "安全提示屏蔽已启用"
                } else {
                    "控制器已就绪，当前屏蔽策略关闭"
                })
                .expect("security probe detail should serialize")
            ),
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
        ),
        (
            "prompt-optimize",
            "提示词优化",
            PROMPT_OPTIMIZE_SCRIPT,
            r#"(() => {
              const optimizer = window.__codeyPromptOptimize;
              if (!optimizer || typeof optimizer.snapshot !== "function") return "";
              const snapshot = optimizer.snapshot();
              return snapshot.ready === true
                ? (snapshot.enabled === true ? "提示词优化按钮已就绪" : "提示词优化已关闭")
                : "";
            })()"#
                .to_string(),
        ),
    ];
    let mut core_bundle = String::with_capacity(
        FAST_STARTUP_SHIELD_SCRIPT.len()
            + CODEY_BRIDGE_SCRIPT.len()
            + WORKFLOW_MODE_SCRIPT.len()
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
    for (id, name, script, probe) in builtin_scripts {
        let descriptor = InjectionScriptDescriptor {
            id: id.to_string(),
            name: name.to_string(),
            source: "builtin",
            probe: Some(probe),
        };
        let prepared = prepare_script(script, fast_codex_startup, slim_codex_pet);
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

fn prepare_script(script: &str, fast_codex_startup: bool, slim_codex_pet: bool) -> Cow<'_, str> {
    if !script.contains("__CODEY_FAST_CODEX_STARTUP__")
        && !script.contains("__CODEY_STATSIG_TIMEOUT_MS__")
        && !script.contains("__CODEY_SLIM_PET__")
    {
        return Cow::Borrowed(script);
    }
    Cow::Owned(
        script
            .replace(
                "__CODEY_FAST_CODEX_STARTUP__",
                if fast_codex_startup { "true" } else { "false" },
            )
            .replace(
                "__CODEY_STATSIG_TIMEOUT_MS__",
                &FAST_STARTUP_STATSIG_TIMEOUT_MS.to_string(),
            )
            .replace(
                "__CODEY_SLIM_PET__",
                if slim_codex_pet { "true" } else { "false" },
            ),
    )
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
    let last_error = loop {
        match tokio::time::timeout_at(
            deadline,
            inject_with_scripts(debug_port, handler.clone(), scripts),
        )
        .await
        {
            Ok(Ok(target)) => return Ok(target),
            Ok(Err(error)) => {
                if tokio::time::Instant::now() + delay > deadline {
                    break error;
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(2));
            }
            Err(_) => {
                break anyhow::anyhow!(
                    "等待 Codex CDP bridge 注入超时（{} ms）",
                    CDP_INJECTION_TIMEOUT.as_millis()
                );
            }
        }
    };
    Err(InjectionRetryFailure { error: last_error })
}

async fn inject_with_scripts(
    debug_port: u16,
    handler: BridgeHandler,
    scripts: &PreparedInjectionScripts,
) -> Result<InjectedTarget> {
    let targets = list_targets(debug_port).await?;
    let target = pick_injectable_codex_page_target(&targets)?;
    let websocket_url: Arc<str> = Arc::from(
        target
            .web_socket_debugger_url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Codex 页面没有 CDP WebSocket 地址"))?,
    );
    let handler = with_lazy_loaders(handler, websocket_url.clone());
    let pump = install_bridge(
        &websocket_url,
        codey_runtime_core::bridge::BRIDGE_BINDING_NAME,
        handler,
        &scripts.scripts,
    )
    .await?;
    ensure_settings_overlay_ready(&websocket_url).await?;
    let injection_statuses = read_injection_statuses(&websocket_url, scripts)
        .await
        .unwrap_or_else(|error| {
            scripts.statuses_with_error(format!("读取注入状态失败：{error:#}"))
        });
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

pub async fn refresh_model_whitelist(
    websocket_url: &str,
    expected_models: &[String],
    expected_default_model: &str,
) -> Result<()> {
    let response = codey_runtime_core::bridge::evaluate_script_with_await_promise(
        websocket_url,
        &model_whitelist_refresh_script(expected_models, expected_default_model),
        true,
    )
    .await
    .context("请求 Codex 刷新模型列表失败")?;
    verify_model_whitelist_refresh_response(&response)
}

fn model_whitelist_refresh_script(
    expected_models: &[String],
    expected_default_model: &str,
) -> String {
    let expected_models =
        serde_json::to_string(expected_models).expect("model ids should serialize");
    let expected_default_model =
        serde_json::to_string(expected_default_model).expect("default model should serialize");
    format!(
        r#"(async () => {{
  const expectedModels = {expected_models};
  const expectedDefaultModel = {expected_default_model};
  const expectedCatalog = {{
    status: expectedModels.length > 0 ? "ok" : "not_configured",
    model: expectedDefaultModel,
    default_model: expectedDefaultModel,
    models: expectedModels,
  }};
  const matchesExpected = (snapshot) => (
    snapshot?.loaded === true
    && Array.isArray(snapshot.models)
    && snapshot.models.length === expectedModels.length
    && snapshot.models.every((model, index) => model === expectedModels[index])
    && snapshot.defaultModel === expectedDefaultModel
  );
  const reachedActiveModelPicker = (delivery) => (
    delivery?.responsePatchInstalled === true
    && Number(delivery.statsigClients) > 0
    && Number(delivery.notifiedClients) > 0
    && Number(delivery.queryClients) > 0
    && Number(delivery.queryEntries) > 0
  );
  let snapshot = null;
  let delivery = null;
  let lastError = "模型白名单补丁尚未就绪";
  for (const delay of [0, 80, 200, 500]) {{
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
      if (
        updated === true
        && matchesExpected(snapshot)
        && reachedActiveModelPicker(delivery)
      ) {{
        return JSON.stringify({{ ok: true, snapshot, delivery }});
      }}
      if (updated !== true) {{
        lastError = "模型白名单拒绝了后端推送的目录";
      }} else if (!matchesExpected(snapshot)) {{
        lastError = "模型白名单快照与已保存配置不一致";
      }} else {{
        lastError = "未能刷新 Codex 当前对话的模型查询缓存";
      }}
    }} catch (error) {{
      lastError = error instanceof Error ? error.message : String(error);
    }}
  }}
  return JSON.stringify({{ ok: false, error: lastError, snapshot, delivery }});
}})()"#
    )
}

fn verify_model_whitelist_refresh_response(response: &serde_json::Value) -> Result<()> {
    let payload = runtime_value(response)
        .and_then(serde_json::Value::as_str)
        .context("Codex 模型列表热更新未返回可解析结果")?;
    let report = serde_json::from_str::<serde_json::Value>(payload)
        .context("解析 Codex 模型列表热更新结果失败")?;
    if report.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(());
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
      if (!entry || entry.status !== "executed") continue;
      try {{
        const evidence = probe();
        const structured = evidence && typeof evidence === "object"
          && Object.prototype.hasOwnProperty.call(evidence, "effective");
        const effective = structured ? evidence.effective === true : Boolean(evidence);
        const detail = structured ? evidence.detail : evidence;
        if (effective) {{
          entry.status = "effective";
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
                    "effective" | "executed" | "failed"
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
        .get_or_init(|| {
            Arc::from(settings_overlay_load_script(
                SETTINGS_OVERLAY_SCRIPT,
                SETTINGS_OVERLAY_STYLES,
            ))
        })
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
    openWorkflow(request) {
      void this.load().then((overlay) => {
        if (typeof overlay.openWorkflow === "function") {
          overlay.openWorkflow(request);
        } else if (typeof overlay.open === "function") {
          overlay.open();
        } else {
          overlay.toggle();
        }
      }).catch((error) => {
        const message = formatError(error);
        window.__codeyOverlayError = message;
        loading = null;
        window.alert(`Codey 内嵌配置面板加载失败：${message}`);
      });
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

fn settings_overlay_load_script(script: &str, styles: &str) -> String {
    let wrapped = wrap_settings_overlay(script);
    let styles = serde_json::to_string(styles).expect("serialize settings overlay styles");
    format!(
        r#"(() => {{
  const current = window.__codeySettingsOverlay;
  if (current && typeof current.toggle === "function" && !current.__codeyLazyLoader) {{
    return "";
  }}
  if (current?.__codeyLazyLoader) delete window.__codeySettingsOverlay;
  window.__codeyComponentStyles = {styles};
  {wrapped}
  const ready = typeof window.__codeySettingsOverlay === "object"
    && typeof window.__codeySettingsOverlay.toggle === "function"
    && !window.__codeySettingsOverlay.__codeyLazyLoader;
  delete window.__codeyComponentStyles;
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

pub async fn is_target_healthy(websocket_url: &str) -> Result<bool> {
    let result = codey_runtime_core::bridge::evaluate_script_with_await_promise(
        websocket_url,
        bridge_health_check_script(),
        true,
    )
    .await
    .context("检查 Codey bridge 健康状态失败")?;
    Ok(result
        .get("result")
        .and_then(|value| value.get("result"))
        .and_then(|value| value.get("value"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false))
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
    fn model_whitelist_refresh_script_retries_and_verifies_the_expected_snapshot() {
        let script = model_whitelist_refresh_script(
            &["gpt-5.6-sol".into(), "provider-\"quoted".into()],
            "provider-\"quoted",
        );

        assert!(script.contains("window.__codeyModelWhitelistPatch"));
        assert!(script.contains("await patch.setCatalog(expectedCatalog)"));
        assert!(script.contains("patch.delivery()"));
        assert!(script.contains("patch.snapshot()"));
        assert!(!script.contains("patch.refresh()"));
        assert!(!script.contains("/codex-model-catalog"));
        assert!(script.contains("[0, 80, 200, 500]"));
        assert!(script.contains(r#"provider-\"quoted"#));
        assert!(script.contains("snapshot.defaultModel === expectedDefaultModel"));
        assert!(script.contains("delivery.queryEntries"));
    }

    #[test]
    fn model_whitelist_refresh_response_requires_a_verified_result() {
        let success = serde_json::json!({
            "result": {
                "result": {
                    "type": "string",
                    "value": r#"{"ok":true,"snapshot":{"loaded":true}}"#
                }
            }
        });
        assert!(verify_model_whitelist_refresh_response(&success).is_ok());

        let mismatch = serde_json::json!({
            "result": {
                "result": {
                    "type": "string",
                    "value": r#"{"ok":false,"error":"模型白名单快照与已保存配置不一致"}"#
                }
            }
        });
        let error = verify_model_whitelist_refresh_response(&mismatch).unwrap_err();
        assert!(format!("{error:#}").contains("快照与已保存配置不一致"));
    }

    #[test]
    fn pet_control_shield_receives_the_launch_setting() {
        let enabled = PET_CONTROL_SHIELD_SCRIPT.replace("__CODEY_SLIM_PET__", "true");
        let disabled = PET_CONTROL_SHIELD_SCRIPT.replace("__CODEY_SLIM_PET__", "false");

        assert!(enabled.contains(r#"["true"][0]==="true""#));
        assert!(disabled.contains(r#"["false"][0]==="true""#));
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
        assert!(core.contains("window.__codeyFastStartupShield"));
        assert!(core.contains(r#"["true"][0]==="true""#));
        assert!(core.contains("window.__codeyBridgeHelpersInstalled"));
        assert!(core.contains("__codeyGitRequestGuard"));
        assert!(core.contains("__codeyWindowsWmiSamplerGuard"));
        assert!(core.contains("window.__codeyModelWhitelistPatch"));
        assert!(core.contains("/codex-model-catalog"));
        assert!(core.contains("window.__codeyRendererCoreLoaded"));
        assert!(core.contains(r#"["true"][0]==="true""#));
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
        assert_eq!(prepared.descriptors.len(), 13);
        assert_eq!(prepared.descriptors[12].id, "user-script-1");
        assert_eq!(prepared.descriptors[12].source, "user");
        let snapshot_script = injection_status_snapshot_script(&prepared.descriptors);
        assert!(snapshot_script.contains("bridge-helpers"));
        assert!(snapshot_script.contains("Windows Git 请求限流已由主进程接管"));
        assert!(snapshot_script.contains("guard.ensureInstalled?.()"));
        assert!(snapshot_script.contains("snapshot.mainProcessProtected === true"));
        assert!(snapshot_script.contains("WMI 周期采样保护已安装"));
        assert!(snapshot_script.contains("snapshot.blocked > 0"));
        assert!(snapshot_script.contains("effective: false"));
        assert!(snapshot_script.contains("Object.prototype.hasOwnProperty.call"));
        assert!(snapshot_script.contains("模型目录已加载"));
        assert!(snapshot_script.contains("插件市场桥接已接管"));
        assert!(snapshot_script.contains("for (const delay of [50, 200, 750])"));
        assert!(!snapshot_script.contains("user-script-1\": () =>"));
        let overlay_load_script = prepared_settings_overlay_load_script();
        assert!(overlay_load_script.contains("codey-settings-overlay-host"));
        assert!(overlay_load_script.contains("window.__codeyComponentStyles = "));
        assert!(overlay_load_script.contains(".semi-button"));
        assert!(overlay_load_script.contains("--semi-color-primary:"));
        assert!(overlay_load_script.contains("delete window.__codeySettingsOverlay"));
        assert!(
            overlay_load_script.contains("window.__codeySettingsOverlay = current"),
            "a failed bundle evaluation must restore the lazy loader for retry"
        );
        let session_tools_load_script = prepared_session_tools_load_script();
        assert!(session_tools_load_script.contains("window.__codeySessionToolsInjectLoaded"));
        // 压缩会改写内部标识符，锚点必须用不会被改名的 window 属性。
        assert!(session_tools_load_script.contains("__codeyDeleteSelectedMessages"));
    }

    #[test]
    fn injection_statuses_preserve_script_order_and_report_missing_entries() {
        let prepared = prepare_injection_scripts(
            false,
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
        ];

        let statuses = reconcile_injection_statuses(&prepared.descriptors, reported);

        assert_eq!(statuses.len(), prepared.descriptors.len());
        assert_eq!(statuses[0].id, "bridge-helpers");
        assert_eq!(statuses[0].status, "effective");
        assert_eq!(statuses[0].detail.as_deref(), Some("桥接函数可调用"));
        assert_eq!(statuses[1].id, "workflow-mode");
        assert_eq!(statuses[1].status, "unknown");
        assert_eq!(statuses[2].id, "fast-startup-shield");
        assert_eq!(statuses[2].status, "unknown");
        assert_eq!(statuses[3].id, "git-request-guard");
        assert_eq!(statuses[3].status, "unknown");
        assert_eq!(statuses[4].id, "windows-wmi-sampler");
        assert_eq!(statuses[4].status, "unknown");
        assert_eq!(statuses[5].id, "model-whitelist");
        assert_eq!(statuses[5].status, "unknown");
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
        let script = settings_overlay_load_script(
            "throw new Error('bundle failed');",
            ".semi-button { color: red; }",
        );

        let delete_index = script
            .find("delete window.__codeySettingsOverlay")
            .expect("lazy loader should be removed before evaluating the bundle");
        let restore_index = script
            .find("window.__codeySettingsOverlay = current")
            .expect("lazy loader should be restored when the bundle is not ready");

        assert!(restore_index > delete_index);
        assert!(script.contains("if (ready) return \"\""));
        assert!(script.contains("delete window.__codeyComponentStyles"));
    }
}
