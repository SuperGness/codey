use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use codex_plus_core::bridge::{BridgeHandler, bridge_health_check_script, install_bridge};
use codex_plus_core::cdp::{list_targets, pick_injectable_codex_page_target};

use crate::pending_approval::SessionLifecycleStatus;

const CODEY_BRIDGE_SCRIPT: &str = include_str!("../../public/codey-bridge.js");
const RENDERER_INJECT_SCRIPT: &str = include_str!("../../public/renderer-inject.js");
const PET_CONTROL_SHIELD_SCRIPT: &str = include_str!("../../public/pet-control-shield.js");
const VOICE_CONTROL_SHIELD_SCRIPT: &str = include_str!("../../public/voice-control-shield.js");
const SETTINGS_OVERLAY_SCRIPT: &str = include_str!("../../dist-overlay/codey-overlay.js");
const CODEY_INJECT_SCRIPT: &str = include_str!("../../public/codey-inject.js");
const FAST_MODE_FIX_SCRIPT: &str = include_str!("../../public/fast-mode-fix.js");
const PLUGIN_MARKETPLACE_FIX_SCRIPT: &str = include_str!("../../public/plugin-marketplace-fix.js");

pub async fn retry_inject_with_scripts(
    debug_port: u16,
    handler: BridgeHandler,
    slim_codex_pet: bool,
    slim_codex_voice: bool,
    user_scripts: &[String],
) -> Result<()> {
    let mut last_error = None;
    for _ in 0..30 {
        match inject_with_scripts(
            debug_port,
            handler.clone(),
            slim_codex_pet,
            slim_codex_voice,
            user_scripts,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Codey CDP 注入失败")))
}

pub async fn inject_with_scripts(
    debug_port: u16,
    handler: BridgeHandler,
    slim_codex_pet: bool,
    slim_codex_voice: bool,
    user_scripts: &[String],
) -> Result<()> {
    let targets = list_targets(debug_port).await?;
    let target = pick_injectable_codex_page_target(&targets)?;
    let websocket_url = target
        .web_socket_debugger_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Codex 页面没有 CDP WebSocket 地址"))?;
    let prepare = |script: &str| {
        script
            .replace(
                "__CODEY_SLIM_PET__",
                if slim_codex_pet { "true" } else { "false" },
            )
            .replace(
                "__CODEY_SLIM_VOICE__",
                if slim_codex_voice { "true" } else { "false" },
            )
    };
    let mut scripts = vec![
        prepare(CODEY_BRIDGE_SCRIPT),
        prepare(RENDERER_INJECT_SCRIPT),
        prepare(PET_CONTROL_SHIELD_SCRIPT),
        prepare(VOICE_CONTROL_SHIELD_SCRIPT),
        wrap_settings_overlay(&prepare(SETTINGS_OVERLAY_SCRIPT)),
        prepare(CODEY_INJECT_SCRIPT),
        prepare(FAST_MODE_FIX_SCRIPT),
        prepare(PLUGIN_MARKETPLACE_FIX_SCRIPT),
    ];
    scripts.extend(
        user_scripts
            .iter()
            .filter(|script| !script.trim().is_empty())
            .cloned(),
    );
    install_bridge(
        websocket_url,
        codex_plus_core::bridge::BRIDGE_BINDING_NAME,
        handler,
        &scripts,
    )
    .await?;
    ensure_settings_overlay_ready(websocket_url).await
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

pub async fn is_healthy(debug_port: u16) -> Result<bool> {
    let targets = list_targets(debug_port).await?;
    let target = pick_injectable_codex_page_target(&targets)?;
    let websocket_url = target
        .web_socket_debugger_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Codex 页面没有 CDP WebSocket 地址"))?;
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
}
