#![cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]

#[cfg(any(windows, target_os = "macos", test))]
mod stdio_relay;

use anyhow::Result;

#[cfg(any(windows, target_os = "macos", test))]
use anyhow::Context;

#[cfg(any(windows, target_os = "macos", test))]
use std::ffi::{OsStr, OsString};
#[cfg(any(windows, target_os = "macos", test))]
use std::io::Write;
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) use stdio_relay::app_server_input_relay_required;
#[cfg(any(windows, target_os = "macos", test))]
use stdio_relay::*;

/// 单次启动尝试等待 CLI 包装器确认的上限。进程退出、明确失败或确认成功都会提前结束；
/// Windows 最多两次尝试，清理后重新计时。须盖住 app-server 冷启动——Codex 先显示
/// 窗口再拉起 CLI，Windows 商店版经常要超过一分钟。
pub(crate) const STARTUP_CLI_READY_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(180);

#[cfg(any(windows, target_os = "macos"))]
pub(crate) const CLI_WRAPPER_TARGET_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_TARGET";
/// 启动器实际发现的原始 CLI；Windows 商店版的执行目标可能是用户目录副本。
#[cfg(any(windows, target_os = "macos"))]
pub(crate) const CLI_WRAPPER_SOURCE_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_SOURCE";
#[cfg(any(windows, target_os = "macos"))]
pub(crate) const CLI_WRAPPER_OVERRIDES_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_OVERRIDES";
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) const CLI_WRAPPER_MISC_MODEL_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_MISC_MODEL";
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) const CLI_WRAPPER_MISC_MODEL_FILE_ENV: &str = "CODEY_CODEX_CLI_MISC_MODEL_FILE";

pub(crate) fn runtime_misc_model_file_path() -> std::path::PathBuf {
    codey_runtime_core::paths::default_app_state_dir().join("runtime-misc-model")
}

pub(crate) fn write_runtime_misc_model(misc_model: Option<&str>) -> Result<std::path::PathBuf> {
    let path = runtime_misc_model_file_path();
    let content = misc_model.unwrap_or_default().trim();
    crate::fs_util::atomic_write_private_with_parent(&path, content.as_bytes())?;
    Ok(path)
}
/// 本次启动具备 stdin relay 的包装器路径，供主进程核对实际启动入口。
#[cfg(any(windows, target_os = "macos"))]
pub(crate) const CLI_WRAPPER_STDIN_RELAY_ENV: &str = "CODEY_CODEX_CLI_STDIN_RELAY";
#[cfg(any(windows, target_os = "macos"))]
pub(crate) const CLI_WRAPPER_SUBAGENT_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_SUBAGENT";
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) const CLI_WRAPPER_PORT_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_PORT";
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) const CLI_WRAPPER_TOKEN_ENV: &str = "CODEY_CODEX_CLI_WRAPPER_TOKEN";
/// 包装器执行记录文件的绝对路径；回环握手丢失时启动器据此确认目标已执行。
#[cfg(any(windows, target_os = "macos", test))]
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
#[cfg(any(windows, target_os = "macos", test))]
const CLI_WRAPPER_OUTPUT_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
#[cfg(any(windows, test))]
pub(crate) const WINDOWS_PACKAGE_RESUME_ARGUMENT: &str = "--codey-resume-packaged-app";

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

/// 启动兼容等待期间 Codex 进程已退出；无需再等待任何握手。
#[derive(Debug)]
pub(crate) struct StartupProcessExited {
    pub process_id: Option<u32>,
    pub exit_code: Option<u32>,
}

impl std::fmt::Display for StartupProcessExited {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.process_id {
            Some(process_id) => write!(
                formatter,
                "Codex 进程在启动兼容等待期间已退出（PID {process_id}）"
            ),
            None => write!(formatter, "Codex 进程在启动兼容等待期间已退出"),
        }?;
        if let Some(exit_code) = self.exit_code {
            write!(formatter, "，退出码 {exit_code}（0x{exit_code:08X}）")?;
        }
        Ok(())
    }
}

impl std::error::Error for StartupProcessExited {}

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
    let target =
        codey_runtime_core::app_paths::codex_runtime_executable(&app_dir).with_context(|| {
            codey_runtime_core::app_paths::codex_runtime_executable_missing(&app_dir)
        })?;
    Ok(Some(target))
}

#[cfg(target_os = "macos")]
fn run_macos_stdio_relay_helper() -> Result<()> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let mut input_fd: Option<i32> = None;
    let mut output_fd: Option<i32> = None;
    let mut args = std::env::args().skip(2);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--input-fd" => {
                let value = args.next().context("缺少 --input-fd 参数")?;
                let fd: i32 = value.parse().context("--input-fd 参数无效")?;
                anyhow::ensure!(fd > 2, "--input-fd 不能使用标准句柄");
                input_fd = Some(fd);
            }
            "--output-fd" => {
                let value = args.next().context("缺少 --output-fd 参数")?;
                let fd: i32 = value.parse().context("--output-fd 参数无效")?;
                anyhow::ensure!(fd > 2, "--output-fd 不能使用标准句柄");
                output_fd = Some(fd);
            }
            other => anyhow::bail!("未知参数：{other}"),
        }
    }
    anyhow::ensure!(
        input_fd.is_some() || output_fd.is_some(),
        "--codey-stdio-relay 至少需要一个管道句柄"
    );
    let overrides = if input_fd.is_some() {
        let raw =
            std::env::var(CLI_WRAPPER_OVERRIDES_ENV).context("Codex 请求转发缺少运行时配置")?;
        serde_json::from_str::<Vec<String>>(&raw).context("解析 Codex 请求转发配置失败")?
    } else {
        Vec::new()
    };
    let misc_model = std::env::var(CLI_WRAPPER_MISC_MODEL_ENV).ok();
    let adopt_pipe = |fd: i32| -> Result<std::fs::File> {
        // SAFETY: 父进程通过 `pre_exec` 清除 FD_CLOEXEC 并保留该独立管道句柄供本辅助进程独占接管。
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let raw = file.as_raw_fd();
        let _ = unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) };
        Ok(file)
    };
    let input_pair = input_fd.map(adopt_pipe).transpose()?.map(|writer| {
        let input: Box<dyn std::io::BufRead + Send> =
            Box::new(std::io::BufReader::new(std::io::stdin()));
        let output: Box<dyn std::io::Write + Send> = Box::new(writer);
        (input, output)
    });
    let output_pair = output_fd.map(adopt_pipe).transpose()?.map(|reader| {
        let input: Box<dyn std::io::Read + Send> = Box::new(reader);
        let output: Box<dyn std::io::Write + Send> = Box::new(std::io::stdout());
        (input, output)
    });
    let relays = InProcessStdioRelays::spawn_with_streams(
        input_pair,
        output_pair,
        crate::codex_config::codex_home().to_path_buf(),
        overrides,
        misc_model,
    )?;
    relays.wait_until_finished();
    Ok(())
}

#[cfg(any(windows, target_os = "macos"))]
pub fn run_cli_wrapper_if_requested() -> Result<bool> {
    #[cfg(target_os = "macos")]
    if std::env::args_os().nth(1).as_deref() == Some(OsStr::new("--codey-stdio-relay")) {
        if let Err(error) = run_macos_stdio_relay_helper() {
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
            {
                return Ok(true);
            }
            return Err(error);
        }
        return Ok(true);
    }
    if std::env::args_os().nth(1).as_deref()
        == Some(OsStr::new("--codey-observe-app-server-output"))
    {
        let result = observe_local_app_server_output(
            std::io::stdin().lock(),
            std::io::stdout().lock(),
            crate::codex_config::codex_home(),
        );
        if let Err(error) = result
            && error.kind() != std::io::ErrorKind::BrokenPipe
        {
            return Err(error).context("转发 Codex 会话响应失败");
        }
        return Ok(true);
    }
    if std::env::args_os().nth(1).as_deref() == Some(OsStr::new("--codey-route-app-server-input")) {
        let overrides =
            std::env::var(CLI_WRAPPER_OVERRIDES_ENV).context("Codex 请求转发缺少运行时配置")?;
        let overrides: Vec<String> =
            serde_json::from_str(&overrides).context("解析 Codex 请求转发配置失败")?;
        let misc_model = std::env::var(CLI_WRAPPER_MISC_MODEL_ENV).ok();
        let result = forward_app_server_input(
            std::io::stdin().lock(),
            std::io::stdout().lock(),
            &overrides,
            Some(crate::codex_config::codex_home()),
            local_router_runtime_enabled(&overrides),
            misc_model.as_deref(),
        );
        if let Err(error) = result
            && error.kind() != std::io::ErrorKind::BrokenPipe
        {
            return Err(error).context("转发 Codex 请求失败");
        }
        return Ok(true);
    }
    #[cfg(windows)]
    if run_windows_package_resume_helper_if_requested()? {
        return Ok(true);
    }

    let original_args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let managed_launch = std::env::var_os(CLI_WRAPPER_TARGET_ENV).is_some();
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
    let readiness = (app_server && managed_launch).then(CliWrapperReadiness::begin);
    #[cfg(target_os = "macos")]
    let mut stdio_relay: Option<std::process::Child> = None;
    #[cfg(windows)]
    let mut in_process_relays: Option<InProcessStdioRelays> = None;
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
            !app_server || !managed_launch || runtime_overrides.is_some(),
            "Codex app-server 缺少本次启动配置，已停止启动；请通过 Codey 重新启动 Codex"
        );
        // 独立浏览器调用没有启动配置，原样转发给 Codex 内置 CLI。
        let rewritten_args = match runtime_overrides.as_ref() {
            Some(overrides) => rewrite_app_server_args(&original_args, overrides)?,
            None => original_args,
        };
        let runtime_overrides = runtime_overrides.unwrap_or_default();
        let mut command = std::process::Command::new(&target);
        command.args(rewritten_args);
        for name in [
            "CODEX_CLI_PATH",
            CLI_WRAPPER_TARGET_ENV,
            CLI_WRAPPER_SOURCE_ENV,
            CLI_WRAPPER_OVERRIDES_ENV,
            CLI_WRAPPER_MISC_MODEL_ENV,
            CLI_WRAPPER_MISC_MODEL_FILE_ENV,
            CLI_WRAPPER_STDIN_RELAY_ENV,
            CLI_WRAPPER_SUBAGENT_ENV,
            CLI_WRAPPER_PORT_ENV,
            CLI_WRAPPER_TOKEN_ENV,
            CLI_WRAPPER_MARKER_ENV,
            "NODE_OPTIONS",
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
        let misc_model = std::env::var(CLI_WRAPPER_MISC_MODEL_ENV).ok();
        #[cfg(target_os = "macos")]
        {
            let relay_input = app_server
                && app_server_input_relay_required(&runtime_overrides, misc_model.as_deref());
            let observe_output = app_server && local_router_runtime_enabled(&runtime_overrides);
            if relay_input || observe_output {
                use std::os::fd::AsRawFd;
                use std::os::unix::process::CommandExt;

                // Keep macOS exec/PID semantics while merging input routing and
                // output observation into a single helper child process.
                let mut relay_cmd = std::process::Command::new(std::env::current_exe()?);
                relay_cmd.arg("--codey-stdio-relay");
                let mut helper_in_write = None;
                let mut helper_out_read = None;
                let mut inherited_fds = Vec::new();
                if relay_input {
                    let (codex_stdin, writer) =
                        std::io::pipe().context("创建 Codex 请求转发管道失败")?;
                    let fd = writer.as_raw_fd();
                    relay_cmd.args(["--input-fd", &fd.to_string()]);
                    inherited_fds.push(fd);
                    helper_in_write = Some(writer);
                    command.stdin(std::process::Stdio::from(codex_stdin));
                } else {
                    relay_cmd.stdin(std::process::Stdio::null());
                }
                if observe_output {
                    let (reader, codex_stdout) =
                        std::io::pipe().context("创建 Codex 会话响应观察管道失败")?;
                    let fd = reader.as_raw_fd();
                    relay_cmd.args(["--output-fd", &fd.to_string()]);
                    inherited_fds.push(fd);
                    helper_out_read = Some(reader);
                    command.stdout(std::process::Stdio::from(codex_stdout));
                } else {
                    relay_cmd.stdout(std::process::Stdio::null());
                }
                // SAFETY: `pre_exec` 仅对上面创建的管道句柄调用异步信号安全的 `fcntl` 清除 FD_CLOEXEC。
                unsafe {
                    relay_cmd.pre_exec(move || {
                        for &fd in &inherited_fds {
                            let flags = libc::fcntl(fd, libc::F_GETFD);
                            if flags == -1 {
                                return Err(std::io::Error::last_os_error());
                            }
                            if libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                                return Err(std::io::Error::last_os_error());
                            }
                        }
                        Ok(())
                    });
                }
                let spawned = relay_cmd.spawn().context("启动 Codex 请求与响应转发失败")?;
                stdio_relay = Some(spawned);
                drop(helper_in_write);
                drop(helper_out_read);
            }
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
            let relay_input = app_server
                && app_server_input_relay_required(&runtime_overrides, misc_model.as_deref());
            let observe_output = app_server && local_router_runtime_enabled(&runtime_overrides);
            if relay_input || observe_output {
                in_process_relays = Some(InProcessStdioRelays::start(
                    &mut command,
                    relay_input,
                    observe_output,
                    runtime_overrides,
                    misc_model,
                )?);
            }
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
            #[cfg(target_os = "macos")]
            if let Some(mut relay) = stdio_relay.take() {
                terminate_cli_child(&mut relay, "stdio 转发器");
            }
            #[cfg(windows)]
            if let Some(relays) = in_process_relays.take() {
                relays.finish(std::time::Duration::ZERO);
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
    if status.is_err() {
        terminate_cli_child(&mut child, "Codex CLI");
    }
    let drain_timeout = if status.is_ok() {
        CLI_WRAPPER_OUTPUT_DRAIN_TIMEOUT
    } else {
        std::time::Duration::ZERO
    };
    #[cfg(target_os = "macos")]
    if let Some(mut relay) = stdio_relay.take() {
        let _ = wait_for_cli_child_exit(&mut relay, drain_timeout);
        terminate_cli_child(&mut relay, "stdio 转发器");
    }
    #[cfg(windows)]
    if let Some(relays) = in_process_relays.take() {
        relays.finish(drain_timeout);
    }
    let status =
        status.with_context(|| format!("等待 Codex CLI 退出失败：{}", target.display()))?;
    let _ = codey_runtime_core::diagnostic_log::flush_diagnostic_log();
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn run_cli_wrapper_if_requested() -> Result<bool> {
    Ok(false)
}

#[cfg(windows)]
fn run_windows_package_resume_helper_if_requested() -> Result<bool> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let Some(WindowsPackageResumeTarget {
        process_id,
        thread_id,
        launch_id,
        feedback_path,
    }) = windows_package_resume_target(&arguments)?
    else {
        return Ok(false);
    };
    let result = crate::launcher::resume_windows_packaged_thread(
        process_id,
        thread_id,
        launch_id,
        &feedback_path,
    );
    if let Err(error) = &result {
        crate::error_log::record_failure(
            "package_thread_resume_failed",
            "resume_windows_packaged_thread",
            format!("{error:#}"),
            serde_json::json!({"processId": process_id, "threadId": thread_id}),
        );
    }
    let _ = codey_runtime_core::diagnostic_log::flush_diagnostic_log();
    result.map(|_| true)
}

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
struct WindowsPackageResumeTarget {
    process_id: u32,
    thread_id: u32,
    launch_id: uuid::Uuid,
    feedback_path: std::path::PathBuf,
}

#[cfg(any(windows, test))]
fn windows_package_resume_target(
    arguments: &[OsString],
) -> Result<Option<WindowsPackageResumeTarget>> {
    if arguments.first().and_then(|value| value.to_str()) != Some(WINDOWS_PACKAGE_RESUME_ARGUMENT) {
        return Ok(None);
    }
    let mut process_id = None;
    let mut thread_id = None;
    let mut launch_id = None;
    let mut feedback_path = None;
    let (pairs, remainder) = arguments[1..].as_chunks::<2>();
    for pair in pairs {
        let name = pair[0].to_str().context("Windows Store 启动参数名称无效")?;
        if name.eq_ignore_ascii_case("--launch-state") {
            anyhow::ensure!(feedback_path.is_none(), "Windows Store 启动通知路径重复");
            feedback_path = Some(std::path::PathBuf::from(&pair[1]));
            continue;
        }
        if name.eq_ignore_ascii_case("--launch-id") {
            anyhow::ensure!(launch_id.is_none(), "Windows Store 启动标识重复");
            let value = pair[1].to_str().context("Windows Store 启动标识无效")?;
            let id = uuid::Uuid::parse_str(value).context("Windows Store 启动标识无效")?;
            anyhow::ensure!(!id.is_nil(), "Windows Store 启动标识为空");
            launch_id = Some(id);
            continue;
        }
        let target = if name.eq_ignore_ascii_case("-p") {
            &mut process_id
        } else if name.eq_ignore_ascii_case("-tid") {
            &mut thread_id
        } else {
            anyhow::bail!("Windows Store 启动参数不受支持");
        };
        anyhow::ensure!(target.is_none(), "Windows Store 启动参数重复");
        let value = pair[1]
            .to_str()
            .context("Windows Store 进程或线程 ID 无效")?;
        anyhow::ensure!(
            !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
            "Windows Store 进程或线程 ID 无效"
        );
        let value = value
            .parse::<u32>()
            .context("Windows Store 进程或线程 ID 无效")?;
        anyhow::ensure!(value != 0, "Windows Store 进程或线程 ID 为空");
        *target = Some(value);
    }
    anyhow::ensure!(remainder.is_empty(), "Windows Store 启动参数缺少值");
    let launch_id = launch_id.context("Windows Store 未传递启动标识")?;
    let feedback_path = feedback_path.context("Windows Store 未传递启动通知路径")?;
    anyhow::ensure!(
        feedback_path.is_absolute()
            && feedback_path.file_name() == Some(OsStr::new(&format!("resume-{launch_id}.json"))),
        "Windows Store 启动通知路径与本次启动不匹配"
    );
    Ok(Some(WindowsPackageResumeTarget {
        process_id: process_id.context("Windows Store 未传递待恢复的进程 ID")?,
        thread_id: thread_id.context("Windows Store 未传递待恢复的线程 ID")?,
        launch_id,
        feedback_path,
    }))
}

/// 包装器向启动器汇报进度的两条通道：回环握手连接和记录文件。任一到达即可确认。
#[cfg(any(windows, target_os = "macos", test))]
struct CliWrapperReadiness {
    stream: Option<std::net::TcpStream>,
    marker: Option<std::path::PathBuf>,
    tcp_deferred: bool,
}

#[cfg(any(windows, target_os = "macos", test))]
impl CliWrapperReadiness {
    fn begin() -> Self {
        let started = std::time::Instant::now();
        let marker = cli_wrapper_marker_path_from_env(std::env::var_os(CLI_WRAPPER_MARKER_ENV));
        Self::begin_with_diagnostics(
            marker,
            started,
            cfg!(windows),
            connect_cli_wrapper_handshake,
        )
    }

    #[cfg(test)]
    fn begin_with(
        marker: Option<std::path::PathBuf>,
        prefer_marker: bool,
        connect: impl FnOnce() -> Result<std::net::TcpStream>,
    ) -> Self {
        Self::begin_with_diagnostics(marker, std::time::Instant::now(), prefer_marker, connect)
    }

    fn begin_with_diagnostics(
        marker: Option<std::path::PathBuf>,
        started: std::time::Instant,
        prefer_marker: bool,
        connect: impl FnOnce() -> Result<std::net::TcpStream>,
    ) -> Self {
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
            tcp_deferred: false,
        };
        let marker_written = readiness.mark(CliWrapperMarkerStatus::Launching, None);
        // Windows 回环连接可能受网络隔离影响。只有本次记录写入成功才跳过 TCP，
        // 启动器仍须等到 Executed 或 Failed；路径已配置不代表文件通道可用。
        let tcp_required = !prefer_marker || !marker_written;
        let stream = if tcp_required {
            readiness.connect_with_diagnostics(marker_written, connect)
        } else {
            None
        };
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "launcher.cli_wrapper_handshake_connect",
            serde_json::json!({
                "connected": stream.is_some(),
                "tcpAttempted": tcp_required,
                "markerWritten": marker_written,
                "elapsedMs": started.elapsed().as_millis(),
            }),
        );
        Self {
            stream,
            tcp_deferred: !tcp_required,
            ..readiness
        }
    }

    fn connect_with_diagnostics(
        &self,
        marker_written: bool,
        connect: impl FnOnce() -> Result<std::net::TcpStream>,
    ) -> Option<std::net::TcpStream> {
        match connect() {
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
                        serde_json::json!({
                            "markerPresent": self.marker.is_some(),
                            "markerWritten": marker_written,
                        }),
                    );
                }
                None
            }
        }
    }

    fn mark(&self, status: CliWrapperMarkerStatus, failure: Option<&CliWrapperFailure>) -> bool {
        let Some(path) = self.marker.as_deref() else {
            return false;
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
            return false;
        }
        true
    }

    fn mark_executed(&self) -> bool {
        self.mark(CliWrapperMarkerStatus::Executed, None)
    }

    fn fail(self, failure: &CliWrapperFailure) {
        self.finish_with(Some(failure), connect_cli_wrapper_handshake);
    }

    /// 记录已执行；随后 drop 关闭握手连接，启动器以 EOF 作为执行确认。
    fn executed(self) {
        self.finish_with(None, connect_cli_wrapper_handshake);
    }

    fn finish_with(
        mut self,
        failure: Option<&CliWrapperFailure>,
        connect: impl FnOnce() -> Result<std::net::TcpStream>,
    ) {
        let marker_written = match failure {
            Some(failure) => self.mark(CliWrapperMarkerStatus::Failed, Some(failure)),
            None => self.mark_executed(),
        };
        // 初次写入成功后，文件仍可能被占用。仅在此前没有尝试 TCP 时补一次有界连接，
        // 避免丢失最终状态，也不会叠加两次连接重试预算。
        if !marker_written && self.tcp_deferred {
            let started = std::time::Instant::now();
            self.stream = self.connect_with_diagnostics(false, connect);
            let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
                "launcher.cli_wrapper_handshake_fallback",
                serde_json::json!({
                    "connected": self.stream.is_some(),
                    "elapsedMs": started.elapsed().as_millis(),
                }),
            );
        }
        if let Some(failure) = failure
            && let Some(mut stream) = self.stream.take()
        {
            let _ = stream.write_all(b"!");
            let _ = serde_json::to_writer(&mut stream, failure);
        }
    }
}

#[cfg(any(windows, target_os = "macos", test))]
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
    runtime_override_string(overrides, "model_provider")
        .is_some_and(|provider| crate::local_router::is_router_provider(&provider))
}

#[cfg(any(windows, target_os = "macos", test))]
fn runtime_override_string(overrides: &[String], name: &str) -> Option<String> {
    let (_, value) = overrides
        .iter()
        .rev()
        .filter_map(|entry| entry.split_once('='))
        .find(|(key, _)| key.trim() == name)?;
    match format!("value={value}").parse::<toml_edit::DocumentMut>() {
        Ok(document) => document.get("value")?.as_str().map(str::to_string),
        // Codex also accepts bare strings in CLI configuration overrides.
        Err(_) => Some(value.trim().to_string()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_helper_process_fixture() {
        let Ok(role) = std::env::var("CODEY_TEST_CLI_HELPER_ROLE") else {
            return;
        };
        std::fs::write(
            std::env::var_os("CODEY_TEST_CLI_HELPER_READY").unwrap(),
            b"ready",
        )
        .unwrap();
        if role == "observer" {
            observe_local_app_server_output(
                std::io::stdin().lock(),
                std::io::stdout().lock(),
                &std::path::PathBuf::from(std::env::var_os("CODEY_TEST_CLI_HELPER_HOME").unwrap()),
            )
            .unwrap();
        } else {
            // 保留继承的 stdout 写句柄，直到父进程关闭 stdin。
            std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink()).unwrap();
        }
        std::process::exit(0);
    }

    struct CliHelperFixture(Option<std::process::Child>);

    impl Drop for CliHelperFixture {
        fn drop(&mut self) {
            if let Some(child) = self.0.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    fn spawn_cli_helper_fixture(
        home: &std::path::Path,
        role: &str,
        stdout: std::process::Stdio,
    ) -> CliHelperFixture {
        let ready = home.join(uuid::Uuid::new_v4().to_string());
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "codex_startup_patch::tests::cli_helper_process_fixture",
                "--nocapture",
            ])
            .env("CODEY_TEST_CLI_HELPER_ROLE", role)
            .env("CODEY_TEST_CLI_HELPER_READY", &ready)
            .env("CODEY_TEST_CLI_HELPER_HOME", home)
            .stdin(std::process::Stdio::piped())
            .stdout(stdout)
            .spawn()
            .unwrap();
        let mut fixture = CliHelperFixture(Some(child));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready.exists() {
            assert!(fixture.0.as_mut().unwrap().try_wait().unwrap().is_none());
            assert!(std::time::Instant::now() < deadline, "helper did not start");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        fixture
    }

    #[test]
    fn cli_helper_cleanup_drains_and_persists_the_final_notification() {
        use std::io::Read;
        let home = tempfile::tempdir().unwrap();
        let mut observer =
            spawn_cli_helper_fixture(home.path(), "observer", std::process::Stdio::piped());
        let notification = br#"{"method":"thread/settings/updated","params":{"threadId":"last","threadSettings":{"model":"route/final"}}}"#;
        let mut input = observer.0.as_mut().unwrap().stdin.take().unwrap();
        input.write_all(notification).unwrap();
        drop(input);

        finish_cli_wrapper_helpers(&mut None, &mut observer.0, CLI_WRAPPER_OUTPUT_DRAIN_TIMEOUT);

        let child = observer.0.as_mut().unwrap();
        assert!(child.try_wait().unwrap().unwrap().success());
        let mut output = Vec::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut output)
            .unwrap();
        assert!(output.ends_with(notification));
        assert_eq!(
            crate::session_model::read(home.path(), "last").unwrap()["model"],
            "route/final"
        );
    }

    #[test]
    fn cli_helper_cleanup_reaps_helpers_while_another_process_holds_the_output_pipe() {
        let home = tempfile::tempdir().unwrap();
        let mut observer =
            spawn_cli_helper_fixture(home.path(), "observer", std::process::Stdio::null());
        let writer = observer.0.as_mut().unwrap().stdin.take().unwrap();
        let mut holder = spawn_cli_helper_fixture(home.path(), "holder", writer.into());
        let mut relay = spawn_cli_helper_fixture(home.path(), "relay", std::process::Stdio::null());
        assert!(observer.0.as_mut().unwrap().try_wait().unwrap().is_none());

        let started = std::time::Instant::now();
        finish_cli_wrapper_helpers(
            &mut relay.0,
            &mut observer.0,
            std::time::Duration::from_millis(100),
        );

        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        assert!(relay.0.as_mut().unwrap().try_wait().unwrap().is_some());
        assert!(observer.0.as_mut().unwrap().try_wait().unwrap().is_some());
        assert!(holder.0.as_mut().unwrap().try_wait().unwrap().is_none());
    }

    #[test]
    fn output_observer_preserves_bytes_and_persists_only_confirmed_settings() {
        struct SplitRead<'a>(&'a [u8]);
        impl std::io::Read for SplitRead<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                std::io::Read::read(&mut self.0, &mut buffer[..1])
            }
        }
        let home = tempfile::tempdir().unwrap();
        let notification = |id: &str, model: &str| {
            serde_json::json!({
                "method":"thread/settings/updated", "emittedAtMs":chrono::Utc::now().timestamp_millis(),
                "params":{"threadId":id,"threadSettings":{"model":model,"modelProvider":"codey_router"}}
            })
        };
        let mut requested = notification("requested", "route/unconfirmed");
        requested["method"] = "thread/settings/update".into();
        requested["id"] = 9.into();
        let mut invalid_request = notification("with-id", "route/unconfirmed");
        invalid_request["id"] = 10.into();
        let input = format!(
            "invalid\r\n{{\"id\":9,\"error\":{{\"message\":\"failed\"}}}}\n{}\n{}\n{}\n{}\r\n{}",
            requested,
            invalid_request,
            notification("blank", "  "),
            notification("first", "route/first"),
            notification("second", "route/second")
        );
        let mut output = Vec::new();
        observe_local_app_server_output(SplitRead(input.as_bytes()), &mut output, home.path())
            .unwrap();
        assert_eq!(output, input.as_bytes());
        for (id, model) in [("first", "route/first"), ("second", "route/second")] {
            let saved = crate::session_model::read(home.path(), id).unwrap();
            assert_eq!(saved["model"], model);
            assert_eq!(saved["modelProvider"], "codey_router");
        }
        for id in ["requested", "with-id", "blank"] {
            assert_eq!(
                crate::session_model::read(home.path(), id).unwrap()["status"],
                "missing"
            );
        }
    }

    #[test]
    fn output_observer_preserves_runtime_provider_for_main_and_page_resume() {
        struct SplitRead<'a>(&'a [u8]);
        impl std::io::Read for SplitRead<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let end = 7.min(buffer.len());
                std::io::Read::read(&mut self.0, &mut buffer[..end])
            }
        }
        let home = tempfile::tempdir().unwrap();
        let resume = |id: serde_json::Value, model: &str, provider: &str| {
            serde_json::json!({"id":id,"result":{"thread":{"id":"t",
                "model":"historical/model","modelProvider":"historical-provider","turns":[]},
                "model":model,"modelProvider":provider,"serviceTier":null,
                "reasoningEffort":"high","cwd":"/workspace/project"}})
        };
        let official = "codey-official-account-a1/gpt-6-astra";
        // 主进程预测和页面 AppServerRequestClient 使用相同的 UUID 请求 ID 格式。
        let internal = resume(
            "thread/resume:3b771392-5499-4e6d-b053-464bac001422".into(),
            official,
            "codey_router",
        );
        let mut page = resume(
            "thread/resume:adeea1ac-2707-4580-a4ee-8cadb48187e0".into(),
            official,
            "codey_router_remote",
        );
        page["result"]["thread"]["turns"] = serde_json::json!([{
            "id":"old-turn","model":"historical/other-model","modelProvider":"other-provider",
            "items":[{"type":"userMessage","content":[{"type":"text","text":"保留历史"}]}]
        }]);
        let mut large = page.clone();
        large["result"]["thread"]["preview"] = "z".repeat(PREDICTION_RESUME_LINE_LIMIT + 1).into();
        let unchanged = [
            resume(
                "thread/resume:2".into(),
                "third-party/model",
                "codey_router",
            ),
            resume(3.into(), official, "codey_router"),
            resume("thread/resume:4".into(), official, "other"),
            resume("thread/resume:5".into(), "gpt-6-astra", "codey_router"),
            resume(
                "thread/fork:68756161-c580-4637-b424-fc4056064ce5".into(),
                official,
                "codey_router",
            ),
            resume("thread/resume:6".into(), official, "openai"),
            large,
        ];
        let input = format!(
            "{}\r\n{}\n{}\n{}",
            internal,
            page,
            unchanged
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\r\n"),
            "y".repeat(PREDICTION_RESUME_LINE_LIMIT + 1)
        );
        let mut output = Vec::new();
        observe_local_app_server_output(SplitRead(input.as_bytes()), &mut output, home.path())
            .unwrap();
        let output = String::from_utf8(output).unwrap();
        let (first, rest) = output.split_once('\n').unwrap();
        let (second, rest) = rest.split_once('\n').unwrap();
        for (line, mut expected) in [(first, internal), (second, page)] {
            let actual: serde_json::Value = serde_json::from_str(line).unwrap();
            let runtime_provider = expected["result"]["modelProvider"].clone();
            expected["result"]["codeyRuntimeModelProvider"] = runtime_provider;
            expected["result"]["modelProvider"] = "openai".into();
            assert_eq!(
                actual, expected,
                "all other response fields must be preserved"
            );
        }
        assert_eq!(
            rest,
            input.splitn(3, '\n').nth(2).unwrap(),
            "other responses and large lines must pass through byte-for-byte"
        );
    }

    #[test]
    fn output_observer_keeps_forwarding_after_storage_failure_and_large_messages() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("codey-session-models"), b"blocked").unwrap();
        let notification = serde_json::json!({"method":"thread/settings/updated",
            "params":{"threadId":"first","threadSettings":{"model":"route/first"}}});
        let input = format!(
            "{}\n{}\n{{\"id\":1,\"result\":{{}}}}\n",
            "x".repeat(1024 * 1024 + 1),
            notification
        );
        let mut output = Vec::new();
        observe_local_app_server_output(input.as_bytes(), &mut output, home.path()).unwrap();
        assert_eq!(output, input.as_bytes());
    }

    #[test]
    fn confirmed_effort_restores_with_model_and_preserves_other_config() {
        let home = tempfile::tempdir().unwrap();
        let db = rusqlite::Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE threads (id TEXT, model_provider TEXT);
            INSERT INTO threads VALUES ('saved', 'codey_router');",
        )
        .unwrap();
        let catalog = home.path().join("catalog.json");
        std::fs::write(&catalog, r#"{"models":[{"slug":"route/model"}]}"#).unwrap();
        let overrides = vec![format!(
            "model_catalog_json={}",
            toml_edit::Value::from(catalog.to_str().unwrap())
        )];
        for (settings, expected) in [
            (
                serde_json::json!({"effort":"high"}),
                Some(serde_json::json!("high")),
            ),
            (
                serde_json::json!({"effort":null,"collaborationMode":{"settings":{"reasoning_effort":"low"}}}),
                None,
            ),
            (
                serde_json::json!({"collaborationMode":{"settings":{"reasoning_effort":"low"}}}),
                Some(serde_json::json!("low")),
            ),
            (serde_json::json!({}), Some(serde_json::json!("medium"))),
        ] {
            let mut settings = settings;
            settings["model"] = "route/model".into();
            remember_app_server_settings(&serde_json::to_vec(&serde_json::json!({
                "method":"thread/settings/updated", "params":{"threadId":"saved","threadSettings":settings}
            })).unwrap(), home.path());
            let input = b"{\"method\":\"thread/resume\",\"params\":{\"threadId\":\"saved\",\"model\":null,\"config\":{\"model_reasoning_effort\":\"medium\",\"other\":true}}}\n";
            let mut output = Vec::new();
            route_local_app_server_input(
                input.as_slice(),
                &mut output,
                &overrides,
                Some(home.path()),
            )
            .unwrap();
            let result: serde_json::Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(result["params"]["model"], "route/model");
            assert_eq!(result["params"]["config"]["other"], true);
            assert_eq!(
                result["params"]["config"]
                    .get("model_reasoning_effort")
                    .cloned(),
                expected
            );
        }
    }

    #[test]
    fn cli_resume_recovers_each_rollout_model_without_changing_explicit_or_new_models() {
        let home = tempfile::tempdir().unwrap();
        let db = rusqlite::Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE threads (id TEXT, model TEXT, model_provider TEXT, rollout_path TEXT);
            INSERT INTO threads VALUES ('first', 'route/default', 'codey_router', 'first.jsonl');
            INSERT INTO threads VALUES ('second', 'route/default', 'codey_router', 'second.jsonl');").unwrap();
        for (id, model) in [("first", "route/first"), ("second", "route/second")] {
            std::fs::write(home.path().join(format!("{id}.jsonl")), format!("{}\n{}\n",
                serde_json::json!({"type":"session_meta","payload":{"id":id,"model_provider":"codey_router"}}),
                serde_json::json!({"type":"turn_context","payload":{"model":model}}))).unwrap();
        }
        let catalog = home.path().join("catalog.json");
        std::fs::write(&catalog, serde_json::json!({"models":[{"slug":"route/first"},{"slug":"route/second"},{"slug":"route/default"}]}).to_string()).unwrap();
        let overrides = vec![format!(
            "model_catalog_json={}",
            toml_edit::Value::from(catalog.to_str().unwrap())
        )];
        for (method, thread, model, expected) in [
            (
                "thread/resume",
                "first",
                serde_json::Value::Null,
                serde_json::json!("route/first"),
            ),
            (
                "thread/resume",
                "second",
                serde_json::json!("  "),
                serde_json::json!("route/second"),
            ),
            (
                "thread/resume",
                "first",
                serde_json::json!("route/default"),
                serde_json::json!("route/default"),
            ),
            (
                "thread/resume",
                "missing",
                serde_json::Value::Null,
                serde_json::Value::Null,
            ),
            (
                "thread/start",
                "first",
                serde_json::Value::Null,
                serde_json::Value::Null,
            ),
            (
                "thread/fork",
                "first",
                serde_json::Value::Null,
                serde_json::Value::Null,
            ),
        ] {
            let input = format!(
                "{}\n{{\"id\":2,\"result\":{{}}}}\r\ninvalid\n",
                serde_json::json!({"id":1,"method":method,"params":{"threadId":thread,"model":model}})
            );
            let mut output = Vec::new();
            route_local_app_server_input(
                std::io::BufReader::with_capacity(1, input.as_bytes()),
                &mut output,
                &overrides,
                Some(home.path()),
            )
            .unwrap();
            let newline = output.iter().position(|byte| *byte == b'\n').unwrap();
            let result: serde_json::Value = serde_json::from_slice(&output[..newline]).unwrap();
            assert_eq!(result["params"]["model"], expected);
            assert_eq!(
                &output[newline + 1..],
                b"{\"id\":2,\"result\":{}}\r\ninvalid\n"
            );
        }
        // 活跃模型不被较旧的磁盘记录覆盖。
        for method in ["thread/settings/update", "turn/start", "thread/resume"] {
            let input = format!(
                "{}\n{}\n",
                serde_json::json!({"id":1,"method":method,"params":{"threadId":"first","model":"route/second"}}),
                serde_json::json!({"id":2,"method":"thread/resume","params":{"threadId":"first"}})
            );
            let mut output = Vec::new();
            route_local_app_server_input(
                input.as_bytes(),
                &mut output,
                &overrides,
                Some(home.path()),
            )
            .unwrap();
            let lines = String::from_utf8(output).unwrap();
            let result: serde_json::Value =
                serde_json::from_str(lines.lines().nth(1).unwrap()).unwrap();
            assert!(result["params"].get("model").is_none());
        }
        // 损坏的元数据不能中断共享协议流。
        std::fs::write(home.path().join("first.jsonl"), "invalid").unwrap();
        let mut output = Vec::new();
        route_local_app_server_input(b"{\"method\":\"thread/resume\",\"params\":{\"threadId\":\"first\",\"model\":null}}\n{}\n".as_slice(), &mut output, &overrides, Some(home.path())).unwrap();
        assert!(String::from_utf8(output).unwrap().ends_with("\n{}\n"));
    }

    #[test]
    fn cli_resume_uses_the_indexed_carrier_before_restoring_a_router_model() {
        let home = tempfile::tempdir().unwrap();
        let db = rusqlite::Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE threads (id TEXT, model TEXT, model_provider TEXT, rollout_path TEXT);
            INSERT INTO threads VALUES ('old', 'native/saved', 'openai', 'old.jsonl');
            INSERT INTO threads VALUES ('native', 'native/saved', 'openai', NULL);
            INSERT INTO threads VALUES ('conflict', 'native/saved', 'openai', NULL);
            INSERT INTO threads VALUES ('routed', 'route/saved', 'codey_router_remote', NULL);",
        )
        .unwrap();
        let sqlite_dir = home.path().join("sqlite");
        std::fs::create_dir(&sqlite_dir).unwrap();
        rusqlite::Connection::open(sqlite_dir.join("codex-dev.db"))
            .unwrap()
            .execute_batch(
                "CREATE TABLE threads (id TEXT, model_provider TEXT);
                 INSERT INTO threads VALUES ('conflict', 'codey_router');",
            )
            .unwrap();
        std::fs::write(home.path().join("old.jsonl"), concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"old\",\"model_provider\":\"codey_router\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"route/saved\",\"effort\":\"high\"}}\n",
        )).unwrap();
        let catalog = home.path().join("catalog.json");
        std::fs::write(&catalog, r#"{"models":[{"slug":"route/saved"}]}"#).unwrap();
        let overrides = vec![
            "model=\"route/default\"".into(),
            format!(
                "model_catalog_json={}",
                toml_edit::Value::from(catalog.to_str().unwrap())
            ),
        ];
        for (id, input, provider, model, config_override) in [
            ("old", serde_json::json!({}), None, None, None),
            (
                "old",
                serde_json::json!({"model":"route/default"}),
                None,
                None,
                None,
            ),
            (
                "old",
                serde_json::json!({"model":"org/native-model"}),
                Some("openai"),
                Some("org/native-model"),
                None,
            ),
            (
                "routed",
                serde_json::json!({}),
                Some("codey_router_remote"),
                Some("route/saved"),
                None,
            ),
            (
                "native",
                serde_json::json!({"config":{"model_reasoning_effort":"high"}}),
                Some("openai"),
                Some("native/saved"),
                Some(("model_reasoning_effort", "high")),
            ),
            (
                "old",
                serde_json::json!({"config":{"model":"org/native-model"}}),
                Some("openai"),
                None,
                Some(("model", "org/native-model")),
            ),
            (
                "old",
                serde_json::json!({"config":{"model":"route/default","model_reasoning_effort":"high"}}),
                None,
                None,
                None,
            ),
            (
                "missing",
                serde_json::json!({"model":"route/saved","config":{"model":"route/saved","model_reasoning_effort":"high"}}),
                None,
                None,
                None,
            ),
            (
                "conflict",
                serde_json::json!({"model":"route/saved","config":{"model_reasoning_effort":"high"}}),
                None,
                None,
                None,
            ),
            (
                "old",
                serde_json::json!({"path":"other.jsonl","model":"route/saved"}),
                None,
                None,
                None,
            ),
            (
                "old",
                serde_json::json!({"history":[],"model":"route/saved"}),
                None,
                None,
                None,
            ),
        ] {
            let mut params = input;
            params["threadId"] = id.into();
            params["modelProvider"] = "untrusted".into();
            params["config"]["model_provider"] = "untrusted".into();
            params["config"]["model_providers.codey_router.name"] = "override".into();
            params["config"]["features.hooks"] = false.into();
            let request = serde_json::json!({"method":"thread/resume","params":params});
            let mut output = Vec::new();
            route_local_app_server_input(
                request.to_string().as_bytes(),
                &mut output,
                &overrides,
                Some(home.path()),
            )
            .unwrap();
            let result: serde_json::Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(result["params"]["modelProvider"].as_str(), provider);
            assert_eq!(result["params"]["model"].as_str(), model);
            let mut config = serde_json::json!({"features.hooks":false});
            if let Some((key, value)) = config_override {
                config[key] = value.into();
            }
            assert_eq!(result["params"]["config"], config);
            assert_eq!(result["params"].get("path"), params.get("path"));
            assert_eq!(result["params"].get("history"), params.get("history"));
        }
    }

    #[test]
    fn resume_model_alias_resolution_never_guesses_between_routes() {
        let home = tempfile::tempdir().unwrap();
        let catalog = home.path().join("catalog.json");
        std::fs::write(&catalog, serde_json::json!({"models":[{"slug":"a/shared"},{"slug":"b/shared"},{"slug":"a/unique"}]}).to_string()).unwrap();
        let overrides = vec![format!(
            "model_catalog_json={}",
            toml_edit::Value::from(catalog.to_str().unwrap())
        )];
        for (model, provider, expected) in [
            ("shared", "codey_router", None),
            ("shared", "b", Some("b/shared")),
            ("a/shared", "b", Some("a/shared")),
            ("unique", "codey_router", Some("a/unique")),
            ("unique", "b", None),
            ("deleted/shared", "codey_router", None),
        ] {
            assert_eq!(
                stdio_relay::resolve_resume_model(
                    &serde_json::json!({
                        "model": model, "modelProvider": provider
                    }),
                    &overrides
                )
                .unwrap()
                .as_deref(),
                expected
            );
        }
    }

    #[test]
    fn windows_cli_wrapper_confirms_execution_and_failure_through_written_marker() {
        for failure in [None, Some(false), Some(true)] {
            let temporary = tempfile::tempdir().unwrap();
            let marker_path = temporary.path().join("wrapper.json");
            assert!(!marker_path.exists());
            let readiness =
                CliWrapperReadiness::begin_with(Some(marker_path.clone()), true, || {
                    panic!("a successfully written Windows marker must avoid the TCP connection")
                });
            assert!(readiness.stream.is_none());
            assert_eq!(
                CliWrapperMarker::read(&marker_path)
                    .unwrap()
                    .unwrap()
                    .status,
                CliWrapperMarkerStatus::Launching
            );
            if let Some(retryable) = failure {
                readiness.fail(&CliWrapperFailure {
                    message: "CLI process creation failed".into(),
                    retryable,
                });
            } else {
                readiness.executed();
            }
            let marker = CliWrapperMarker::read(&marker_path).unwrap().unwrap();
            assert_eq!(marker.retryable, failure);
            if failure.is_some() {
                assert_eq!(marker.status, CliWrapperMarkerStatus::Failed);
                assert_eq!(
                    marker.message.as_deref(),
                    Some("CLI process creation failed")
                );
            } else {
                assert_eq!(marker.status, CliWrapperMarkerStatus::Executed);
                assert_eq!(marker.message, None);
            }
        }
    }

    #[test]
    fn windows_cli_wrapper_keeps_tcp_confirmation_when_marker_missing_or_unwritable() {
        use std::io::Read;

        let temporary = tempfile::tempdir().unwrap();
        let blocked_destination = temporary.path().join("directory.json");
        std::fs::create_dir(&blocked_destination).unwrap();
        let blocked_parent = temporary.path().join("file");
        std::fs::write(&blocked_parent, b"not a directory").unwrap();
        for marker in [
            None,
            Some(blocked_destination),
            Some(blocked_parent.join("wrapper.json")),
        ] {
            for failure in [None, Some(false), Some(true)] {
                let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
                let readiness = CliWrapperReadiness::begin_with(marker.clone(), true, || {
                    Ok(std::net::TcpStream::connect(listener.local_addr()?)?)
                });
                assert!(readiness.stream.is_some());
                let (mut receiver, _) = listener.accept().unwrap();
                receiver
                    .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                    .unwrap();
                if let Some(retryable) = failure {
                    readiness.fail(&CliWrapperFailure {
                        message: "CLI process creation failed".into(),
                        retryable,
                    });
                } else {
                    readiness.executed();
                }
                let mut received = Vec::new();
                receiver.read_to_end(&mut received).unwrap();
                if let Some(retryable) = failure {
                    assert_eq!(received.first(), Some(&b'!'));
                    let failure: CliWrapperFailure =
                        serde_json::from_slice(&received[1..]).unwrap();
                    assert_eq!(failure.message, "CLI process creation failed");
                    assert_eq!(failure.retryable, retryable);
                } else {
                    assert!(received.is_empty(), "execution is confirmed by EOF");
                }
            }
        }
    }

    #[test]
    fn windows_cli_wrapper_falls_back_to_tcp_if_final_marker_write_fails() {
        use std::io::Read;

        for retryable in [None, Some(false), Some(true)] {
            let temporary = tempfile::tempdir().unwrap();
            let marker_path = temporary.path().join("wrapper.json");
            let readiness =
                CliWrapperReadiness::begin_with(Some(marker_path.clone()), true, || {
                    panic!("the initial marker write succeeded")
                });
            assert_eq!(
                CliWrapperMarker::read(&marker_path)
                    .unwrap()
                    .unwrap()
                    .status,
                CliWrapperMarkerStatus::Launching
            );
            std::fs::remove_file(&marker_path).unwrap();
            std::fs::create_dir(&marker_path).unwrap();
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let failure = retryable.map(|retryable| CliWrapperFailure {
                message: "CLI process creation failed".into(),
                retryable,
            });
            let mut attempted = false;
            readiness.finish_with(failure.as_ref(), || {
                attempted = true;
                Ok(std::net::TcpStream::connect(listener.local_addr()?)?)
            });
            assert!(
                attempted,
                "the failed final write must use the deferred TCP channel"
            );
            let (mut receiver, _) = listener.accept().unwrap();
            receiver
                .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                .unwrap();
            let mut received = Vec::new();
            receiver.read_to_end(&mut received).unwrap();
            if let Some(retryable) = retryable {
                assert_eq!(received.first(), Some(&b'!'));
                let failure: CliWrapperFailure = serde_json::from_slice(&received[1..]).unwrap();
                assert_eq!(failure.message, "CLI process creation failed");
                assert_eq!(failure.retryable, retryable);
            } else {
                assert!(received.is_empty(), "execution is confirmed by EOF");
            }
        }
    }

    #[test]
    fn cli_wrapper_does_not_repeat_an_exhausted_tcp_connect_budget() {
        for prefer_marker in [false, true] {
            let readiness = CliWrapperReadiness::begin_with(None, prefer_marker, || {
                Err(std::io::Error::from(std::io::ErrorKind::TimedOut).into())
            });
            readiness.finish_with(None, || {
                panic!("the connection budget was already used before launching")
            });
        }
    }

    #[test]
    fn cli_wrapper_readiness_writes_marker_even_when_handshake_is_refused() {
        let temporary = tempfile::tempdir().unwrap();
        let marker_path = temporary.path().join("wrapper.json");
        let mut attempted = false;
        let readiness = CliWrapperReadiness::begin_with(Some(marker_path.clone()), false, || {
            attempted = true;
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "listener already gone",
            )
            .into())
        });
        assert!(
            attempted,
            "other platforms keep TCP even after the marker was written"
        );
        assert!(readiness.stream.is_none());
        assert_eq!(
            CliWrapperMarker::read(&marker_path)
                .unwrap()
                .unwrap()
                .status,
            CliWrapperMarkerStatus::Launching
        );
    }

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
                &[],
                None,
            )
            .unwrap();
            let first_line = output.iter().position(|byte| *byte == b'\n').unwrap();
            let routed: serde_json::Value = serde_json::from_slice(&output[..first_line]).unwrap();
            let mut expected = serde_json::json!({ "id": 17, "method": method,
                    "params": { "threadId": "old-thread", "model": "route-second/gpt-6-astra",
                        "config": { "service_tier": "fast", "artifact.session": "keep" } }
            });
            if method == "thread/resume" {
                expected["params"].as_object_mut().unwrap().remove("model");
            } else {
                expected["params"]["modelProvider"] = "codey_router".into();
            }
            assert_eq!(routed, expected);
            assert_eq!(&output[first_line + 1..], passthrough);
        }
    }

    #[test]
    fn title_generation_uses_misc_model_and_keeps_disabled_fastctx_valid() {
        for local_router_enabled in [false, true] {
            for method in ["thread/start", "thread/fork"] {
                let overrides =
                    vec!["mcp_servers.codey_fastctx.command=\"/Codey path/codey\"".to_string()];
                let mut expected = serde_json::json!({
                    "id":"title:1", "method":method,
                    "params":{"threadId":"source", "ephemeral":true,
                        "threadSource":"thread_title", "model":"native-title-model",
                        "modelProvider":null, "excludeTurns":true,
                        "config":{"mcp_servers":{"codey_fastctx":{"enabled":false},
                            "user_server":{"enabled":false}}, "features.hooks":false}}
                });
                let turn = "{\"id\":2,\"method\":\"turn/start\",\"params\":{\"threadId\":\"temporary\",\"turnTrigger\":\"thread_title\",\"model\":null,\"effort\":null}}\r\n";
                let input = format!("{expected}\n{turn}");
                let mut output = Vec::new();
                forward_app_server_input(
                    std::io::BufReader::with_capacity(1, input.as_bytes()),
                    &mut output,
                    &overrides,
                    None,
                    local_router_enabled,
                    Some("configured/title-model"),
                )
                .unwrap();
                let newline = output.iter().position(|byte| *byte == b'\n').unwrap();
                let actual: serde_json::Value = serde_json::from_slice(&output[..newline]).unwrap();
                expected["params"]["model"] = "configured/title-model".into();
                expected["params"]["config"]["mcp_servers"]["codey_fastctx"]["command"] =
                    "/Codey path/codey".into();
                if local_router_enabled {
                    expected["params"]["modelProvider"] = "codey_router".into();
                }
                assert_eq!(actual, expected);
                assert_eq!(
                    &output[newline + 1..],
                    turn.as_bytes(),
                    "the native null model inherits the temporary thread model"
                );
            }
        }
    }

    #[test]
    fn title_generation_preserves_native_model_without_a_misc_selection() {
        for misc_model in [None, Some(""), Some("  ")] {
            for method in ["thread/start", "thread/fork"] {
                let input = serde_json::json!({"id":1,"method":method,"params":{
                    "threadSource":"thread_title","ephemeral":true,"model":"native-title-model",
                    "config":{"mcp_servers":{"codey_fastctx":{"enabled":false}}}
                }});
                let mut output = Vec::new();
                forward_app_server_input(
                    input.to_string().as_bytes(),
                    &mut output,
                    &["mcp_servers.codey_fastctx.command=codey".into()],
                    None,
                    false,
                    misc_model,
                )
                .unwrap();
                let actual: serde_json::Value = serde_json::from_slice(&output).unwrap();
                assert_eq!(actual["params"]["model"], "native-title-model");
                assert_eq!(
                    actual["params"]["config"]["mcp_servers"]["codey_fastctx"],
                    serde_json::json!({"enabled":false,"command":"codey"})
                );
                assert!(actual["params"].get("modelProvider").is_none());

                let mut unchanged = Vec::new();
                let bytes = format!(" {input}\r\n");
                forward_app_server_input(
                    bytes.as_bytes(),
                    &mut unchanged,
                    &[],
                    None,
                    false,
                    misc_model,
                )
                .unwrap();
                assert_eq!(unchanged, bytes.as_bytes());
            }
        }
    }

    #[test]
    fn misc_model_never_rewrites_unconfirmed_or_ordinary_requests() {
        let mut messages = Vec::new();
        for method in [
            "thread/start",
            "thread/fork",
            "thread/resume",
            "turn/start",
            "thread/settings/update",
        ] {
            for source in [
                None,
                Some("automation"),
                Some("thread_summary"),
                Some("thread_title"),
            ] {
                for ephemeral in [None, Some(false), Some(true)] {
                    if matches!(method, "thread/start" | "thread/fork")
                        && source == Some("thread_title")
                        && ephemeral == Some(true)
                    {
                        continue;
                    }
                    let mut params = serde_json::json!({"model":"gpt-6-luna","threadId":"ordinary",
                        "turnTrigger":"thread_title", "input":"Generate a title",
                        "config":{"mcp_servers":{"codey_fastctx":{"enabled":false}}}});
                    if let Some(source) = source {
                        params["threadSource"] = source.into();
                    }
                    if let Some(ephemeral) = ephemeral {
                        params["ephemeral"] = ephemeral.into();
                    }
                    messages.push(
                        serde_json::json!({"id":messages.len(),"method":method,"params":params})
                            .to_string(),
                    );
                }
            }
        }
        messages.extend([
            "invalid-json".into(),
            "{}".into(),
            "{\"id\":9,\"result\":{}}".into(),
            "{\"method\":\"thread/start\",\"params\":null}".into(),
        ]);
        let input = format!(" {}\r\n", messages.join("\r\n "));
        let mut output = Vec::new();
        forward_app_server_input(
            input.as_bytes(),
            &mut output,
            &["mcp_servers.codey_fastctx.command=codey".into()],
            None,
            false,
            Some("configured/title-model"),
        )
        .unwrap();
        assert_eq!(output, input.as_bytes());
    }

    #[test]
    fn title_generation_does_not_replace_existing_or_user_mcp_transports() {
        for server in [
            serde_json::json!({"enabled":false,"command":"user-command","args":["keep"]}),
            serde_json::json!({"enabled":false,"url":"https://user.example/mcp"}),
            serde_json::json!({"enabled":true}),
            serde_json::json!({"enabled":null}),
            serde_json::Value::Null,
        ] {
            let mut message = serde_json::json!({"method":"thread/start","params":{
                "threadSource":"thread_title","ephemeral":true,
                "config":{"mcp_servers":{"codey_fastctx":server,"user":{"enabled":false}}}
            }});
            let before = message.clone();
            assert!(!rewrite_title_generation_request(
                &mut message,
                &["mcp_servers.codey_fastctx.command=codey".into()],
                None
            ));
            assert_eq!(message, before);
        }
    }

    #[test]
    fn input_relay_covers_title_configuration_without_enabling_a_router() {
        for misc_model in [None, Some(""), Some("  ")] {
            assert!(!app_server_input_relay_required(&[], misc_model));
        }
        for overrides in [
            vec!["model_provider=codey_router".into()],
            vec!["mcp_servers.codey_fastctx.command=codey".into()],
        ] {
            assert!(app_server_input_relay_required(&overrides, None));
        }
        assert!(app_server_input_relay_required(
            &[],
            Some("configured-title-model")
        ));
        assert!(!local_router_runtime_enabled(&[
            "model_provider=openai".into()
        ]));
    }

    #[test]
    fn cli_input_uses_one_provider_in_mixed_catalogs() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("catalog.json");
        std::fs::write(
            &path,
            serde_json::json!({"models":[
                {"slug":"remote/shared", "codey_remote_compaction":true},
                {"slug":"local/shared", "codey_remote_compaction":false}
            ]})
            .to_string(),
        )
        .unwrap();
        let overrides = vec![
            "model_provider=\"codey_router_remote\"".into(),
            format!(
                "model_catalog_json={}",
                toml_edit::Value::from(path.to_str().unwrap())
            ),
            "model=\"remote/shared\"".into(),
        ];
        assert!(local_router_runtime_enabled(&overrides));
        for method in ["thread/start", "thread/fork"] {
            for (model, provider) in [
                ("remote/shared", "codey_router"),
                ("local/shared", "codey_router"),
                ("shared", "codey_router"),
            ] {
                let input = serde_json::json!({"method":method, "params":{"model":model,"modelProvider":"codey_router_remote"}}).to_string();
                let mut output = Vec::new();
                route_local_app_server_input(input.as_bytes(), &mut output, &overrides, None)
                    .unwrap();
                let result: serde_json::Value = serde_json::from_slice(&output).unwrap();
                assert_eq!(result["params"]["modelProvider"], provider);
            }
        }
    }

    #[test]
    fn unindexed_resumes_omit_empty_model_overrides_and_forks_keep_their_input() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("catalog.json");
        std::fs::write(
            &path,
            serde_json::json!({"models":[
                {"slug":"remote/shared", "codey_remote_compaction":true}
            ]})
            .to_string(),
        )
        .unwrap();
        for default_provider in ["codey_router", "codey_router_remote"] {
            for with_catalog in [false, true] {
                let mut overrides = vec![
                    format!("model_provider=\"{default_provider}\""),
                    "model=\"remote/shared\"".into(),
                ];
                if with_catalog {
                    overrides.push(format!(
                        "model_catalog_json={}",
                        toml_edit::Value::from(path.to_str().unwrap())
                    ));
                }
                for method in ["thread/resume", "thread/fork"] {
                    for model in [
                        None,
                        Some(serde_json::Value::Null),
                        Some("".into()),
                        Some("  ".into()),
                    ] {
                        let mut params = serde_json::json!({
                            "threadId":"saved", "modelProvider":"codey_router_remote"
                        });
                        if let Some(model) = model {
                            params["model"] = model;
                        }
                        let input =
                            serde_json::json!({"method":method,"params":params}).to_string();
                        let mut output = Vec::new();
                        route_local_app_server_input(
                            input.as_bytes(),
                            &mut output,
                            &overrides,
                            None,
                        )
                        .unwrap();
                        let result: serde_json::Value = serde_json::from_slice(&output).unwrap();
                        if method == "thread/resume" {
                            let params = params.as_object_mut().unwrap();
                            params.remove("modelProvider");
                            if params.get("model").is_some_and(|model| !model.is_null()) {
                                params.remove("model");
                            }
                        } else {
                            params["modelProvider"] = "codey_router".into();
                        }
                        assert_eq!(result["params"], params);
                    }
                }
            }
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
    fn windows_package_resume_helper_requires_both_process_and_thread() {
        let launch_id = "37f4f759-55f1-4b66-a45d-160644fe20d4";
        let feedback_path = std::env::temp_dir()
            .join("自定义 Codey 状态")
            .join(format!("resume-{launch_id}.json"));
        assert_eq!(windows_package_resume_target(&[]).unwrap(), None);
        assert_eq!(
            windows_package_resume_target(&["--debug-port".into()]).unwrap(),
            None
        );
        for args in [["-p", "42", "-tid", "73"], ["-TID", "73", "-P", "42"]] {
            let args = std::iter::once(WINDOWS_PACKAGE_RESUME_ARGUMENT)
                .chain(["--launch-id", launch_id])
                .chain(args)
                .map(OsString::from)
                .chain([
                    OsString::from("--launch-state"),
                    feedback_path.clone().into_os_string(),
                ])
                .collect::<Vec<_>>();
            assert_eq!(
                windows_package_resume_target(&args).unwrap(),
                Some(WindowsPackageResumeTarget {
                    process_id: 42,
                    thread_id: 73,
                    launch_id: uuid::Uuid::parse_str(launch_id).unwrap(),
                    feedback_path: feedback_path.clone()
                })
            );
        }
    }

    #[test]
    fn windows_package_resume_helper_rejects_malformed_targets() {
        let feedback_path =
            std::env::temp_dir().join("resume-37f4f759-55f1-4b66-a45d-160644fe20d4.json");
        for args in [
            vec![],
            vec!["-tid", "73"],
            vec!["-p", "42"],
            vec!["-p", "42", "-tid"],
            vec!["-p", "0", "-tid", "73"],
            vec!["-p", "42", "-tid", "0"],
            vec!["-p", "invalid", "-tid", "73"],
            vec!["-p", "42", "-tid", "4294967296"],
            vec!["-p", "42", "-tid", "+73"],
            vec!["-p", "42", "-tid", "-73"],
            vec!["-p", "42", "-tid", "73", "-P", "42"],
            vec!["-p", "42", "-tid", "73", "-tid", "74"],
            vec!["-p", "42", "-tid", "73", "--extra", "1"],
        ] {
            let args = std::iter::once(WINDOWS_PACKAGE_RESUME_ARGUMENT)
                .chain(["--launch-id", "37f4f759-55f1-4b66-a45d-160644fe20d4"])
                .chain(args)
                .map(OsString::from)
                .chain([
                    OsString::from("--launch-state"),
                    feedback_path.clone().into_os_string(),
                ])
                .collect::<Vec<_>>();
            assert!(windows_package_resume_target(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn windows_package_resume_helper_requires_valid_unique_launch_id() {
        let feedback_path =
            std::env::temp_dir().join("resume-37f4f759-55f1-4b66-a45d-160644fe20d4.json");
        for extra in [
            vec![],
            vec!["--launch-id", "../other-launch"],
            vec!["--launch-id", "00000000-0000-0000-0000-000000000000"],
            vec![
                "--launch-id",
                "37f4f759-55f1-4b66-a45d-160644fe20d4",
                "--launch-id",
                "37f4f759-55f1-4b66-a45d-160644fe20d4",
            ],
        ] {
            let args = [WINDOWS_PACKAGE_RESUME_ARGUMENT, "-p", "42", "-tid", "73"]
                .into_iter()
                .chain(extra)
                .map(OsString::from)
                .chain([
                    OsString::from("--launch-state"),
                    feedback_path.clone().into_os_string(),
                ])
                .collect::<Vec<_>>();
            assert!(windows_package_resume_target(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn windows_package_resume_helper_rejects_missing_or_unrelated_state_path() {
        let base = [
            WINDOWS_PACKAGE_RESUME_ARGUMENT,
            "-p",
            "42",
            "-tid",
            "73",
            "--launch-id",
            "37f4f759-55f1-4b66-a45d-160644fe20d4",
        ]
        .map(OsString::from)
        .to_vec();
        assert!(windows_package_resume_target(&base).is_err());
        for path in [
            std::path::PathBuf::from("relative/resume-37f4f759-55f1-4b66-a45d-160644fe20d4.json"),
            std::env::temp_dir().join("unrelated.json"),
        ] {
            let mut args = base.clone();
            args.extend([OsString::from("--launch-state"), path.into_os_string()]);
            assert!(windows_package_resume_target(&args).is_err());
        }
        let path = std::env::temp_dir().join("resume-37f4f759-55f1-4b66-a45d-160644fe20d4.json");
        let mut args = base;
        args.extend([
            OsString::from("--launch-state"),
            path.clone().into_os_string(),
        ]);
        assert!(windows_package_resume_target(&args).is_ok());
        args.extend([OsString::from("--launch-state"), path.into_os_string()]);
        assert!(windows_package_resume_target(&args).is_err());
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
        for id in crate::local_router::ROUTER_PROVIDER_IDS {
            for router in [
                format!("model_provider=\"{id}\""),
                format!("model_provider='{id}'"),
                format!("model_provider={id}"),
            ] {
                let router = vec![router];
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
        }
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
