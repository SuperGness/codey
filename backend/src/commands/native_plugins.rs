use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Value, json};

use super::{argument, optional_argument, string_argument};
use crate::codey_plugins;

mod package_upload;

pub(super) async fn invoke(command: &str, args: &Value) -> Result<Value, String> {
    match command {
        "get_codey_plugin_host_info" => {
            serde_json::to_value(codey_plugins::host_info()).map_err(|error| error.to_string())
        }
        "validate_codey_plugin_config" => {
            let content = string_argument(args, "content")?;
            blocking(move || Ok(codey_plugin_sdk::config::validate(&content))).await
        }
        "list_codey_plugins" => blocking(codey_plugins::list).await,
        "get_codey_plugin_config_file" => {
            let id = string_argument(args, "pluginId")?;
            blocking(move || codey_plugins::get_config_file(&id)).await
        }
        "select_codey_plugin_package" => select_package().await,
        "inspect_codey_plugin" => {
            if let Some(id) = optional_argument::<String>(args, "discardUpload")? {
                blocking(move || package_upload::discard(&id)).await
            } else if let Some(upload) = args.get("upload") {
                let upload = upload.clone();
                blocking(move || package_upload::receive(upload)).await
            } else {
                let path = PathBuf::from(string_argument(args, "path")?);
                blocking(move || codey_plugins::inspect(&path)).await
            }
        }
        "install_codey_plugin" => {
            let path = PathBuf::from(string_argument(args, "path")?);
            let sha256 = string_argument(args, "sha256")?;
            blocking(move || codey_plugins::install(&path, &sha256)).await
        }
        "set_codey_plugin_enabled" => {
            let id = string_argument(args, "pluginId")?;
            let enabled = argument::<bool>(args, "enabled")?;
            blocking(move || codey_plugins::set_enabled(&id, enabled)).await
        }
        "save_codey_plugin_config_file" => {
            let id = string_argument(args, "pluginId")?;
            let content = string_argument(args, "content")?;
            let expected_sha256 = string_argument(args, "expectedSha256")?;
            blocking(move || codey_plugins::save_config_file(&id, &content, &expected_sha256)).await
        }
        "uninstall_codey_plugin" => {
            let id = string_argument(args, "pluginId")?;
            let remove_data = optional_argument::<bool>(args, "removeData")?.unwrap_or(false);
            blocking(move || codey_plugins::uninstall(&id, remove_data)).await
        }
        "open_codey_plugin_directory" => {
            let id = string_argument(args, "pluginId")?;
            blocking(move || {
                super::open_in_file_manager(&codey_plugins::plugin_directory(&id)?)?;
                Ok(json!({"status": "ok"}))
            })
            .await
        }
        "clear_codey_plugin_logs" => {
            let id = string_argument(args, "pluginId")?;
            if !argument::<bool>(args, "confirmed")? {
                return Err("清除插件日志需要确认".into());
            }
            blocking(move || codey_plugins::clear_logs(&id)).await
        }
        "open_codey_plugin_logs" => {
            let id = string_argument(args, "pluginId")?;
            blocking(move || {
                let directory = codey_plugins::plugin_directory(&id)?;
                let status = match crate::plugin_log_terminal::open(&directory)? {
                    crate::plugin_log_terminal::OpenStatus::Opened => "ok",
                    crate::plugin_log_terminal::OpenStatus::AlreadyOpen => "already_open",
                };
                Ok(json!({"status": status}))
            })
            .await
        }
        "invoke_codey_plugin" => {
            let id = string_argument(args, "pluginId")?;
            let method = string_argument(args, "method")?;
            let params = args.get("params").cloned().unwrap_or(Value::Null);
            blocking(move || codey_plugins::invoke(&id, &method, params)).await
        }
        _ => Err("未知插件管理命令".into()),
    }
}

async fn blocking<T: Serialize + Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || {
        serde_json::to_value(work()?).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("插件管理任务异常退出：{error}"))?
}

#[cfg(any(target_os = "macos", windows))]
async fn select_package() -> Result<Value, String> {
    blocking(|| {
        rfd::FileDialog::new()
            .set_title("导入 Codey 插件")
            .add_filter("Codey 插件", &["codey-plugin"])
            .pick_file()
            .map(|path| codey_plugins::inspect(&path))
            .transpose()
    })
    .await
}

#[cfg(not(any(target_os = "macos", windows)))]
async fn select_package() -> Result<Value, String> {
    Err("当前平台请在导入弹窗中填写插件包路径".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn readonly_commands_do_not_require_an_installed_plugin() {
        let value = invoke("get_codey_plugin_host_info", &json!({}))
            .await
            .unwrap();
        let info: codey_plugin_sdk::host::HostInfo = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(info.abi_version, codey_plugin_sdk::ABI_VERSION);
        assert_eq!(
            info.limits.max_config_bytes,
            codey_plugin_sdk::config::MAX_CONFIG_BYTES
        );
        assert_eq!(info.platform, std::env::consts::OS);
        assert_eq!(info.arch, std::env::consts::ARCH);
        assert_eq!(value.as_object().unwrap().len(), 5);

        let result = invoke(
            "validate_codey_plugin_config",
            &json!({
                "content": r#"{"_comments":{"secret-key":false},"token":"secret-value"}"#
            }),
        )
        .await
        .unwrap();
        assert_eq!(result["valid"], false);
        assert_eq!(result["error"]["code"], "invalid_comment_value");
        assert!(!result.to_string().contains("secret-key"));
        assert!(!result.to_string().contains("secret-value"));
        let result = invoke("validate_codey_plugin_config", &json!({"content":"{}"}))
            .await
            .unwrap();
        assert_eq!(result["valid"], true);
        assert!(result["error"].is_null());
    }

    #[tokio::test]
    async fn invalid_plugin_arguments_are_rejected_before_execution() {
        for (command, args) in [
            ("validate_codey_plugin_config", json!({})),
            ("validate_codey_plugin_config", json!({"content":{}})),
            (
                "set_codey_plugin_enabled",
                json!({"pluginId":"demo","enabled":"true"}),
            ),
            (
                "install_codey_plugin",
                json!({"path":"/tmp/demo.codey-plugin"}),
            ),
            ("save_codey_plugin_config_file", json!({"pluginId":"demo"})),
            ("inspect_codey_plugin", json!({"discardUpload":42})),
            ("inspect_codey_plugin", json!({"upload":{}})),
            ("inspect_codey_plugin", json!({})),
            ("get_codey_plugin_config_file", json!({"pluginId":42})),
            (
                "uninstall_codey_plugin",
                json!({"pluginId":"demo","removeData":1}),
            ),
            ("open_codey_plugin_directory", json!({"pluginId":42})),
            ("open_codey_plugin_directory", json!({})),
            ("open_codey_plugin_logs", json!({"pluginId":42})),
            ("open_codey_plugin_logs", json!({})),
            (
                "clear_codey_plugin_logs",
                json!({"pluginId":42,"confirmed":true}),
            ),
            ("clear_codey_plugin_logs", json!({"pluginId":"demo"})),
            (
                "clear_codey_plugin_logs",
                json!({"pluginId":"demo","confirmed":false}),
            ),
            (
                "clear_codey_plugin_logs",
                json!({"pluginId":"demo","confirmed":"true"}),
            ),
        ] {
            assert!(invoke(command, &args).await.is_err(), "{command}");
        }
    }
}
