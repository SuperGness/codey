#![cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]

use anyhow::Result;

#[cfg(any(windows, target_os = "macos", test))]
use anyhow::Context;

#[cfg(any(windows, target_os = "macos", test))]
use std::ffi::{OsStr, OsString};
#[cfg(any(windows, target_os = "macos"))]
use std::io::Write;

const PATCH_RESULT: &str = "codey-startup-patch-installed-v38";
const APP_SERVER_RUNTIME_OVERRIDES_VERIFIED_RESULT: &str =
    "codey-app-server-runtime-overrides-verified";
const MAX_INSPECTOR_TARGET_RESPONSE_BYTES: usize = 1024 * 1024;
pub(crate) const STARTUP_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
const STARTUP_PATCH_INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const STARTUP_PATCH_RUNTIME_OVERRIDE_INSTALL_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(24);
// 整个兼容启动复用一次发现与安装的预算，重试不重新计时。
pub(crate) const STARTUP_COMPATIBILITY_TIMEOUT: std::time::Duration =
    STARTUP_READY_TIMEOUT.saturating_add(STARTUP_PATCH_RUNTIME_OVERRIDE_INSTALL_TIMEOUT);

#[cfg(any(windows, target_os = "macos"))]
pub(crate) const CLI_WRAPPER_TARGET_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_TARGET";
#[cfg(any(windows, target_os = "macos"))]
pub(crate) const CLI_WRAPPER_OVERRIDES_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_OVERRIDES";
#[cfg(any(windows, target_os = "macos"))]
pub(crate) const CLI_WRAPPER_SUBAGENT_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_SUBAGENT";
#[cfg(any(windows, target_os = "macos"))]
pub(crate) const CLI_WRAPPER_PORT_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_PORT";
#[cfg(any(windows, target_os = "macos"))]
pub(crate) const CLI_WRAPPER_TOKEN_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_TOKEN";
#[cfg(any(windows, test))]
pub(crate) const WINDOWS_PACKAGE_RESUME_ARGUMENT: &str = "--codey-resume-packaged-app";

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

#[cfg(any(windows, target_os = "macos", test))]
pub(crate) fn is_retryable_startup_io_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
    ) || cfg!(windows) && matches!(error.raw_os_error(), Some(32 | 33))
}

#[cfg(any(windows, target_os = "macos", test))]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CliWrapperFailure {
    pub message: String,
    pub retryable: bool,
}

#[cfg(any(windows, target_os = "macos", test))]
impl std::fmt::Display for CliWrapperFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Codex CLI 兼容执行器启动失败：{}", self.message)
    }
}

#[cfg(any(windows, target_os = "macos", test))]
impl std::error::Error for CliWrapperFailure {}

#[cfg(any(windows, target_os = "macos"))]
pub(crate) const MAX_CLI_WRAPPER_FAILURE_BYTES: usize = 8 * 1024;

#[cfg(any(windows, target_os = "macos"))]
fn cli_wrapper_target(
    arguments: &[OsString],
    target: Option<OsString>,
    config_store: &crate::config::ConfigStore,
) -> Result<Option<std::path::PathBuf>> {
    if let Some(target) = target {
        return Ok(Some(target.into()));
    }
    // Browser helpers keep CODEX_CLI_PATH but can discard Codey's environment.
    // CLI arguments must never fall through to the desktop startup/cleanup path.
    let Some(first) = arguments.first() else {
        return Ok(None);
    };
    if first == "--debug-port"
        || cfg!(target_os = "macos")
            && first
                .to_str()
                .is_some_and(|value| value.starts_with("-psn_"))
    {
        return Ok(None);
    }
    let config = config_store.load().context("读取 Codex CLI 应用位置失败")?;
    let saved = config.codex_app_path.trim();
    let app_dir = codey_runtime_core::app_paths::resolve_codex_app_dir_with_saved(
        (!saved.is_empty()).then_some(std::path::Path::new(saved)),
        None,
    )
    .context("找不到有效的 Codex 桌面应用，无法转发 CLI 调用")?;
    #[cfg(windows)]
    let target = crate::launcher::windows_cli_wrapper_target(&app_dir)?;
    #[cfg(target_os = "macos")]
    let target = codey_runtime_core::app_paths::codex_runtime_executable(&app_dir)
        .context("Codex App 内未找到内置 CLI")?;
    Ok(Some(target))
}

#[cfg(any(windows, target_os = "macos"))]
pub fn run_cli_wrapper_if_requested() -> Result<bool> {
    #[cfg(windows)]
    if run_windows_package_resume_helper_if_requested()? {
        return Ok(true);
    }

    let original_args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let Some(target) = cli_wrapper_target(
        &original_args,
        std::env::var_os(CLI_WRAPPER_TARGET_ENV),
        &crate::config::ConfigStore::default(),
    )?
    else {
        return Ok(false);
    };
    let app_server = original_args
        .iter()
        .filter(|argument| argument.as_os_str() == OsStr::new("app-server"))
        .count()
        == 1;
    // 启动器只接收首次握手；之后 app-server 重启时仍必须能执行 CLI。
    let readiness = if app_server && std::env::var_os(CLI_WRAPPER_TARGET_ENV).is_some() {
        match notify_cli_wrapper_ready() {
            Ok(stream) => Some(stream),
            Err(error) => {
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_none_or(|error| error.kind() != std::io::ErrorKind::ConnectionRefused)
                {
                    crate::error_log::record_failure(
                        "compatibility_fallback",
                        "connect_cli_wrapper_handshake",
                        format!("{error:#}"),
                        serde_json::json!({}),
                    );
                }
                None
            }
        }
    } else {
        None
    };
    let launch = (|| -> Result<std::process::Child> {
        anyhow::ensure!(
            target.is_absolute(),
            "Codex CLI 兼容目标无效：{}",
            target.display()
        );
        let metadata = std::fs::metadata(&target)
            .with_context(|| format!("Codex CLI 兼容目标无效：{}", target.display()))?;
        anyhow::ensure!(
            metadata.is_file(),
            "Codex CLI 兼容目标不是文件：{}",
            target.display()
        );
        if std::fs::canonicalize(&target).ok()
            == std::env::current_exe().and_then(std::fs::canonicalize).ok()
        {
            anyhow::bail!("Codex CLI 兼容目标不能指向 Codey 自身");
        }
        let runtime_overrides = std::env::var(CLI_WRAPPER_OVERRIDES_ENV)
            .ok()
            .map(|value| serde_json::from_str::<Vec<String>>(&value))
            .transpose()
            .context("解析 Codex CLI 兼容运行时配置失败")?;
        anyhow::ensure!(
            !app_server || runtime_overrides.is_some(),
            "Codex app-server 缺少本次启动配置，已停止启动；请通过 Codey 重新启动 Codex"
        );
        let runtime_overrides = runtime_overrides.unwrap_or_default();
        let rewritten_args = rewrite_app_server_args(&original_args, &runtime_overrides)?;
        let mut command = std::process::Command::new(&target);
        command.args(rewritten_args);
        for name in [
            "CODEX_CLI_PATH",
            CLI_WRAPPER_TARGET_ENV,
            CLI_WRAPPER_OVERRIDES_ENV,
            CLI_WRAPPER_SUBAGENT_ENV,
            CLI_WRAPPER_PORT_ENV,
            CLI_WRAPPER_TOKEN_ENV,
        ] {
            command.env_remove(name);
        }
        if app_server
            && std::env::var_os(CLI_WRAPPER_SUBAGENT_ENV).as_deref() == Some(OsStr::new("1"))
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
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::process::CommandExt;
            Err(command.exec())
                .with_context(|| format!("启动 Codex CLI 失败：{}", target.display()))
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(codey_runtime_core::windows_create_no_window());
            command
                .spawn()
                .with_context(|| format!("启动 Codex CLI 失败：{}", target.display()))
        }
    })();
    let mut child = match launch {
        Ok(child) => child,
        Err(error) => {
            if let Some(mut stream) = readiness {
                // 校验或创建进程失败必须显式回传，避免被误报成握手超时。
                let failure = CliWrapperFailure {
                    message: format!("{error:#}").chars().take(1024).collect(),
                    retryable: error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(is_retryable_startup_io_error),
                };
                let _ = stream.write_all(b"!");
                let _ = serde_json::to_writer(&mut stream, &failure);
            }
            return Err(error);
        }
    };
    // macOS exec 成功由 CLOEXEC 关闭连接；Windows 只在 spawn 成功后关闭。
    drop(readiness);
    let status = child
        .wait()
        .with_context(|| format!("等待 Codex CLI 退出失败：{}", target.display()))?;
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn run_cli_wrapper_if_requested() -> Result<bool> {
    Ok(false)
}

#[cfg(windows)]
fn run_windows_package_resume_helper_if_requested() -> Result<bool> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let Some(thread_id) = windows_package_resume_thread_id(&arguments)? else {
        return Ok(false);
    };
    let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, thread_id) }
        .context("打开 Windows Store Codex 启动线程失败")?;
    let previous_suspend_count = unsafe { ResumeThread(thread) };
    let resume_error = (previous_suspend_count == u32::MAX).then(windows::core::Error::from_win32);
    unsafe { CloseHandle(thread) }.context("关闭 Windows Store Codex 启动线程句柄失败")?;
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
        "launcher.windows_package_thread_resumed",
        serde_json::json!({
            "threadId": thread_id,
            "previousSuspendCount": previous_suspend_count,
            "succeeded": resume_error.is_none(),
            "helperWrapperEnvironmentPresent": std::env::var_os(CLI_WRAPPER_TARGET_ENV).is_some(),
            "helperWslEnvironmentPresent": std::env::var_os("WSL_DISTRO_NAME").is_some(),
        }),
    );
    // 助手随即退出，确保这次恢复结果已写入磁盘。
    let _ = codey_runtime_core::diagnostic_log::flush_diagnostic_log();
    if let Some(error) = resume_error {
        return Err(error).context("恢复 Windows Store Codex 启动线程失败");
    }
    Ok(true)
}

#[cfg(any(windows, test))]
fn windows_package_resume_thread_id(arguments: &[OsString]) -> Result<Option<u32>> {
    if arguments.first().and_then(|value| value.to_str()) != Some(WINDOWS_PACKAGE_RESUME_ARGUMENT) {
        return Ok(None);
    }
    let value = arguments
        .windows(2)
        .find(|pair| {
            pair[0]
                .to_str()
                .is_some_and(|value| value.eq_ignore_ascii_case("-tid"))
        })
        .and_then(|pair| pair[1].to_str())
        .context("Windows Store 未向 Codey 传递待恢复的线程 ID")?;
    let thread_id = value
        .parse::<u32>()
        .context("Windows Store 传递了无效的线程 ID")?;
    anyhow::ensure!(thread_id != 0, "Windows Store 传递了空线程 ID");
    Ok(Some(thread_id))
}

#[cfg(any(windows, target_os = "macos"))]
fn notify_cli_wrapper_ready() -> Result<std::net::TcpStream> {
    let port = std::env::var(CLI_WRAPPER_PORT_ENV)
        .context("Codex CLI 缺少兼容校验端口")?
        .parse::<u16>()
        .context("Codex CLI 兼容校验端口无效")?;
    anyhow::ensure!(port != 0, "Codex CLI 兼容校验端口不能为 0");
    let token = std::env::var(CLI_WRAPPER_TOKEN_ENV).context("Codex CLI 缺少兼容校验令牌")?;
    anyhow::ensure!(
        !token.is_empty() && token.len() <= 128,
        "Codex CLI 兼容校验令牌无效"
    );
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream =
        std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(500))
            .context("连接 Codex CLI 兼容校验端口失败")?;
    stream
        .set_write_timeout(Some(std::time::Duration::from_millis(500)))
        .context("设置 Codex CLI 兼容校验写入时限失败")?;
    stream
        .write_all(token.as_bytes())
        .context("发送 Codex CLI 兼容校验令牌失败")?;
    Ok(stream)
}

#[cfg(any(windows, target_os = "macos", test))]
fn runtime_override_key(config: &str) -> &str {
    config.split_once('=').map_or(config, |(key, _)| key).trim()
}

#[cfg(any(windows, target_os = "macos", test))]
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

#[cfg(any(windows, target_os = "macos", test))]
pub(crate) fn local_router_runtime_enabled(overrides: &[String]) -> bool {
    overrides.iter().rev().find_map(|entry| {
        let (key, value) = entry.split_once('=')?;
        (key.trim() == "model_provider").then(|| value.trim().trim_matches(['\'', '"']))
    }) == Some(crate::local_router::ROUTER_PROVIDER_ID)
}

#[cfg(any(windows, target_os = "macos", test))]
fn rewrite_app_server_args(
    args: &[OsString],
    runtime_overrides: &[String],
) -> Result<Vec<OsString>> {
    if args
        .iter()
        .filter(|argument| argument.as_os_str() == OsStr::new("app-server"))
        .count()
        != 1
    {
        return Ok(args.to_vec());
    }

    anyhow::ensure!(
        !local_router_runtime_enabled(runtime_overrides)
            || !args.iter().any(|arg| arg == "proxy" || arg == "daemon"),
        "本地路由模式不能使用 app-server proxy/daemon；请移除自定义后台服务启动命令"
    );

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

    // Parent-table overrides from Desktop must precede the runtime fields.
    rewritten.extend(
        configs
            .into_iter()
            .flat_map(|config| [OsString::from("-c"), OsString::from(config)]),
    );
    Ok(rewritten)
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
    // 这里只访问本机 HTTP，无需同步加载系统 TLS 证书库。
    let client = reqwest::Client::builder()
        .no_proxy()
        .tls_built_in_root_certs(false)
        .timeout(std::time::Duration::from_millis(750))
        .build()?;
    let endpoint = format!("http://127.0.0.1:{port}/json/list");
    let deadline = tokio::time::Instant::now() + STARTUP_READY_TIMEOUT;
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
                            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                                "launcher.inspector_discovered",
                                serde_json::json!({ "port": port }),
                            );
                            return Ok(url.to_string());
                        }
                        last_error = "调试端口没有可连接的目标".to_string();
                    }
                    Err(error) => return Err(error.context("Codex Inspector 返回了无效的目标响应")),
                }
            }
            Ok(response) => anyhow::bail!("Codex Inspector 返回 HTTP {}", response.status()),
            Err(error) => last_error = format!("{:#}", anyhow::Error::new(error)),
        }
        tokio::time::sleep(retry_delay).await;
        retry_delay = std::cmp::min(
            retry_delay.saturating_mul(2),
            std::time::Duration::from_millis(100),
        );
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("等待 Codex 启动补丁超时：{last_error}"),
    )
    .into())
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

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn detached_cli_calls_resolve_the_saved_app_or_fail_without_starting_the_desktop() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::config::ConfigStore::new(temp.path().join("config.json"));
        #[cfg(target_os = "macos")]
        let app = temp.path().join("Codex.app");
        #[cfg(windows)]
        let app = temp.path().join("Codex");
        #[cfg(target_os = "macos")]
        let target = app.join("Contents/Resources/codex");
        #[cfg(windows)]
        let target = app.join("resources/codex.exe");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "test CLI").unwrap();
        #[cfg(windows)]
        std::fs::write(app.join("Codex.exe"), "test desktop").unwrap();
        store
            .save(&crate::config::CodeyConfig {
                codex_app_path: app.to_string_lossy().into_owned(),
                ..Default::default()
            })
            .unwrap();
        for args in [
            vec!["sandbox", "windows", "--", "node.exe"],
            vec!["-c", "key=value", "app-server"],
            vec!["exec-server"],
            vec!["--version"],
        ] {
            assert_eq!(
                cli_wrapper_target(
                    &args.iter().map(OsString::from).collect::<Vec<_>>(),
                    None,
                    &store
                )
                .unwrap(),
                Some(target.clone()),
            );
        }
        std::fs::remove_file(&target).unwrap();
        assert!(cli_wrapper_target(&["sandbox".into()], None, &store).is_err());
        assert_eq!(cli_wrapper_target(&[], None, &store).unwrap(), None);
        assert_eq!(
            cli_wrapper_target(&["--debug-port".into(), "9333".into()], None, &store).unwrap(),
            None,
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            cli_wrapper_target(&["-psn_0_1".into()], None, &store).unwrap(),
            None
        );
        std::fs::write(store.path(), "invalid config").unwrap();
        assert!(cli_wrapper_target(&["sandbox".into()], None, &store).is_err());
        assert_eq!(
            cli_wrapper_target(&[], Some(target.clone().into_os_string()), &store).unwrap(),
            Some(target),
        );
    }

    #[test]
    fn windows_package_resume_helper_requires_its_marker_and_thread_id() {
        assert_eq!(windows_package_resume_thread_id(&[]).unwrap(), None);
        assert_eq!(
            windows_package_resume_thread_id(
                &[WINDOWS_PACKAGE_RESUME_ARGUMENT, "-p", "42", "-tid", "73"].map(OsString::from)
            )
            .unwrap(),
            Some(73)
        );
        assert!(
            windows_package_resume_thread_id(
                &[WINDOWS_PACKAGE_RESUME_ARGUMENT, "-tid", "invalid"].map(OsString::from)
            )
            .is_err()
        );
    }

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
            rewrite_app_server_args(&args, &overrides).unwrap(),
            [
                "-c",
                "features.code_mode_host=true",
                "app-server",
                "-c",
                "unmanaged=true",
                "-c",
                "analytics.enabled=false",
                "-c",
                "model_provider=codey_router",
                "-c",
                "features.hooks=true",
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn router_runtime_rejects_shared_app_server_commands() {
        let router = vec!["model_provider=\"codey_router\"".to_string()];
        assert!(local_router_runtime_enabled(&router));
        assert!(!local_router_runtime_enabled(&[
            router[0].clone(),
            "model_provider=\"openai\"".to_string(),
        ]));
        for subcommand in ["proxy", "daemon"] {
            let args = ["app-server", subcommand].map(OsString::from);
            assert!(rewrite_app_server_args(&args, &router).is_err());
            assert!(rewrite_app_server_args(&args, &[]).is_ok());
        }
    }

    #[test]
    fn inspector_is_loopback_only_and_pauses_before_startup() {
        assert_eq!(inspector_argument(19321), "--inspect-brk=127.0.0.1:19321");
    }

    #[test]
    fn patch_result_is_stable_for_launch_status_validation() {
        assert_eq!(PATCH_RESULT, "codey-startup-patch-installed-v38");
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
        assert!(expression.contains("module._compile(source, filename)"));
        assert!(expression.contains("CODEY_SUBAGENT_GATE_RUNTIME_ID"));
        assert!(expression.contains("default Chinese locale"));
        assert!(expression.contains("__CODEY_DEFAULT_CHINESE_LOCALE_RENDERER_PATCH__"));
        assert!(expression.contains("spawnSync"));
        assert!(expression.contains("writeCodeyPatchFailuresAsync"));
        assert!(expression.contains("optionalPatchFailureQueue"));
        assert!(expression.contains("--codey-record-error"));
        assert!(expression.contains("codex: readCodexAppVersion()"));
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

    #[tokio::test(start_paused = true)]
    async fn inspector_discovery_accepts_cold_start_after_fifteen_seconds() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let port = reserve_loopback_port().unwrap();
        let server = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(18)).await;
            tokio::time::resume();
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .unwrap();
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 1024];
            assert_ne!(stream.read(&mut request).await.unwrap(), 0);
            let body = r#"[{"webSocketDebuggerUrl":"ws://127.0.0.1/test"}]"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        assert_eq!(
            wait_for_inspector(port).await.unwrap(),
            "ws://127.0.0.1/test"
        );
        server.await.unwrap();
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
