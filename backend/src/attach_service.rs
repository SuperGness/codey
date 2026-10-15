//! host-service 模式：为已经在运行的宿主客户端提供 Codey 后端能力。
//!
//! 默认入口（无参数）仍然是 GUI；本模式由宿主客户端在启动前拉起，只提供
//! 后端能力，不开窗口、不自行拉起客户端。调用形式：
//!
//! ```text
//! codey --codey-host-service --app-dir <宿主客户端路径>
//! ```
//!
//! 分工是固定的，因为主进程启动补丁必须随客户端进程一起装载：
//!
//! 1. 本服务准备运行环境（本地路由、配置写入、主进程启动补丁），把
//!    `debugPort` 与所需环境按 JSON 输出到标准输出的第一行；
//! 2. 宿主读取该行，用这些环境把客户端进程启动起来；
//! 3. 本服务等调试端口就绪，完成渲染层注入，随后常驻。
//!
//! 宿主退出后本服务随之结束。

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

/// 标记参数：出现即进入本模式。
const HOST_SERVICE_FLAG: &str = "--codey-host-service";
/// 宿主客户端的安装路径。
const APP_DIR_FLAG: &str = "--app-dir";
/// 存活检查的间隔（秒）。宿主是父进程，父进程退出后本服务没有存在意义。
const WATCHDOG_INTERVAL_SECONDS: u64 = 10;
/// 外壳启动器读取进程环境用的 NUL 分隔文件。
const HOST_LAUNCH_ENVIRONMENT_FILE: &str = "host-launch.env";
/// 环境文件里额外记录调试端口的键。外壳启动器据此构造客户端的调试参数，
/// 不必解析 JSON。
const DEBUG_PORT_ENV_KEY: &str = "CODEY_HOST_DEBUG_PORT";

/// 把启动环境写成 NUL 分隔的 `KEY=VALUE` 序列。
///
/// 外壳启动器是 C 程序，解析 JSON 代价高；NUL 分隔的键值对可以直接切分，
/// 值里的空格与等号也不需要转义。条目可能含本地路由令牌，所以权限收紧。
fn write_launch_environment(
    path: &std::path::Path,
    environment: &[(String, String)],
) -> Result<()> {
    let mut bytes = Vec::new();
    for (key, value) in environment {
        bytes.extend_from_slice(key.as_bytes());
        bytes.push(b'=');
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(0);
    }
    std::fs::write(path, &bytes)
        .with_context(|| format!("写入启动环境失败：{}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("收紧启动环境权限失败：{}", path.display()))?;
    }
    Ok(())
}

/// 解析后的启动请求。
struct HostServiceRequest {
    app_dir: PathBuf,
}

/// 解析本模式的参数；不属于本模式时返回 `None`，调用方继续走其它分支。
fn parse_request(arguments: &[OsString]) -> Result<Option<HostServiceRequest>> {
    if !arguments
        .iter()
        .any(|argument| argument.to_str() == Some(HOST_SERVICE_FLAG))
    {
        return Ok(None);
    }

    let mut app_dir = None;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        if argument.to_str() != Some(APP_DIR_FLAG) {
            continue;
        }
        let value = arguments.next().context("--app-dir 缺少取值")?;
        app_dir = Some(PathBuf::from(value));
    }

    let app_dir = app_dir.context("host-service 模式缺少必需的 --app-dir")?;
    anyhow::ensure!(app_dir.is_absolute(), "--app-dir 必须是绝对路径");

    Ok(Some(HostServiceRequest { app_dir }))
}

pub fn run_if_requested() -> Result<bool> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let Some(request) = parse_request(&arguments)? else {
        return Ok(false);
    };
    crate::build_async_runtime()?.block_on(serve(request.app_dir))?;
    Ok(true)
}

#[cfg(not(any(windows, target_os = "macos")))]
async fn serve(_app_dir: PathBuf) -> Result<()> {
    anyhow::bail!("host-service 模式仅支持 macOS 与 Windows")
}

#[cfg(any(windows, target_os = "macos"))]
async fn serve(app_dir: PathBuf) -> Result<()> {
    // 与 GUI 模式一致的状态初始化。配置加载、凭据台账与 HTTP 客户端构造都是
    // 同步且会加载系统根证书，放在阻塞池里做。
    let state = tokio::task::spawn_blocking(|| {
        crate::error_log::initialize();
        let state = Arc::new(crate::commands::AppState::default());
        crate::commands::install_plugin_route_handler(Arc::clone(&state));
        let plugin_root =
            codey_runtime_core::paths::default_app_state_dir().join("codey-plugins");
        if let Err(error) = crate::codey_plugins::initialize(plugin_root) {
            crate::error_log::record_failure(
                "plugin_initialization_failed",
                "initialize_codey_plugins",
                error,
                serde_json::json!({}),
            );
        }
        state
    })
    .await
    .context("初始化 Codey 状态的任务异常退出")?;

    state.prewarm_official_account_probe().await;

    let config = state.config.read().await.clone();
    let runtime = crate::launcher::prepare_host_runtime(
        &config,
        &app_dir,
        Arc::clone(&state.account_usage_cache),
    )
    .await
    .context("准备宿主客户端运行环境失败")?;

    // 宿主读第一行就开始启动客户端，所以这一行必须完整落盘再继续。JSON 供
    // 调试与工具读取；启动器是 C 程序，按下面的 NUL 分隔文件构造进程环境。
    let environment_path = codey_runtime_core::paths::default_app_state_dir()
        .join(HOST_LAUNCH_ENVIRONMENT_FILE);
    let mut environment = runtime.environment.clone();
    environment.push((
        DEBUG_PORT_ENV_KEY.to_string(),
        runtime.debug_port.to_string(),
    ));
    write_launch_environment(&environment_path, &environment)
        .context("写入宿主启动环境失败")?;

    let plan = serde_json::json!({
        "debugPort": runtime.debug_port,
        "environment": environment
            .iter()
            .cloned()
            .collect::<std::collections::BTreeMap<String, String>>(),
        "environmentPath": environment_path.to_string_lossy(),
        "routerBaseUrl": runtime
            .router_endpoint
            .as_ref()
            .map(|endpoint| endpoint.base_url.as_str()),
    });
    println!("{plan}");
    std::io::stdout().flush().context("输出启动计划失败")?;

    let handler = crate::commands::make_bridge_handler(&state);
    // 注入结果必须活到本函数结束：它持有 bridge 的事件泵，一 drop 后端连接就
    // 断了，页面上的 bridge 调用会一直等不到应答。
    let injected = match runtime.attach(handler).await {
        Ok(target) => {
            eprintln!(
                "Codey host 服务已注入宿主客户端（调试端口 {}，目标 {}）",
                runtime.debug_port,
                target.websocket_url()
            );
            target
        }
        Err(error) => {
            eprintln!("Codey host 服务注入宿主客户端失败：{error:#}");
            runtime.stop_local_router().await;
            return Err(error);
        }
    };

    // 注册宿主上下文。注入完成后才知道页面的 WebSocket 地址，所以放在这里
    // 而不是 prepare_host_runtime 里。注册之后，需要「本次运行」信息的命令
    // 才能走通：状态查询、提示词优化、模型与线路热更新、扩展清单刷新。
    {
        let mut attach = state.host_attach.write().await;
        *attach = Some(Arc::new(crate::commands::HostAttach {
            debug_port: runtime.debug_port,
            websocket_url: injected.websocket_url().to_string(),
            app_dir,
            router_endpoint: runtime.router_endpoint.clone(),
            local_router: runtime.local_router_handle(),
            applied_config: config,
        }));
    }

    let mut watchdog = tokio::time::interval(Duration::from_secs(WATCHDOG_INTERVAL_SECONDS));
    watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("Codey host 服务收到退出信号");
                break;
            }
            _ = watchdog.tick() => {
                if !host_is_alive(runtime.debug_port) {
                    eprintln!("宿主客户端已退出，Codey host 服务随之结束");
                    break;
                }
                // 顺带确认注入仍在：泵被移除说明桥接已经失效。
                if injected.websocket_url().is_empty() {
                    eprintln!("注入目标已失效，Codey host 服务结束");
                    break;
                }
            }
        }
    }

    runtime.stop_local_router().await;
    Ok(())
}

/// 宿主是否还在运行。
///
/// 调试端口是宿主进程的对外特征：注入成功后端口必然已监听，端口随之消失
/// 也说明宿主退出了。不依赖进程树，因为外壳启动器通常会 `exec` 掉自己。
fn host_is_alive(debug_port: u16) -> bool {
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], debug_port));
    std::net::TcpStream::connect_timeout(&address, Duration::from_millis(500)).is_ok()
}
