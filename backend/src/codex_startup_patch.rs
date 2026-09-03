#![cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]

use anyhow::Result;

#[cfg(target_os = "macos")]
use anyhow::Context;

#[cfg(any(target_os = "macos", test))]
use std::ffi::{OsStr, OsString};
#[cfg(target_os = "macos")]
use std::io::Write;

const PATCH_RESULT: &str = "codey-startup-patch-installed-v37";
const APP_SERVER_RUNTIME_OVERRIDES_VERIFIED_RESULT: &str =
    "codey-app-server-runtime-overrides-verified";
const MAX_INSPECTOR_TARGET_RESPONSE_BYTES: usize = 1024 * 1024;
const STARTUP_PATCH_INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const STARTUP_PATCH_RUNTIME_OVERRIDE_INSTALL_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(24);

#[cfg(target_os = "macos")]
pub(crate) const CLI_WRAPPER_TARGET_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_TARGET";
#[cfg(target_os = "macos")]
pub(crate) const CLI_WRAPPER_OVERRIDES_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_OVERRIDES";
#[cfg(target_os = "macos")]
pub(crate) const CLI_WRAPPER_SUBAGENT_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_SUBAGENT";
#[cfg(target_os = "macos")]
pub(crate) const CLI_WRAPPER_PORT_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_PORT";
#[cfg(target_os = "macos")]
pub(crate) const CLI_WRAPPER_TOKEN_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_TOKEN";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchOptions {
    pub disable_pet: bool,
    pub subagent_gate_active: bool,
}

pub fn inspector_argument(port: u16) -> String {
    format!("--inspect-brk=127.0.0.1:{port}")
}

const STARTUP_PATCH_TEMPLATE: &str = concat!("\n", include_str!("codex_startup_patch.js"));

#[cfg(test)]
fn patch_expression(options: PatchOptions) -> String {
    patch_expression_with_runtime_overrides(options, &[])
}

#[cfg(test)]
fn patch_expression_with_runtime_overrides(
    options: PatchOptions,
    runtime_config_overrides: &[String],
) -> String {
    patch_expression_with_runtime_overrides_and_validation(options, runtime_config_overrides, false)
}

fn patch_expression_with_runtime_overrides_and_validation(
    options: PatchOptions,
    runtime_config_overrides: &[String],
    require_app_server_runtime_overrides: bool,
) -> String {
    let error_logger_executable = match std::env::current_exe() {
        Ok(path) => serde_json::to_string(&path.to_string_lossy().to_string())
            .expect("error logger executable path should serialize"),
        Err(error) => {
            crate::error_log::record_failure(
                "patch_failed",
                "resolve_error_log_helper",
                error.to_string(),
                serde_json::json!({}),
            );
            "\"\"".to_string()
        }
    };
    STARTUP_PATCH_TEMPLATE
        .replace(
            "\"__CODEY_RUNTIME_CONFIG_OVERRIDES__\"",
            &serde_json::to_string(runtime_config_overrides)
                .expect("runtime config overrides should serialize"),
        )
        .replace(
            "\"__CODEY_ERROR_LOGGER_EXECUTABLE__\"",
            &error_logger_executable,
        )
        .replace(
            "__DISABLE_PET__",
            if options.disable_pet { "true" } else { "false" },
        )
        .replace(
            "__SUBAGENT_GATE_ACTIVE__",
            if options.subagent_gate_active {
                "true"
            } else {
                "false"
            },
        )
        .replace(
            "__REQUIRE_APP_SERVER_RUNTIME_OVERRIDES__",
            if require_app_server_runtime_overrides {
                "true"
            } else {
                "false"
            },
        )
}

pub fn reserve_loopback_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

#[cfg(target_os = "macos")]
pub fn run_cli_wrapper_if_requested() -> Result<bool> {
    use std::os::unix::process::CommandExt;

    let Some(target) = std::env::var_os(CLI_WRAPPER_TARGET_ENV) else {
        return Ok(false);
    };
    let target = std::path::PathBuf::from(target);
    if !target.is_absolute() || !target.is_file() {
        anyhow::bail!("Codex CLI 兼容目标无效：{}", target.display());
    }
    if std::fs::canonicalize(&target).ok()
        == std::env::current_exe().and_then(std::fs::canonicalize).ok()
    {
        anyhow::bail!("Codex CLI 兼容目标不能指向 Codey 自身");
    }

    let runtime_overrides = std::env::var(CLI_WRAPPER_OVERRIDES_ENV)
        .ok()
        .map(|value| serde_json::from_str::<Vec<String>>(&value))
        .transpose()
        .context("解析 Codex CLI 兼容运行时配置失败")?
        .unwrap_or_default();
    let original_args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let app_server = original_args
        .iter()
        .filter(|argument| argument.as_os_str() == OsStr::new("app-server"))
        .count()
        == 1;
    let rewritten_args = rewrite_app_server_args(&original_args, &runtime_overrides);

    if app_server {
        notify_cli_wrapper_ready();
    }

    let mut command = std::process::Command::new(&target);
    command.args(rewritten_args);
    for name in [
        CLI_WRAPPER_OVERRIDES_ENV,
        CLI_WRAPPER_SUBAGENT_ENV,
        CLI_WRAPPER_PORT_ENV,
        CLI_WRAPPER_TOKEN_ENV,
    ] {
        command.env_remove(name);
    }
    if app_server && std::env::var_os(CLI_WRAPPER_SUBAGENT_ENV).as_deref() == Some(OsStr::new("1"))
    {
        command.env(crate::subagent_gate::RUNTIME_ACTIVE_ENV, "1");
        command.env(
            crate::subagent_gate::RUNTIME_ID_ENV,
            uuid::Uuid::new_v4().to_string(),
        );
    }
    if let Some(parent) = target.parent() {
        let mut paths = vec![parent.to_path_buf()];
        if let Some(path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&path));
        }
        if let Ok(path) = std::env::join_paths(paths) {
            command.env("PATH", path);
        }
    }

    Err(command.exec()).with_context(|| format!("启动 Codex CLI 失败：{}", target.display()))
}

#[cfg(not(target_os = "macos"))]
pub fn run_cli_wrapper_if_requested() -> Result<bool> {
    Ok(false)
}

#[cfg(target_os = "macos")]
fn notify_cli_wrapper_ready() {
    let Some(port) = std::env::var(CLI_WRAPPER_PORT_ENV)
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
    else {
        return;
    };
    let Some(token) = std::env::var(CLI_WRAPPER_TOKEN_ENV)
        .ok()
        .filter(|value| !value.is_empty() && value.len() <= 128)
    else {
        return;
    };
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    if let Ok(mut stream) =
        std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(500))
    {
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(500)));
        let _ = stream.write_all(token.as_bytes());
    }
}

#[cfg(any(target_os = "macos", test))]
fn runtime_override_key(config: &str) -> &str {
    config.split_once('=').map_or(config, |(key, _)| key).trim()
}

#[cfg(any(target_os = "macos", test))]
fn app_server_runtime_configs(runtime_overrides: &[String]) -> Vec<String> {
    let mut configs = vec!["analytics.enabled=false".to_string()];
    for config in runtime_overrides {
        let key = runtime_override_key(config);
        if key.is_empty() || key == "analytics.enabled" {
            continue;
        }
        if let Some(index) = configs
            .iter()
            .position(|existing| runtime_override_key(existing) == key)
        {
            configs[index] = config.clone();
        } else {
            configs.push(config.clone());
        }
    }
    configs
}

#[cfg(any(target_os = "macos", test))]
fn rewrite_app_server_args(args: &[OsString], runtime_overrides: &[String]) -> Vec<OsString> {
    if args
        .iter()
        .filter(|argument| argument.as_os_str() == OsStr::new("app-server"))
        .count()
        != 1
    {
        return args.to_vec();
    }

    let configs = app_server_runtime_configs(runtime_overrides);
    let managed_keys = configs
        .iter()
        .map(|config| runtime_override_key(config))
        .collect::<std::collections::HashSet<_>>();
    let mut rewritten = Vec::with_capacity(args.len() + configs.len() * 2);
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if argument.as_os_str() == OsStr::new("--analytics-default-enabled") {
            index += 1;
            continue;
        }
        if (argument.as_os_str() == OsStr::new("-c")
            || argument.as_os_str() == OsStr::new("--config"))
            && let Some(config) = args.get(index + 1).and_then(|value| value.to_str())
        {
            if !managed_keys.contains(runtime_override_key(config)) {
                rewritten.push(argument.clone());
                rewritten.push(args[index + 1].clone());
            }
            index += 2;
            continue;
        }
        if let Some(config) = argument
            .to_str()
            .and_then(|value| value.strip_prefix("--config="))
            && managed_keys.contains(runtime_override_key(config))
        {
            index += 1;
            continue;
        }
        rewritten.push(argument.clone());
        index += 1;
    }

    let app_server_index = rewritten
        .iter()
        .position(|argument| argument.as_os_str() == OsStr::new("app-server"))
        .expect("single app-server argument should remain");
    rewritten.splice(
        app_server_index + 1..app_server_index + 1,
        configs
            .into_iter()
            .flat_map(|config| [OsString::from("-c"), OsString::from(config)]),
    );
    rewritten
}

pub async fn install(
    port: u16,
    options: PatchOptions,
    runtime_config_overrides: &[String],
    require_app_server_runtime_overrides: bool,
) -> Result<()> {
    let websocket_url = wait_for_inspector(port).await?;
    let expression = patch_expression_with_runtime_overrides_and_validation(
        options,
        runtime_config_overrides,
        require_app_server_runtime_overrides,
    );
    let install_timeout = if require_app_server_runtime_overrides {
        STARTUP_PATCH_RUNTIME_OVERRIDE_INSTALL_TIMEOUT
    } else {
        STARTUP_PATCH_INSTALL_TIMEOUT
    };
    tokio::time::timeout(
        install_timeout,
        install_over_websocket(
            &websocket_url,
            &expression,
            require_app_server_runtime_overrides,
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Codex 启动补丁调试会话超时"))??;
    Ok(())
}

async fn wait_for_inspector(port: u16) -> Result<String> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_millis(750))
        .build()?;
    let endpoint = format!("http://127.0.0.1:{port}/json/list");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut last_error = "调试端口尚未响应".to_string();
    let mut retry_delay = std::time::Duration::from_millis(20);

    while tokio::time::Instant::now() < deadline {
        match client.get(&endpoint).send().await {
            Ok(response) if response.status().is_success() => {
                let targets = crate::http_response::read_bounded_body(
                    response,
                    MAX_INSPECTOR_TARGET_RESPONSE_BYTES,
                    "Codex Inspector 目标响应",
                )
                .await
                .and_then(|body| {
                    serde_json::from_slice::<Vec<serde_json::Value>>(&body)
                        .map_err(anyhow::Error::from)
                });
                match targets {
                    Ok(targets) => {
                        if let Some(url) = targets.iter().find_map(|target| {
                            target
                                .get("webSocketDebuggerUrl")
                                .and_then(serde_json::Value::as_str)
                        }) {
                            return Ok(url.to_string());
                        }
                        last_error = "调试端口没有可连接的目标".to_string();
                    }
                    Err(error) => last_error = error.to_string(),
                }
            }
            Ok(response) => last_error = format!("调试端口返回 HTTP {}", response.status()),
            Err(error) => last_error = error.to_string(),
        }
        tokio::time::sleep(retry_delay).await;
        retry_delay = std::cmp::min(
            retry_delay.saturating_mul(2),
            std::time::Duration::from_millis(100),
        );
    }

    anyhow::bail!("等待 Codex 启动补丁超时：{last_error}")
}

async fn install_over_websocket(
    websocket_url: &str,
    expression: &str,
    require_app_server_runtime_overrides: bool,
) -> Result<()> {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    let (mut socket, _) = tokio_tungstenite::connect_async(websocket_url).await?;
    send_command(&mut socket, 1, "Runtime.enable", serde_json::json!({})).await?;
    send_command(&mut socket, 2, "Debugger.enable", serde_json::json!({})).await?;

    let mut runtime_enabled = false;
    let mut debugger_enabled = false;
    let mut continued = false;
    let mut evaluation_sent = false;

    while let Some(message) = socket.next().await {
        let message = message?;
        let text = match message {
            Message::Text(text) => text,
            Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {
                continue;
            }
            Message::Close(_) => anyhow::bail!("Codex 启动补丁调试连接提前关闭"),
        };
        let payload: serde_json::Value = serde_json::from_str(text.as_ref())?;

        match payload.get("id").and_then(serde_json::Value::as_u64) {
            Some(1) => {
                ensure_protocol_success(&payload, "Runtime.enable")?;
                runtime_enabled = true;
            }
            Some(2) => {
                ensure_protocol_success(&payload, "Debugger.enable")?;
                debugger_enabled = true;
            }
            Some(3) => {
                ensure_protocol_success(&payload, "Runtime.runIfWaitingForDebugger")?;
            }
            Some(4) => {
                ensure_protocol_success(&payload, "Debugger.evaluateOnCallFrame")?;
                if let Some(exception) = payload
                    .get("result")
                    .and_then(|result| result.get("exceptionDetails"))
                {
                    anyhow::bail!("Codex 启动补丁执行异常：{exception}");
                }
                let value = payload
                    .pointer("/result/result/value")
                    .and_then(serde_json::Value::as_str);
                if value != Some(PATCH_RESULT) {
                    anyhow::bail!("Codex 启动补丁未返回预期状态");
                }
                send_command(&mut socket, 5, "Debugger.resume", serde_json::json!({})).await?;
            }
            Some(5) => {
                ensure_protocol_success(&payload, "Debugger.resume")?;
                if require_app_server_runtime_overrides {
                    send_command(
                        &mut socket,
                        6,
                        "Runtime.evaluate",
                        serde_json::json!({
                            "expression": "globalThis.__CODEY_AWAIT_CODEX_APP_SERVER_RUNTIME_OVERRIDES__()",
                            "awaitPromise": true,
                            "returnByValue": true,
                            "silent": false,
                        }),
                    )
                    .await?;
                    continue;
                }
                let _ = socket.close(None).await;
                return Ok(());
            }
            Some(6) => {
                ensure_protocol_success(&payload, "Runtime.evaluate")?;
                if let Some(exception) = payload
                    .get("result")
                    .and_then(|result| result.get("exceptionDetails"))
                {
                    anyhow::bail!("Codex app-server 运行时覆盖校验失败：{exception}");
                }
                let value = payload
                    .pointer("/result/result/value")
                    .and_then(serde_json::Value::as_str);
                if value != Some(APP_SERVER_RUNTIME_OVERRIDES_VERIFIED_RESULT) {
                    anyhow::bail!("Codex app-server 运行时覆盖校验未返回预期状态");
                }
                let _ = socket.close(None).await;
                return Ok(());
            }
            _ => {}
        }

        if runtime_enabled && debugger_enabled && !continued {
            continued = true;
            send_command(
                &mut socket,
                3,
                "Runtime.runIfWaitingForDebugger",
                serde_json::json!({}),
            )
            .await?;
        }

        if payload.get("method").and_then(serde_json::Value::as_str) == Some("Debugger.paused")
            && !evaluation_sent
        {
            let frame_id = payload
                .pointer("/params/callFrames/0/callFrameId")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("Codex 启动补丁没有收到可用的调用栈"))?;
            evaluation_sent = true;
            send_command(
                &mut socket,
                4,
                "Debugger.evaluateOnCallFrame",
                serde_json::json!({
                    "callFrameId": frame_id,
                    "expression": expression,
                    "returnByValue": true,
                    "silent": false,
                }),
            )
            .await?;
        }
    }

    anyhow::bail!("Codex 启动补丁调试连接未返回执行结果")
}

async fn send_command<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    let message = serde_json::json!({
        "id": id,
        "method": method,
        "params": params,
    });
    socket
        .send(Message::Text(message.to_string().into()))
        .await?;
    Ok(())
}

fn ensure_protocol_success(payload: &serde_json::Value, method: &str) -> Result<()> {
    if let Some(error) = payload.get("error") {
        anyhow::bail!("{method} 失败：{error}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_wrapper_rewrites_only_managed_app_server_configs() {
        let args = [
            "-c",
            "features.code_mode_host=true",
            "app-server",
            "--analytics-default-enabled",
            "--config",
            "model_provider=old",
            "-c",
            "unmanaged=true",
        ]
        .map(OsString::from);
        let overrides = vec![
            "model_provider=first".to_string(),
            "features.hooks=true".to_string(),
            "model_provider=codey_router".to_string(),
        ];

        assert_eq!(
            rewrite_app_server_args(&args, &overrides),
            [
                "-c",
                "features.code_mode_host=true",
                "app-server",
                "-c",
                "analytics.enabled=false",
                "-c",
                "model_provider=codey_router",
                "-c",
                "features.hooks=true",
                "-c",
                "unmanaged=true",
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn inspector_is_loopback_only_and_pauses_before_startup() {
        assert_eq!(inspector_argument(19321), "--inspect-brk=127.0.0.1:19321");
    }

    #[test]
    fn patch_result_is_stable_for_launch_status_validation() {
        assert_eq!(PATCH_RESULT, "codey-startup-patch-installed-v37");
        assert_eq!(
            APP_SERVER_RUNTIME_OVERRIDES_VERIFIED_RESULT,
            "codey-app-server-runtime-overrides-verified"
        );
        assert!(STARTUP_PATCH_RUNTIME_OVERRIDE_INSTALL_TIMEOUT > STARTUP_PATCH_INSTALL_TIMEOUT);
    }

    #[test]
    fn patch_expression_keeps_pet_slimming_voice_compatible() {
        let expression = patch_expression(PatchOptions {
            disable_pet: true,
            subagent_gate_active: true,
        });

        assert!(expression.contains("const disablePet = true"));
        assert!(
            expression
                .contains("const disableWindowsOptimizations = process.platform === \"win32\"")
        );
        assert!(expression.contains("const disableMicro = disableWindowsOptimizations"));
        assert!(expression.contains("patchCodexRendererResponse"));
        assert!(expression.contains("pet settings avatar resources"));
        assert!(expression.contains("avatarOverlayPrewarm"));
        assert!(expression.contains("__CODEY_PATCH_CODEX_AVATAR_OVERLAY_PREWARM__"));
        assert!(expression.contains("restoreNativeModelAndSpeedControls: true"));
        assert!(!expression.contains("CodeyPetBlockedBrowserWindow"));
        assert!(!expression.contains("__CODEY_DISABLED_PET_MANAGER__"));
        assert!(!expression.contains("__CODEY_PET_HARD_DISABLE_STATUS__"));
        assert!(!expression.contains("CODEY_PET_DISABLED"));
        assert!(!expression.contains("guardAvatarOverlayLifecycle"));
        assert!(!expression.contains("Codex pet window disabled by Codey"));
        assert!(expression.contains("process.getBuiltinModule(\"inspector\").close()"));
        assert!(expression.contains("disableAppServerAnalytics: true"));
        assert!(expression.contains("get disableDesktopCesAnalytics()"));
        assert!(expression.contains("analytics.enabled=false"));
        assert!(expression.contains("reconcileExternalPluginState"));
        assert!(expression.contains("get throttleExternalPluginFocusReconcile()"));
        assert!(expression.contains("get disableAppStateHeartbeat()"));
        assert!(expression.contains("get optionalMainBundlePatchFailures()"));
        assert!(expression.contains("__CODEY_MAIN_GIT_REQUEST_GUARD__"));
        assert!(expression.contains("wrapIpcHandler"));
        assert!(expression.contains("electron/main"));
        assert!(expression.contains("codey-git-request-guard-status"));
        assert!(expression.contains("get mainGitRequestGuard()"));
        assert!(expression.contains("module._compile(source, filename)"));
        assert!(expression.contains("CODEY_SUBAGENT_GATE_RUNTIME_ID"));
        assert!(expression.contains("default Chinese locale"));
        assert!(expression.contains("__CODEY_DEFAULT_CHINESE_LOCALE_RENDERER_PATCH__"));
        assert!(expression.contains("spawnSync"));
        assert!(expression.contains("writeCodeyPatchFailuresAsync"));
        assert!(expression.contains("optionalPatchFailureQueue"));
        assert!(expression.contains("--codey-record-error"));
        assert!(expression.replace("\r\n", "\n").contains(
            "setImmediate(() => {\n        try { process.getBuiltinModule(\"inspector\").close()"
        ));
        assert!(!expression.contains("__REQUIRE_APP_SERVER_RUNTIME_OVERRIDES__"));
        assert!(!expression.contains("\"__CODEY_ERROR_LOGGER_EXECUTABLE__\""));
    }

    #[test]
    fn patch_expression_embeds_runtime_config_overrides_as_json() {
        let overrides = vec![
            "features.hooks=true".to_string(),
            "developer_instructions=\"line one\\nline two\"".to_string(),
        ];
        let expression = patch_expression_with_runtime_overrides(
            PatchOptions {
                disable_pet: false,
                subagent_gate_active: true,
            },
            &overrides,
        );

        assert!(expression.contains("const codeyRuntimeConfigOverrides = ["));
        assert!(expression.contains("features.hooks=true"));
        assert!(expression.contains("developer_instructions="));
        assert!(!expression.contains("__CODEY_RUNTIME_CONFIG_OVERRIDES__"));
    }

    #[test]
    fn windows_lag_patch_only_short_circuits_the_wmi_snapshot_worker() {
        let expression = patch_expression(PatchOptions {
            disable_pet: false,
            subagent_gate_active: true,
        });

        assert!(expression.contains("process.platform === \"win32\""));
        assert!(expression.contains("isKnownWmiSnapshotWorkerName"));
        assert!(expression.contains("isKnownWmiSnapshotWorkerThreadName"));
        assert!(expression.contains("worker-option-name"));
        assert!(expression.contains("__codeyRunWmiSamplerSelfTest"));
        assert!(expression.contains("const recognizersPassed ="));
        assert!(expression.contains("selfTestPassed"));
        assert!(expression.contains("hasWmiSnapshotSourceSignature"));
        assert!(expression.contains("Get-(?:CimInstance|WmiObject)"));
        assert!(expression.contains("Win32_Process"));
        assert!(expression.contains("Win32_Perf(?:Formatted|Raw)Data_PerfProc_Process"));
        assert!(expression.contains("CodeyDisabledWmiSnapshotWorker"));
        assert!(expression.contains("this.emit(\"message\", { type: \"ok\", value: [] })"));
        assert!(expression.contains("codey-windows-wmi-sampler-status"));
        assert!(expression.contains("get windowsWmiSampler()"));
        assert!(expression.contains("super(filename, {"));
    }

    #[test]
    fn automatic_lifecycle_patch_unsubscribes_subagents_and_reclaims_node_repl() {
        let expression = patch_expression(PatchOptions {
            disable_pet: false,
            subagent_gate_active: true,
        });

        assert!(expression.contains("__CODEY_TEMP_WEBVIEW_LIFECYCLE__.close"));
        assert!(expression.contains("__CODEY_TEMP_WEBVIEW_LIFECYCLE__.track"));
        assert!(expression.contains("checkout-webview-presentation-changed"));
        assert!(expression.contains("__CODEY_INSTALL_EXECUTION_REAPER__"));
        assert!(expression.contains("const activeTurns = new Map()"));
        assert!(expression.contains("\"completed\""));
        assert!(expression.contains("\"aborted\""));
        assert!(expression.contains("reclaimAuthorizedVersion"));
        assert!(expression.contains("waitForReclaimBarrier"));
        assert!(!expression.contains("evictStaleTurns"));
        assert!(expression.contains("turnStateVersion"));
        assert!(expression.contains("__CODEY_EXECUTION_PROCESS_LIFECYCLE__"));
        assert!(expression.contains("child-process-snapshot-worker.js"));
        assert!(!expression.contains("mcpDuplicateGraceMs"));
        assert!(expression.contains("subagentThreadIds"));
        assert!(expression.contains("unsubscribeThread"));
        assert!(expression.contains("successfulThreadUnsubscribeStates"));
        assert!(expression.contains("\"notSubscribed\""));
        assert!(expression.contains("maxSubagentUnsubscribeAttempts"));
        assert!(expression.contains("isStandaloneNodeReplProcess"));
        assert!(expression.contains("processInfo?.kind === \"other\""));
        assert!(expression.contains("cua_node[/\\\\](?:bin[/\\\\])?node_repl"));
        assert!(!expression.contains("codeyMcpDuplicateReclaimScope"));
        assert!(!expression.contains("reclaimAuthorizedScope"));
        assert!(!expression.contains("rootsByIdentity"));
        assert!(expression.contains("rootChildPid"));
        assert!(!expression.contains("mcp-duplicate"));
        assert!(expression.contains("process.kill(normalizedPid, \"SIGTERM\")"));
        assert!(!expression.contains("codegraph\\.js\\s+serve"));
        assert!(!expression.contains("mcp[/\\\\]server"));
        assert!(expression.contains("node_repl"));
        assert!(!expression.contains("handlers[\"child-process-kill\"]"));
        assert!(!expression.contains("listProcessManagerSnapshot"));
    }

    #[tokio::test]
    async fn inspector_protocol_installs_stub_before_resuming() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();

            for expected_id in [1_u64, 2] {
                let message = socket.next().await.unwrap().unwrap();
                let Message::Text(text) = message else {
                    panic!("expected inspector command");
                };
                let command: serde_json::Value = serde_json::from_str(text.as_ref()).unwrap();
                assert_eq!(command["id"], expected_id);
                socket
                    .send(Message::Text(
                        serde_json::json!({"id": expected_id, "result": {}})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
            }

            let message = socket.next().await.unwrap().unwrap();
            let Message::Text(text) = message else {
                panic!("expected runIfWaitingForDebugger");
            };
            let command: serde_json::Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(command["method"], "Runtime.runIfWaitingForDebugger");
            socket
                .send(Message::Text(
                    serde_json::json!({"id": 3, "result": {}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "method": "Debugger.paused",
                        "params": {
                            "callFrames": [{"callFrameId": "frame-1"}]
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();

            let message = socket.next().await.unwrap().unwrap();
            let Message::Text(text) = message else {
                panic!("expected evaluateOnCallFrame");
            };
            let command: serde_json::Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(command["method"], "Debugger.evaluateOnCallFrame");
            assert_eq!(command["params"]["callFrameId"], "frame-1");
            let expression = command["params"]["expression"].as_str().unwrap();
            assert!(expression.contains("@worklouder/device-kit-oai"));
            assert!(expression.contains("pet settings avatar resources"));
            assert!(!expression.contains("CodeyPetBlockedBrowserWindow"));
            assert!(!expression.contains("CODEY_PET_DISABLED"));
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "id": 4,
                        "result": {
                            "result": {
                                "type": "string",
                                "value": PATCH_RESULT
                            }
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();

            let message = socket.next().await.unwrap().unwrap();
            let Message::Text(text) = message else {
                panic!("expected Debugger.resume");
            };
            let command: serde_json::Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(command["method"], "Debugger.resume");
            socket
                .send(Message::Text(
                    serde_json::json!({"id": 5, "result": {}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        });

        let expression = patch_expression(PatchOptions {
            disable_pet: true,
            subagent_gate_active: true,
        });
        install_over_websocket(&format!("ws://{address}"), &expression, false)
            .await
            .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn inspector_protocol_waits_for_app_server_runtime_override_validation() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();

            for expected_id in [1_u64, 2] {
                let message = socket.next().await.unwrap().unwrap();
                let Message::Text(text) = message else {
                    panic!("expected inspector command");
                };
                let command: serde_json::Value = serde_json::from_str(text.as_ref()).unwrap();
                assert_eq!(command["id"], expected_id);
                socket
                    .send(Message::Text(
                        serde_json::json!({"id": expected_id, "result": {}})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
            }

            let message = socket.next().await.unwrap().unwrap();
            let Message::Text(text) = message else {
                panic!("expected runIfWaitingForDebugger");
            };
            let command: serde_json::Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(command["id"], 3);
            assert_eq!(command["method"], "Runtime.runIfWaitingForDebugger");
            socket
                .send(Message::Text(
                    serde_json::json!({"id": 3, "result": {}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "method": "Debugger.paused",
                        "params": {
                            "callFrames": [{"callFrameId": "frame-1"}]
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();

            let message = socket.next().await.unwrap().unwrap();
            let Message::Text(text) = message else {
                panic!("expected evaluateOnCallFrame");
            };
            let command: serde_json::Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(command["id"], 4);
            assert_eq!(command["method"], "Debugger.evaluateOnCallFrame");
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "id": 4,
                        "result": {
                            "result": {
                                "type": "string",
                                "value": PATCH_RESULT
                            }
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();

            let message = socket.next().await.unwrap().unwrap();
            let Message::Text(text) = message else {
                panic!("expected Debugger.resume");
            };
            let command: serde_json::Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(command["id"], 5);
            assert_eq!(command["method"], "Debugger.resume");
            socket
                .send(Message::Text(
                    serde_json::json!({"id": 5, "result": {}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();

            let message = socket.next().await.unwrap().unwrap();
            let Message::Text(text) = message else {
                panic!("expected Runtime.evaluate");
            };
            let command: serde_json::Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(command["id"], 6);
            assert_eq!(command["method"], "Runtime.evaluate");
            assert_eq!(command["params"]["awaitPromise"], true);
            assert!(
                command["params"]["expression"]
                    .as_str()
                    .unwrap()
                    .contains("__CODEY_AWAIT_CODEX_APP_SERVER_RUNTIME_OVERRIDES__")
            );
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "id": 6,
                        "result": {
                            "result": {
                                "type": "string",
                                "value": APP_SERVER_RUNTIME_OVERRIDES_VERIFIED_RESULT
                            }
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        });

        let expression = patch_expression(PatchOptions {
            disable_pet: true,
            subagent_gate_active: true,
        });
        install_over_websocket(&format!("ws://{address}"), &expression, true)
            .await
            .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn inspector_protocol_fails_immediately_when_continue_is_rejected() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();

            for expected_id in [1_u64, 2] {
                let message = socket.next().await.unwrap().unwrap();
                let Message::Text(text) = message else {
                    panic!("expected inspector command");
                };
                let command: serde_json::Value = serde_json::from_str(text.as_ref()).unwrap();
                assert_eq!(command["id"], expected_id);
                socket
                    .send(Message::Text(
                        serde_json::json!({"id": expected_id, "result": {}})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
            }

            let message = socket.next().await.unwrap().unwrap();
            let Message::Text(text) = message else {
                panic!("expected runIfWaitingForDebugger");
            };
            let command: serde_json::Value = serde_json::from_str(text.as_ref()).unwrap();
            assert_eq!(command["id"], 3);
            assert_eq!(command["method"], "Runtime.runIfWaitingForDebugger");
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "id": 3,
                        "error": { "code": -32000, "message": "not waiting" }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        });

        let expression = patch_expression(PatchOptions {
            disable_pet: true,
            subagent_gate_active: true,
        });
        let error = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            install_over_websocket(&format!("ws://{address}"), &expression, false),
        )
        .await
        .expect("protocol error should not wait for the outer startup timeout")
        .expect_err("runIfWaitingForDebugger error should fail installation");
        let message = error.to_string();
        assert!(
            message.contains("Runtime.runIfWaitingForDebugger"),
            "{message}"
        );
        assert!(message.contains("not waiting"), "{message}");
        server.await.unwrap();
    }
}
