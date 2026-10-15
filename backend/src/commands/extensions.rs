use serde_json::{Value, json};
use std::sync::{Arc, atomic::Ordering};

use super::{AppState, argument};

/// 仅在这里取得宿主路径和生命周期状态；扩展模块不依赖 AppState。
pub(super) async fn invoke(state: &Arc<AppState>, args: &Value) -> Result<Value, String> {
    let request = argument::<Value>(args, "request")?;
    let reload_mcp = matches!(
        request["action"].as_str(),
        Some("save_mcp" | "set_mcp_enabled" | "set_mcps_enabled" | "remove_mcp")
    );
    if state.is_shutting_down() || state.restart_in_progress.load(Ordering::Acquire) {
        return Err("Codey 正在退出或重启，请稍后重新操作".into());
    }
    let user_home = directories::BaseDirs::new()
        .ok_or("无法确定当前用户目录")?
        .home_dir()
        .to_path_buf();
    let data_dir = state
        .store
        .path()
        .parent()
        .ok_or("Codey 配置目录无效")?
        .join("codex-extensions");
    let mut result = crate::codex_extensions::dispatch(
        crate::codex_config::codex_home().to_path_buf(),
        data_dir,
        user_home,
        request,
        state.can_show_native_panels().await,
    )
    .await
    .map_err(|error| error.to_string())?;
    if reload_mcp
        && (result["applyStatus"] == "reload-required" || result["applyStatus"] == "unchanged")
    {
        // 配置事务已完成；刷新失败也必须返回已保存的清单，避免重复导入。
        // 宿主注入模式下没有客户端进程句柄，注入目标改从注入上下文取。
        if let Some(websocket_url) = state.active_renderer_websocket_url().await {
            let reloaded = tokio::time::timeout(
                std::time::Duration::from_secs(25),
                crate::cdp::reload_mcp_servers(&websocket_url),
            )
            .await;
            if matches!(reloaded, Ok(Ok(()))) {
                result["applyStatus"] = json!("applied");
                result["message"] = json!("配置已保存，下一轮对话生效。");
            } else {
                result["applyStatus"] = json!("reload-failed");
                result["message"] = json!("配置已保存，未能确认自动刷新，请重启 Codex。");
            }
        } else {
            result["applyStatus"] = json!("pending-runtime");
            result["message"] = json!("配置已保存，Codex 下次启动时生效。");
        }
    }
    Ok(result)
}
