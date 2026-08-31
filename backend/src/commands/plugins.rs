use std::path::Path;

use serde_json::{Value, json};

use crate::codex_config::codex_home;
use crate::error_log;
use crate::plugin_marketplace;

pub(super) async fn plugin_marketplace_status() -> Result<Value, String> {
    let home = codex_home();
    let marketplace_home = home;
    let result = tokio::task::spawn_blocking(move || {
        plugin_marketplace::marketplaces_status(marketplace_home)
    })
    .await
    .map_err(|error| format!("插件市场状态任务异常退出：{error}"));
    let mut status = match result {
        Ok(status) => status,
        Err(error) => {
            error_log::record_failure(
                "patch_status_failed",
                "read_plugin_marketplace_status",
                error.clone(),
                json!({
                    "codexHome": home,
                }),
            );
            return Err(error);
        }
    };
    decorate_plugin_marketplace_status(home, &mut status);
    Ok(status)
}

pub(super) async fn repair_plugin_marketplace() -> Result<Value, String> {
    let home = codex_home();
    let marketplace_home = home;
    let result = tokio::task::spawn_blocking(move || {
        plugin_marketplace::ensure_marketplaces(marketplace_home)
    })
    .await
    .map_err(|error| format!("插件市场修复任务异常退出：{error}"))
    .and_then(|result| result.map_err(|error| error.to_string()));
    let repair = match result {
        Ok(repair) => repair,
        Err(error) => {
            error_log::record_failure(
                "patch_failed",
                "repair_plugin_marketplace",
                error.clone(),
                json!({
                    "codexHome": home,
                }),
            );
            return Err(error);
        }
    };
    let mut status = plugin_marketplace::marketplaces_status(home);
    if let Some(object) = status.as_object_mut() {
        for key in ["initializedRemote", "configuredRemote", "configChanged"] {
            if let Some(value) = repair.get(key) {
                object.insert(key.into(), value.clone());
            }
        }
    }
    decorate_plugin_marketplace_status(home, &mut status);
    Ok(status)
}

fn decorate_plugin_marketplace_status(home: &Path, status: &mut Value) {
    let needs_repair = status
        .get("needsRepair")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if let Some(object) = status.as_object_mut() {
        object.insert(
            "status".into(),
            Value::String(
                if needs_repair {
                    "needs_repair"
                } else {
                    "ready"
                }
                .into(),
            ),
        );
        object.insert(
            "localMarketplacePath".into(),
            Value::String(
                home.join(".tmp/plugins-remote")
                    .to_string_lossy()
                    .to_string(),
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marketplace_status_decoration_reports_readiness_and_local_path() {
        let home = Path::new("/tmp/codey-test-home");
        let mut ready = json!({ "needsRepair": false });
        decorate_plugin_marketplace_status(home, &mut ready);
        assert_eq!(ready["status"], "ready");
        assert_eq!(
            ready["localMarketplacePath"],
            home.join(".tmp/plugins-remote").to_string_lossy().as_ref(),
        );

        let mut needs_repair = json!({ "needsRepair": true });
        decorate_plugin_marketplace_status(home, &mut needs_repair);
        assert_eq!(needs_repair["status"], "needs_repair");
    }
}
