#![cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]

use anyhow::Result;

#[cfg(any(windows, target_os = "macos", test))]
use anyhow::Context;

#[cfg(any(windows, target_os = "macos", test))]
use std::ffi::{OsStr, OsString};
#[cfg(any(windows, target_os = "macos"))]
use std::io::Write;

const PATCH_RESULT: &str = "codey-startup-patch-installed-v39";
const APP_SERVER_RUNTIME_OVERRIDES_VERIFIED_RESULT: &str =
    "codey-app-server-runtime-overrides-verified";
const MAX_INSPECTOR_TARGET_RESPONSE_BYTES: usize = 1024 * 1024;
/// Inspector 发现窗口。fuse 允许时 Node 在应用脚本运行前就绑定端口，20 秒足以覆盖冷启动。
pub(crate) const STARTUP_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
const STARTUP_PATCH_INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const STARTUP_PATCH_RUNTIME_OVERRIDE_INSTALL_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(24);
/// 单次启动尝试等待 CLI 包装器确认的上限。进程退出、明确失败或确认成功都会提前结束；
/// Windows 最多两次尝试，清理后重新计时。
pub(crate) const STARTUP_CLI_READY_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(60);
/// 回环端口连通性探测时限（渲染进程调试端口、Inspector 端口）。
const LOOPBACK_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

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
/// 包装器执行记录文件的绝对路径；回环握手丢失时启动器据此确认目标已执行。
#[cfg(any(windows, target_os = "macos"))]
pub(crate) const CLI_WRAPPER_MARKER_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_MARKER";
#[cfg(any(windows, target_os = "macos", test))]
const CLI_WRAPPER_HANDSHAKE_CONNECT_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(500);
/// 单次回连最多 500ms；被安全软件或高负载拖慢时在 3 秒内重试，端口被拒绝则立即放弃。
#[cfg(any(windows, target_os = "macos", test))]
const CLI_WRAPPER_HANDSHAKE_CONNECT_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);
#[cfg(any(windows, target_os = "macos", test))]
const CLI_WRAPPER_HANDSHAKE_RETRY_DELAY: std::time::Duration =
    std::time::Duration::from_millis(100);
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

#[cfg(any(windows, target_os = "macos", test))]
pub(crate) const MAX_CLI_WRAPPER_MARKER_BYTES: u64 = 16 * 1024;

/// 包装器在握手端口之外留下的文件记录。回环连接被拖慢或丢失时，启动器仍能确认目标已执行，
/// 不会因为一次握手丢失就杀掉健康的 Codex。
#[cfg(any(windows, target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CliWrapperMarkerStatus {
    Launching,
    Executed,
    Failed,
}

#[cfg(any(windows, target_os = "macos", test))]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CliWrapperMarker {
    pub status: CliWrapperMarkerStatus,
    pub pid: u32,
    pub timestamp_ms: u128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

#[cfg(any(windows, target_os = "macos", test))]
impl CliWrapperMarker {
    pub(crate) fn new(status: CliWrapperMarkerStatus) -> Self {
        Self {
            status,
            pid: std::process::id(),
            timestamp_ms: crate::fs_util::timestamp_millis(),
            message: None,
            retryable: None,
        }
    }

    pub(crate) fn write(&self, path: &std::path::Path) -> Result<()> {
        crate::fs_util::atomic_write_private_with_parent(path, &serde_json::to_vec(self)?)
    }

    /// `Ok(None)` 表示文件尚不存在；内容无法解析时返回错误，由调用方决定是否继续等待。
    pub(crate) fn read(path: &std::path::Path) -> Result<Option<Self>> {
        match crate::fs_util::read_bounded(path, MAX_CLI_WRAPPER_MARKER_BYTES) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
}

/// 只接受启动器传入的绝对 JSON 路径，避免包装器把记录写到不可预期的位置。
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) fn cli_wrapper_marker_path_from_env(
    value: Option<OsString>,
) -> Option<std::path::PathBuf> {
    let path = std::path::PathBuf::from(value?);
    (path.is_absolute()
        && path
            .extension()
            .is_some_and(|extension| extension == "json"))
    .then_some(path)
}

/// 渲染进程调试端口已应答而 Inspector 端口拒绝连接：Node 环境早已创建却没有 Inspector，
/// 说明 `--inspect-brk` 被 Electron 丢弃，主进程补丁不会再有机会；进程本身在正常运行。
#[derive(Debug)]
pub(crate) struct InspectorUnavailable {
    pub refused: u32,
    pub elapsed_ms: u128,
}

impl std::fmt::Display for InspectorUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Codex 主进程 Inspector 不可用：渲染进程调试端口已就绪，但 Inspector 端口拒绝连接 {} 次（{} ms）",
            self.refused, self.elapsed_ms
        )
    }
}

impl std::error::Error for InspectorUnavailable {}

/// 启动兼容等待期间 Codex 进程已退出；无需再等待任何握手。
#[derive(Debug)]
pub(crate) struct StartupProcessExited {
    pub process_id: Option<u32>,
}

impl std::fmt::Display for StartupProcessExited {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.process_id {
            Some(process_id) => write!(
                formatter,
                "Codex 进程在启动兼容等待期间已退出（PID {process_id}）"
            ),
            None => write!(formatter, "Codex 进程在启动兼容等待期间已退出"),
        }
    }
}

impl std::error::Error for StartupProcessExited {}

pub(crate) async fn loopback_port_accepts(port: u16) -> bool {
    tokio::time::timeout(
        LOOPBACK_PROBE_TIMEOUT,
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .is_ok_and(|result| result.is_ok())
}

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
    if std::env::args_os().nth(1).as_deref() == Some(OsStr::new("--codey-route-app-server-input")) {
        let result =
            route_local_app_server_input(std::io::stdin().lock(), std::io::stdout().lock());
        if let Err(error) = result
            && error.kind() != std::io::ErrorKind::BrokenPipe
        {
            return Err(error).context("转发 Codex 本地路由请求失败");
        }
        return Ok(true);
    }
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
    let readiness = (app_server && std::env::var_os(CLI_WRAPPER_TARGET_ENV).is_some())
        .then(CliWrapperReadiness::begin);
    let mut input_router: Option<std::process::Child> = None;
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
            CLI_WRAPPER_MARKER_ENV,
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
        if app_server && local_router_runtime_enabled(&runtime_overrides) {
            // Keep macOS exec/PID semantics. The helper owns only the input pipe;
            // Desktop closing stdin ends it, including when app-server restarts.
            let mut relay = std::process::Command::new(std::env::current_exe()?);
            relay
                .arg("--codey-route-app-server-input")
                .stdout(std::process::Stdio::piped());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                relay.creation_flags(codey_runtime_core::windows_create_no_window());
            }
            let mut relay = relay.spawn().context("启动 Codex 本地路由请求转发失败")?;
            command.stdin(
                relay
                    .stdout
                    .take()
                    .context("Codex 本地路由请求管道不可用")?,
            );
            input_router = Some(relay);
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::process::CommandExt;
            // exec 成功后不再有机会写文件：先记录已执行，exec 失败时随后改写为失败。
            if let Some(readiness) = readiness.as_ref() {
                readiness.mark_executed();
            }
            let _ = codey_runtime_core::diagnostic_log::flush_diagnostic_log();
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
            if let Some(mut relay) = input_router {
                let _ = relay.kill();
                let _ = relay.wait();
            }
            if let Some(readiness) = readiness {
                // 校验或创建进程失败必须显式回传，避免被误报成握手超时。
                readiness.fail(&CliWrapperFailure {
                    message: format!("{error:#}").chars().take(1024).collect(),
                    retryable: error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(is_retryable_startup_io_error),
                });
            }
            return Err(error);
        }
    };
    // macOS exec 成功由 CLOEXEC 关闭连接；Windows 只在 spawn 成功后关闭。
    if let Some(readiness) = readiness {
        readiness.executed();
    }
    let status = child.wait();
    if let Some(mut relay) = input_router {
        let _ = relay.kill();
        let _ = relay.wait();
    }
    let status =
        status.with_context(|| format!("等待 Codex CLI 退出失败：{}", target.display()))?;
    let _ = codey_runtime_core::diagnostic_log::flush_diagnostic_log();
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(any(windows, target_os = "macos", test))]
fn route_local_app_server_input(
    mut input: impl std::io::BufRead,
    mut output: impl std::io::Write,
) -> std::io::Result<()> {
    let mut line = Vec::new();
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        if let Ok(mut message) = serde_json::from_slice::<serde_json::Value>(&line)
            && matches!(
                message.get("method").and_then(serde_json::Value::as_str),
                Some("thread/start" | "thread/resume" | "thread/fork")
            )
        {
            if message.get("params").is_none_or(serde_json::Value::is_null) {
                message["params"] = serde_json::json!({});
            }
            if let Some(params) = message
                .get_mut("params")
                .and_then(serde_json::Value::as_object_mut)
            {
                params.insert("modelProvider".into(), "codey_router".into());
                if let Some(config) = params
                    .get_mut("config")
                    .and_then(serde_json::Value::as_object_mut)
                {
                    config.retain(|key, _| {
                        key != "model_provider"
                            && !key.starts_with("model_provider.")
                            && key != "model_providers"
                            && !key.starts_with("model_providers.")
                    });
                }
                line = serde_json::to_vec(&message).map_err(std::io::Error::other)?;
                line.push(b'\n');
            }
        }
        output.write_all(&line)?;
        output.flush()?;
    }
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
/// 包装器向启动器汇报进度的两条通道：回环握手连接和记录文件。任一到达即可确认。
#[cfg(any(windows, target_os = "macos"))]
struct CliWrapperReadiness {
    stream: Option<std::net::TcpStream>,
    marker: Option<std::path::PathBuf>,
}

#[cfg(any(windows, target_os = "macos"))]
impl CliWrapperReadiness {
    fn begin() -> Self {
        let started = std::time::Instant::now();
        let marker = cli_wrapper_marker_path_from_env(std::env::var_os(CLI_WRAPPER_MARKER_ENV));
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "launcher.cli_wrapper_started",
            serde_json::json!({
                "pid": std::process::id(),
                "markerPresent": marker.is_some(),
            }),
        );
        let readiness = Self {
            stream: None,
            marker,
        };
        readiness.mark(CliWrapperMarkerStatus::Launching, None);
        let stream = match connect_cli_wrapper_handshake() {
            Ok(stream) => Some(stream),
            Err(error) => {
                // 端口被拒绝说明启动器已不再监听（例如 app-server 重启），属正常情况。
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_none_or(|error| error.kind() != std::io::ErrorKind::ConnectionRefused)
                {
                    crate::error_log::record_failure(
                        "compatibility_fallback",
                        "connect_cli_wrapper_handshake",
                        format!("{error:#}"),
                        serde_json::json!({ "markerPresent": readiness.marker.is_some() }),
                    );
                }
                None
            }
        };
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "launcher.cli_wrapper_handshake_connect",
            serde_json::json!({
                "connected": stream.is_some(),
                "elapsedMs": started.elapsed().as_millis(),
            }),
        );
        Self {
            stream,
            ..readiness
        }
    }

    fn mark(&self, status: CliWrapperMarkerStatus, failure: Option<&CliWrapperFailure>) {
        let Some(path) = self.marker.as_deref() else {
            return;
        };
        let mut marker = CliWrapperMarker::new(status);
        if let Some(failure) = failure {
            marker.message = Some(failure.message.clone());
            marker.retryable = Some(failure.retryable);
        }
        if let Err(error) = marker.write(path) {
            crate::error_log::record_failure(
                "compatibility_fallback",
                "write_cli_wrapper_marker",
                format!("{error:#}"),
                serde_json::json!({ "marker": path, "status": status }),
            );
        }
    }

    fn mark_executed(&self) {
        self.mark(CliWrapperMarkerStatus::Executed, None);
    }

    fn fail(mut self, failure: &CliWrapperFailure) {
        self.mark(CliWrapperMarkerStatus::Failed, Some(failure));
        if let Some(mut stream) = self.stream.take() {
            let _ = stream.write_all(b"!");
            let _ = serde_json::to_writer(&mut stream, failure);
        }
    }

    /// 记录已执行；随后 drop 关闭握手连接，启动器以 EOF 作为执行确认。
    fn executed(self) {
        self.mark_executed();
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn connect_cli_wrapper_handshake() -> Result<std::net::TcpStream> {
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
    let mut stream = connect_loopback_with_retry(
        &address,
        std::time::Instant::now() + CLI_WRAPPER_HANDSHAKE_CONNECT_BUDGET,
    )
    .context("连接 Codex CLI 兼容校验端口失败")?;
    stream
        .set_write_timeout(Some(CLI_WRAPPER_HANDSHAKE_CONNECT_TIMEOUT))
        .context("设置 Codex CLI 兼容校验写入时限失败")?;
    stream
        .write_all(token.as_bytes())
        .context("发送 Codex CLI 兼容校验令牌失败")?;
    Ok(stream)
}

/// 被拒绝立即返回；超时等暂时性错误在预算内重试，避免单次 500ms 连不上就静默放弃握手。
#[cfg(any(windows, target_os = "macos", test))]
fn connect_loopback_with_retry(
    address: &std::net::SocketAddr,
    deadline: std::time::Instant,
) -> std::io::Result<std::net::TcpStream> {
    connect_loopback_with_retry_using(address, deadline, std::net::TcpStream::connect_timeout)
}

#[cfg(any(windows, target_os = "macos", test))]
fn connect_loopback_with_retry_using(
    address: &std::net::SocketAddr,
    deadline: std::time::Instant,
    mut connect: impl FnMut(
        &std::net::SocketAddr,
        std::time::Duration,
    ) -> std::io::Result<std::net::TcpStream>,
) -> std::io::Result<std::net::TcpStream> {
    let mut attempts = 0_u32;
    loop {
        attempts += 1;
        match connect(address, CLI_WRAPPER_HANDSHAKE_CONNECT_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                return Err(error);
            }
            Err(error) => {
                let now = std::time::Instant::now();
                if now + CLI_WRAPPER_HANDSHAKE_RETRY_DELAY >= deadline {
                    return Err(std::io::Error::new(
                        error.kind(),
                        format!("{error}（已重试 {attempts} 次）"),
                    ));
                }
                std::thread::sleep(CLI_WRAPPER_HANDSHAKE_RETRY_DELAY);
            }
        }
    }
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
    renderer_debug_port: Option<u16>,
) -> Result<()> {
    let websocket_url = wait_for_inspector(port, renderer_debug_port).await?;
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

/// 从 reqwest 错误链里找出底层 socket 错误类型，用于区分「被拒绝」与「被拖住」。
fn connect_error_kind(error: &anyhow::Error) -> Option<std::io::ErrorKind> {
    for current in error.chain() {
        if current
            .downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_timeout)
        {
            return Some(std::io::ErrorKind::TimedOut);
        }
        if let Some(io_error) = current.downcast_ref::<std::io::Error>() {
            return Some(io_error.kind());
        }
    }
    None
}

/// 等待 Inspector 的 `/json/list`。传入渲染进程调试端口时，一旦该端口已应答而 Inspector
/// 端口仍被拒绝，立即返回 [`InspectorUnavailable`]：Chromium 的调试服务在应用脚本运行后
/// 才监听，此时 Node 环境早已创建，Inspector 不会再出现，无需耗满发现窗口。
async fn wait_for_inspector(port: u16, renderer_debug_port: Option<u16>) -> Result<String> {
    // 这里只访问本机 HTTP，无需同步加载系统 TLS 证书库。
    let client = reqwest::Client::builder()
        .no_proxy()
        .tls_built_in_root_certs(false)
        .timeout(std::time::Duration::from_millis(750))
        .build()?;
    let endpoint = format!("http://127.0.0.1:{port}/json/list");
    wait_for_inspector_with_probe(port, renderer_debug_port, || async {
        client
            .get(&endpoint)
            .send()
            .await
            .map_err(anyhow::Error::from)
    })
    .await
}

async fn wait_for_inspector_with_probe<F, Fut>(
    port: u16,
    renderer_debug_port: Option<u16>,
    mut probe: F,
) -> Result<String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<reqwest::Response>>,
{
    let started = tokio::time::Instant::now();
    let deadline = started + STARTUP_READY_TIMEOUT;
    let mut last_error = "调试端口尚未响应".to_string();
    let mut retry_delay = std::time::Duration::from_millis(20);
    let mut refused = 0_u32;
    let mut timed_out = 0_u32;
    let mut other_errors = 0_u32;
    let mut next_renderer_probe = started;

    while tokio::time::Instant::now() < deadline {
        match probe().await {
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
            Err(error) => {
                match connect_error_kind(&error) {
                    Some(std::io::ErrorKind::ConnectionRefused) => {
                        refused += 1;
                        let now = tokio::time::Instant::now();
                        if let Some(debug_port) = renderer_debug_port
                            && now >= next_renderer_probe
                        {
                            next_renderer_probe = now + std::time::Duration::from_millis(500);
                            if loopback_port_accepts(debug_port).await {
                                let elapsed_ms = started.elapsed().as_millis();
                                let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                                    "launcher.inspector_probe_summary",
                                    serde_json::json!({
                                        "port": port,
                                        "outcome": "unavailable",
                                        "refused": refused,
                                        "timedOut": timed_out,
                                        "otherErrors": other_errors,
                                        "rendererReady": true,
                                        "elapsedMs": elapsed_ms,
                                    }),
                                );
                                return Err(InspectorUnavailable {
                                    refused,
                                    elapsed_ms,
                                }
                                .into());
                            }
                        }
                    }
                    Some(std::io::ErrorKind::TimedOut) => timed_out += 1,
                    _ => other_errors += 1,
                }
                last_error = format!("{error:#}");
            }
        }
        tokio::time::sleep(retry_delay).await;
        retry_delay = std::cmp::min(
            retry_delay.saturating_mul(2),
            std::time::Duration::from_millis(100),
        );
    }

    let renderer_ready = match renderer_debug_port {
        Some(debug_port) => Some(loopback_port_accepts(debug_port).await),
        None => None,
    };
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
        "launcher.inspector_probe_summary",
        serde_json::json!({
            "port": port,
            "outcome": "timeout",
            "refused": refused,
            "timedOut": timed_out,
            "otherErrors": other_errors,
            "rendererReady": renderer_ready,
            "elapsedMs": started.elapsed().as_millis(),
        }),
    );
    let renderer_state = match renderer_ready {
        Some(true) => "，渲染进程调试端口已就绪",
        Some(false) => "，渲染进程调试端口未就绪",
        None => "",
    };
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!(
            "等待 Codex 启动补丁超时：{last_error}；连接被拒绝 {refused} 次、超时 {timed_out} 次、其他错误 {other_errors} 次{renderer_state}"
        ),
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

    #[test]
    fn cli_input_routes_thread_requests_and_preserves_other_protocol_messages() {
        for method in ["thread/start", "thread/resume", "thread/fork"] {
            let message = serde_json::json!({
                "id": 17, "method": method,
                "params": { "threadId": "old-thread", "model": "route-second/gpt-6-astra",
                    "modelProvider": null, "config": { "model_provider": "first",
                        "model_provider.name": "first", "model_providers": {"first": {}},
                        "model_providers.codey_router.base_url": "https://wrong.example",
                        "service_tier": "fast", "artifact.session": "keep" } }
            });
            let passthrough = b"{\"id\":18,\"method\":\"turn/start\",\"params\":{\"model\":\"route-second/gpt-6-astra\"}}\r\n{\"id\":19,\"result\":{}}\ninvalid-json\n";
            let mut input = serde_json::to_vec(&message).unwrap();
            input.push(b'\n');
            input.extend_from_slice(passthrough);
            let mut output = Vec::new();
            // One-byte buffers exercise split messages without changing framing.
            route_local_app_server_input(
                std::io::BufReader::with_capacity(1, input.as_slice()),
                &mut output,
            )
            .unwrap();
            let first_line = output.iter().position(|byte| *byte == b'\n').unwrap();
            let routed: serde_json::Value = serde_json::from_slice(&output[..first_line]).unwrap();
            assert_eq!(
                routed,
                serde_json::json!({ "id": 17, "method": method,
                    "params": { "threadId": "old-thread", "model": "route-second/gpt-6-astra",
                        "modelProvider": "codey_router", "config": { "service_tier": "fast", "artifact.session": "keep" } }
                })
            );
            assert_eq!(&output[first_line + 1..], passthrough);
        }
    }

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
        assert_eq!(PATCH_RESULT, "codey-startup-patch-installed-v39");
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
            wait_for_inspector(port, None).await.unwrap(),
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

    #[tokio::test]
    async fn inspector_wait_gives_up_once_the_renderer_port_answers_but_the_inspector_refuses() {
        let renderer = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let renderer_port = renderer.local_addr().unwrap().port();
        let started = std::time::Instant::now();
        // Closed loopback ports can time out on Windows before refusal arrives.
        // Supply the error explicitly so this test exercises refusal handling.
        let mut probes = 0;
        let error = wait_for_inspector_with_probe(0, Some(renderer_port), || {
            probes += 1;
            let kind = if probes == 1 {
                std::io::ErrorKind::TimedOut
            } else {
                std::io::ErrorKind::ConnectionRefused
            };
            std::future::ready(Err(std::io::Error::from(kind).into()))
        })
        .await
        .unwrap_err();
        let unavailable = error
            .downcast_ref::<InspectorUnavailable>()
            .unwrap_or_else(|| panic!("expected InspectorUnavailable, got {error:#}"));
        assert!(unavailable.refused >= 1);
        assert!(
            probes >= 2,
            "a timeout alone must not mark Inspector unavailable"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "must not wait for the full discovery window"
        );
        drop(renderer);
    }

    #[test]
    fn cli_wrapper_marker_round_trips_and_only_accepts_absolute_json_paths() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("marker.json");
        assert!(CliWrapperMarker::read(&path).unwrap().is_none());
        let mut marker = CliWrapperMarker::new(CliWrapperMarkerStatus::Failed);
        marker.message = Some("boom".to_string());
        marker.retryable = Some(true);
        marker.write(&path).unwrap();
        let read = CliWrapperMarker::read(&path).unwrap().unwrap();
        assert_eq!(read.status, CliWrapperMarkerStatus::Failed);
        assert_eq!(read.pid, std::process::id());
        assert_eq!(read.message.as_deref(), Some("boom"));
        assert_eq!(read.retryable, Some(true));
        CliWrapperMarker::new(CliWrapperMarkerStatus::Executed)
            .write(&path)
            .unwrap();
        let read = CliWrapperMarker::read(&path).unwrap().unwrap();
        assert_eq!(read.status, CliWrapperMarkerStatus::Executed);
        assert_eq!(read.message, None);
        std::fs::write(&path, "not json").unwrap();
        assert!(CliWrapperMarker::read(&path).is_err());

        assert_eq!(
            cli_wrapper_marker_path_from_env(Some(path.clone().into_os_string())),
            Some(path.clone())
        );
        assert_eq!(
            cli_wrapper_marker_path_from_env(Some(OsString::from("relative.json"))),
            None
        );
        assert_eq!(
            cli_wrapper_marker_path_from_env(Some(temp.path().join("marker.txt").into_os_string())),
            None
        );
        assert_eq!(cli_wrapper_marker_path_from_env(None), None);
    }

    #[test]
    fn handshake_connect_connects_when_listening() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let deadline = std::time::Instant::now() + CLI_WRAPPER_HANDSHAKE_CONNECT_BUDGET;
        connect_loopback_with_retry(&address, deadline).unwrap();
    }

    #[test]
    fn handshake_connect_does_not_retry_refusal() {
        let address = "127.0.0.1:1".parse().unwrap();
        let mut attempts = 0;
        let error = connect_loopback_with_retry_using(
            &address,
            std::time::Instant::now() + CLI_WRAPPER_HANDSHAKE_CONNECT_BUDGET,
            |_, _| {
                attempts += 1;
                Err(std::io::ErrorKind::ConnectionRefused.into())
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
        assert_eq!(attempts, 1, "a refused connection must not be retried");
    }

    #[test]
    fn handshake_connect_retries_timeout_and_connects_when_listening() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let mut attempts = 0;
        connect_loopback_with_retry_using(
            &address,
            std::time::Instant::now() + CLI_WRAPPER_HANDSHAKE_CONNECT_BUDGET,
            |address, timeout| {
                attempts += 1;
                if attempts == 1 {
                    Err(std::io::ErrorKind::TimedOut.into())
                } else {
                    std::net::TcpStream::connect_timeout(address, timeout)
                }
            },
        )
        .unwrap();
        assert_eq!(attempts, 2);
    }

    #[test]
    fn handshake_connect_stops_retrying_timeout_at_deadline() {
        let address = "127.0.0.1:1".parse().unwrap();
        let mut attempts = 0;
        let error =
            connect_loopback_with_retry_using(&address, std::time::Instant::now(), |_, _| {
                attempts += 1;
                Err(std::io::ErrorKind::TimedOut.into())
            })
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(attempts, 1);
    }
}
