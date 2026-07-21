use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use codex_plus_core::bridge::{
    BridgeHandler, BridgePumpHandle, bridge_health_check_script, install_bridge,
};
use codex_plus_core::cdp::{list_targets, pick_injectable_codex_page_target};

use crate::pending_approval::SessionLifecycleStatus;

const SETTINGS_OVERLAY_LOAD_PATH: &str = "/internal/codey/settings-overlay/load";
const CODEY_BRIDGE_SCRIPT: &str = include_str!("../../public/codey-bridge.js");
const RENDERER_INJECT_SCRIPT: &str = include_str!("../../public/renderer-inject.js");
const PET_CONTROL_SHIELD_SCRIPT: &str = include_str!("../../public/pet-control-shield.js");
const VOICE_CONTROL_SHIELD_SCRIPT: &str = include_str!("../../public/voice-control-shield.js");
const SETTINGS_OVERLAY_SCRIPT: &str = include_str!("../../dist-overlay/codey-overlay.js");
const CODEY_INJECT_SCRIPT: &str = include_str!("../../public/codey-inject.js");
const FAST_MODE_FIX_SCRIPT: &str = include_str!("../../public/fast-mode-fix.js");
const PLUGIN_MARKETPLACE_FIX_SCRIPT: &str = include_str!("../../public/plugin-marketplace-fix.js");
static SETTINGS_OVERLAY_LOAD_SCRIPT: OnceLock<Arc<str>> = OnceLock::new();

#[derive(Clone)]
pub struct PreparedInjectionScripts {
    scripts: Arc<[String]>,
}

pub struct InjectedTarget {
    websocket_url: Arc<str>,
    pump: BridgePumpHandle,
}

impl InjectedTarget {
    pub fn websocket_url(&self) -> &str {
        &self.websocket_url
    }

    pub async fn close(self) {
        self.pump.close().await;
    }
}

pub fn prepare_injection_scripts(
    slim_codex_pet: bool,
    slim_codex_voice: bool,
    user_scripts: &[String],
) -> PreparedInjectionScripts {
    let mut core_bundle = String::with_capacity(
        CODEY_BRIDGE_SCRIPT.len()
            + RENDERER_INJECT_SCRIPT.len()
            + PET_CONTROL_SHIELD_SCRIPT.len()
            + VOICE_CONTROL_SHIELD_SCRIPT.len()
            + CODEY_INJECT_SCRIPT.len()
            + FAST_MODE_FIX_SCRIPT.len()
            + PLUGIN_MARKETPLACE_FIX_SCRIPT.len()
            + 4096,
    );
    for (name, script) in [
        ("bridge helpers", CODEY_BRIDGE_SCRIPT),
        ("renderer marker", RENDERER_INJECT_SCRIPT),
        ("pet control shield", PET_CONTROL_SHIELD_SCRIPT),
        ("voice control shield", VOICE_CONTROL_SHIELD_SCRIPT),
        (
            "settings overlay loader",
            lazy_settings_overlay_loader_script(),
        ),
        ("renderer controls", CODEY_INJECT_SCRIPT),
        ("legacy fast control cleanup", FAST_MODE_FIX_SCRIPT),
        (
            "plugin marketplace compatibility",
            PLUGIN_MARKETPLACE_FIX_SCRIPT,
        ),
    ] {
        let prepared = prepare_script(script, slim_codex_pet, slim_codex_voice);
        append_guarded_script(&mut core_bundle, name, prepared.as_ref());
    }

    let mut scripts = Vec::with_capacity(1 + user_scripts.len());
    scripts.push(core_bundle);
    scripts.extend(
        user_scripts
            .iter()
            .filter(|script| !script.trim().is_empty())
            .cloned(),
    );
    PreparedInjectionScripts {
        scripts: Arc::from(scripts),
    }
}

fn prepare_script(script: &str, slim_codex_pet: bool, slim_codex_voice: bool) -> Cow<'_, str> {
    if !script.contains("__CODEY_SLIM_PET__") && !script.contains("__CODEY_SLIM_VOICE__") {
        return Cow::Borrowed(script);
    }
    Cow::Owned(
        script
            .replace(
                "__CODEY_SLIM_PET__",
                if slim_codex_pet { "true" } else { "false" },
            )
            .replace(
                "__CODEY_SLIM_VOICE__",
                if slim_codex_voice { "true" } else { "false" },
            ),
    )
}

fn append_guarded_script(bundle: &mut String, name: &str, script: &str) {
    bundle.push_str("\ntry {\n");
    bundle.push_str(script);
    bundle.push_str("\n} catch (error) {\n  console.error(\"[Codey] ");
    bundle.push_str(name);
    bundle.push_str(" injection failed\", error);\n}\n");
}

pub async fn retry_inject_with_scripts(
    debug_port: u16,
    handler: BridgeHandler,
    scripts: &PreparedInjectionScripts,
) -> Result<InjectedTarget> {
    let mut last_error = None;
    for _ in 0..30 {
        match inject_with_scripts(debug_port, handler.clone(), scripts).await {
            Ok(target) => return Ok(target),
            Err(error) => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Codey CDP 注入失败")))
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
    let handler = with_settings_overlay_loader(handler, websocket_url.clone());
    let pump = install_bridge(
        &websocket_url,
        codex_plus_core::bridge::BRIDGE_BINDING_NAME,
        handler,
        &scripts.scripts,
    )
    .await?;
    ensure_settings_overlay_ready(&websocket_url).await?;
    Ok(InjectedTarget {
        websocket_url,
        pump,
    })
}

fn with_settings_overlay_loader(handler: BridgeHandler, websocket_url: Arc<str>) -> BridgeHandler {
    Arc::new(move |path, payload| {
        if path != SETTINGS_OVERLAY_LOAD_PATH {
            return handler(path, payload);
        }

        let websocket_url = websocket_url.clone();
        Box::pin(async move {
            let settings_overlay_load_script = prepared_settings_overlay_load_script();
            let response = codex_plus_core::bridge::evaluate_script(
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
        })
    })
}

fn prepared_settings_overlay_load_script() -> Arc<str> {
    SETTINGS_OVERLAY_LOAD_SCRIPT
        .get_or_init(|| Arc::from(settings_overlay_load_script(SETTINGS_OVERLAY_SCRIPT)))
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

async fn ensure_settings_overlay_ready(websocket_url: &str) -> Result<()> {
    let ready = codex_plus_core::bridge::evaluate_script(
        websocket_url,
        r#"typeof window.__codeySettingsOverlay === "object"
          && typeof window.__codeySettingsOverlay.toggle === "function""#,
    )
    .await
    .context("检查 Codey 内嵌配置面板状态失败")?;
    if runtime_value(&ready).and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(());
    }

    let error = codex_plus_core::bridge::evaluate_script(
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
    let result = codex_plus_core::bridge::evaluate_script_with_await_promise(
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

pub async fn sync_thread_statuses(
    debug_port: u16,
    statuses: &HashMap<String, SessionLifecycleStatus>,
) -> Result<()> {
    let targets = list_targets(debug_port).await?;
    let target = pick_injectable_codex_page_target(&targets)?;
    let websocket_url = target
        .web_socket_debugger_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Codex 页面没有 CDP WebSocket 地址"))?;
    let script = thread_status_sync_script(statuses)?;
    codex_plus_core::bridge::evaluate_script(websocket_url, &script)
        .await
        .context("同步 Codex 侧边栏任务状态失败")?;
    Ok(())
}

fn thread_status_sync_script(statuses: &HashMap<String, SessionLifecycleStatus>) -> Result<String> {
    let normalized = statuses
        .iter()
        .map(|(session_id, status)| {
            (
                session_id.trim().trim_start_matches("local:").to_string(),
                status,
            )
        })
        .collect::<HashMap<_, _>>();
    let statuses_json = serde_json::to_string(&normalized)?;
    Ok(format!(
        r#"(() => {{
  const statuses = {statuses_json};
  const attribute = "data-codey-thread-traffic-status";
  const normalize = (value) => String(value || "").trim().replace(/^local:/, "");
  const sessionIdFromRow = (row) => {{
    if (typeof window.__codeyThreadSessionIdFromRow === "function") {{
      try {{
        const resolved = normalize(window.__codeyThreadSessionIdFromRow(row));
        if (resolved) return resolved;
      }} catch {{}}
    }}
    return normalize(row.getAttribute("data-app-action-sidebar-thread-id"));
  }};
  window.__codeyHostThreadStatuses = statuses;
  window.__codeyHostThreadStatusesAuthoritative = true;
  if (typeof window.__codeyInstallThreadStatusIndicators === "function") {{
    window.__codeyInstallThreadStatusIndicators();
  }}
  document.querySelectorAll("[data-app-action-sidebar-thread-row]").forEach((row) => {{
    const sessionId = sessionIdFromRow(row);
    const state = statuses[sessionId] || "idle";
    if (state === "running" || state === "error" || state === "waiting") {{
      row.setAttribute(attribute, state);
    }} else {{
      row.removeAttribute(attribute);
    }}
  }});
  return true;
}})()"#
    ))
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
    fn host_status_script_applies_an_authoritative_normalized_map() {
        let statuses = HashMap::from([
            (
                "local:thread-running".to_string(),
                SessionLifecycleStatus::Running,
            ),
            ("thread-idle".to_string(), SessionLifecycleStatus::Idle),
        ]);

        let script = thread_status_sync_script(&statuses).unwrap();

        assert!(script.contains(r#""thread-running":"running""#));
        assert!(script.contains(r#""thread-idle":"idle""#));
        assert!(script.contains("window.__codeyHostThreadStatusesAuthoritative = true"));
        assert!(script.contains("window.__codeyThreadSessionIdFromRow(row)"));
        assert!(script.contains("data-codey-thread-traffic-status"));
    }

    #[test]
    fn pet_control_shield_receives_the_launch_setting() {
        let enabled = PET_CONTROL_SHIELD_SCRIPT.replace("__CODEY_SLIM_PET__", "true");
        let disabled = PET_CONTROL_SHIELD_SCRIPT.replace("__CODEY_SLIM_PET__", "false");

        assert!(enabled.contains(r#"const enabled = "true" === "true""#));
        assert!(disabled.contains(r#"const enabled = "false" === "true""#));
    }

    #[test]
    fn voice_control_shield_receives_the_launch_setting() {
        let enabled = VOICE_CONTROL_SHIELD_SCRIPT.replace("__CODEY_SLIM_VOICE__", "true");
        let disabled = VOICE_CONTROL_SHIELD_SCRIPT.replace("__CODEY_SLIM_VOICE__", "false");

        assert!(enabled.contains(r#"const enabled = "true" === "true""#));
        assert!(disabled.contains(r#"const enabled = "false" === "true""#));
    }

    #[test]
    fn core_scripts_share_one_cdp_document_script_and_user_scripts_stay_isolated() {
        let prepared = prepare_injection_scripts(
            true,
            false,
            &["".to_string(), "window.userScriptRan = true;".to_string()],
        );

        assert_eq!(prepared.scripts.len(), 2);
        let core = &prepared.scripts[0];
        assert!(core.contains("window.__codeyBridgeHelpersInstalled"));
        assert!(core.contains("window.__codeyRendererInjectLoaded"));
        assert!(core.contains(r#"const enabled = "true" === "true""#));
        assert!(core.contains(r#"const enabled = "false" === "true""#));
        assert!(core.contains(SETTINGS_OVERLAY_LOAD_PATH));
        assert!(core.contains("__codeyLazyLoader"));
        assert!(!core.contains("codey-settings-overlay-host"));
        assert!(core.len() < SETTINGS_OVERLAY_SCRIPT.len());
        assert!(core.contains("plugin marketplace compatibility injection failed"));
        assert_eq!(prepared.scripts[1], "window.userScriptRan = true;");
        let overlay_load_script = prepared_settings_overlay_load_script();
        assert!(overlay_load_script.contains("codey-settings-overlay-host"));
        assert!(overlay_load_script.contains("delete window.__codeySettingsOverlay"));
        assert!(
            overlay_load_script.contains("window.__codeySettingsOverlay = current"),
            "a failed bundle evaluation must restore the lazy loader for retry"
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
