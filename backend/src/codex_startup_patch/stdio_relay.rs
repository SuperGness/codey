//! Codex CLI app-server 的标准输入/输出流中继与设置追踪模块。

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use memchr::{memchr, memmem};

/// 预测前的会话设置回复不含历史轮次，远小于此上限。
#[cfg(any(windows, target_os = "macos", test))]
pub(super) const PREDICTION_RESUME_LINE_LIMIT: usize = 256 * 1024;

#[cfg(any(windows, target_os = "macos", test))]
pub(super) fn wait_for_cli_child_exit(
    child: &mut std::process::Child,
    timeout: Duration,
) -> std::io::Result<bool> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(true),
            Ok(None) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        std::thread::sleep(remaining.min(Duration::from_millis(20)));
    }
}

#[cfg(any(windows, target_os = "macos", test))]
pub(super) fn terminate_cli_child(child: &mut std::process::Child, role: &str) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    let termination = child.kill();
    // Windows 的终止请求是异步的；回收同样有时限，避免清理失败阻塞包装器退出。
    match wait_for_cli_child_exit(child, Duration::from_secs(1)) {
        Ok(true) => {}
        result => eprintln!(
            "Codey 回收 {role} 失败（PID {}）：终止结果 {termination:?}，等待结果 {result:?}",
            child.id()
        ),
    }
}

#[cfg(test)]
pub(super) fn finish_cli_wrapper_helpers(
    input_router: &mut Option<std::process::Child>,
    output_observer: &mut Option<std::process::Child>,
    drain_timeout: Duration,
) {
    // CLI 已结束，不再需要输入；先清理，避免输出管道未关闭时连带保留输入进程。
    if let Some(relay) = input_router {
        terminate_cli_child(relay, "输入转发器");
    }
    if let Some(observer) = output_observer {
        // 保留末尾通知的排空时间，但后代继承 stdout 时不能无限等待 EOF。
        if !matches!(wait_for_cli_child_exit(observer, drain_timeout), Ok(true)) {
            terminate_cli_child(observer, "输出观察器");
        }
    }
}

/// 在单进程工作线程与匿名管道中继 stdio（Windows 主进程或 macOS 单辅助进程共用）。
#[cfg(any(windows, target_os = "macos", test))]
pub(super) struct InProcessStdioRelays {
    output_done: Option<std::sync::mpsc::Receiver<()>>,
    input_done: Option<std::sync::mpsc::Receiver<()>>,
}

#[cfg(any(windows, target_os = "macos", test))]
impl InProcessStdioRelays {
    #[cfg(windows)]
    pub(super) fn start(
        command: &mut std::process::Command,
        relay_input: bool,
        observe_output: bool,
        runtime_overrides: Vec<String>,
        misc_model: Option<String>,
    ) -> Result<Self> {
        let codex_home = crate::codex_config::codex_home().to_path_buf();
        let mut child_stdin = None;
        let mut input_thread_pipe = None;
        if relay_input {
            let (reader, writer) = std::io::pipe().context("创建 Codex 请求转发管道失败")?;
            child_stdin = Some(reader);
            input_thread_pipe = Some(writer);
        }
        let mut child_stdout = None;
        let mut output_thread_pipe = None;
        if observe_output {
            let (reader, writer) =
                std::io::pipe().context("创建 Codex 会话响应观察管道失败")?;
            output_thread_pipe = Some(reader);
            child_stdout = Some(writer);
        }

        let relays = Self::spawn_with_streams(
            input_thread_pipe.map(|writer| {
                let input: Box<dyn std::io::BufRead + Send> =
                    Box::new(std::io::BufReader::new(std::io::stdin()));
                let output: Box<dyn std::io::Write + Send> = Box::new(writer);
                (input, output)
            }),
            output_thread_pipe.map(|reader| {
                let input: Box<dyn std::io::Read + Send> = Box::new(reader);
                let output: Box<dyn std::io::Write + Send> = Box::new(std::io::stdout());
                (input, output)
            }),
            codex_home,
            runtime_overrides,
            misc_model,
        )?;

        if let Some(reader) = child_stdin {
            command.stdin(std::process::Stdio::from(reader));
        }
        if let Some(writer) = child_stdout {
            command.stdout(std::process::Stdio::from(writer));
        }
        Ok(relays)
    }

    pub(super) fn spawn_with_streams(
        input_pair: Option<(Box<dyn std::io::BufRead + Send>, Box<dyn std::io::Write + Send>)>,
        output_pair: Option<(Box<dyn std::io::Read + Send>, Box<dyn std::io::Write + Send>)>,
        codex_home: std::path::PathBuf,
        runtime_overrides: Vec<String>,
        misc_model: Option<String>,
    ) -> Result<Self> {
        let mut input_done = None;
        if let Some((input, mut output)) = input_pair {
            let home = codex_home.clone();
            let overrides = runtime_overrides.clone();
            let local_router = super::local_router_runtime_enabled(&overrides);
            let (tx, rx) = std::sync::mpsc::channel();
            input_done = Some(rx);
            std::thread::Builder::new()
                .name("codey-stdin-relay".into())
                .spawn(move || {
                    let result = forward_app_server_input(
                        input,
                        &mut output,
                        &overrides,
                        Some(&home),
                        local_router,
                        misc_model.as_deref(),
                    );
                    if let Err(error) = result
                        && error.kind() != std::io::ErrorKind::BrokenPipe
                    {
                        eprintln!("Codey 转发 Codex 请求失败：{error:#}");
                    }
                    let _ = tx.send(());
                })
                .context("启动同进程输入转发线程失败")?;
        }

        let mut output_done = None;
        if let Some((input, mut output)) = output_pair {
            let (tx, rx) = std::sync::mpsc::channel();
            output_done = Some(rx);
            std::thread::Builder::new()
                .name("codey-stdout-observer".into())
                .spawn(move || {
                    let result = observe_local_app_server_output(input, &mut output, &codex_home);
                    if let Err(error) = result
                        && error.kind() != std::io::ErrorKind::BrokenPipe
                    {
                        eprintln!("Codey 观察 Codex 会话响应失败：{error:#}");
                    }
                    let _ = tx.send(());
                })
                .context("启动同进程输出观察线程失败")?;
        }

        Ok(Self {
            output_done,
            input_done,
        })
    }

    #[cfg(any(windows, test))]
    pub(super) fn finish(self, drain_timeout: Duration) {
        if drain_timeout.is_zero() {
            return;
        }
        if let Some(done) = self.output_done {
            let _ = done.recv_timeout(drain_timeout);
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn wait_until_finished(self) {
        if let Some(done) = self.output_done {
            let _ = done.recv();
            if let Some(input_done) = self.input_done {
                let _ = input_done.recv_timeout(Duration::from_millis(50));
            }
        } else if let Some(done) = self.input_done {
            let _ = done.recv();
        }
    }
}

const STDIO_RELAY_BUFFER_SHRINK_CAP: usize = 64 * 1024;

fn clear_and_shrink_line(line: &mut Vec<u8>) {
    line.clear();
    if line.capacity() > STDIO_RELAY_BUFFER_SHRINK_CAP {
        line.shrink_to(STDIO_RELAY_BUFFER_SHRINK_CAP);
    }
}

#[cfg(any(windows, target_os = "macos", test))]
pub(super) fn observe_local_app_server_output(
    mut input: impl std::io::Read,
    mut output: impl std::io::Write,
    home: &Path,
) -> std::io::Result<()> {
    let mut buffer = [0; 8192];
    let mut line = Vec::new();
    let mut oversized = false;
    // 小行等到换行才转发以便改写；超过 256 KiB 暂存上限后立即直通剩余字节。
    let mut streaming = false;
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            if !streaming {
                if !line.is_empty() {
                    output.write_all(&line)?;
                    remember_app_server_settings(&line, home);
                }
                output.flush()?;
            }
            return Ok(());
        }
        let mut start = 0;
        let chunk_buf = &buffer[..count];
        while let Some(rel_nl) = memchr(b'\n', &chunk_buf[start..]) {
            let nl = start + rel_nl;
            let segment = &chunk_buf[start..nl];
            if streaming {
                output.write_all(&chunk_buf[start..=nl])?;
            } else if line.len() + segment.len() > PREDICTION_RESUME_LINE_LIMIT {
                if !line.is_empty() {
                    output.write_all(&line)?;
                }
                output.write_all(&chunk_buf[start..=nl])?;
            } else {
                line.extend_from_slice(segment);
                remember_app_server_settings(&line, home);
                match rewrite_prediction_resume_response(&line) {
                    Some(rewritten) => output.write_all(&rewritten)?,
                    None => output.write_all(&line)?,
                }
                output.write_all(b"\n")?;
            }
            start = nl + 1;
            clear_and_shrink_line(&mut line);
            oversized = false;
            streaming = false;
        }
        if start < count {
            let tail = &chunk_buf[start..];
            if streaming || oversized {
                output.write_all(tail)?;
            } else if line.len() + tail.len() > PREDICTION_RESUME_LINE_LIMIT {
                if !line.is_empty() {
                    output.write_all(&line)?;
                    line.clear();
                }
                output.write_all(tail)?;
                oversized = true;
                streaming = true;
            } else {
                line.extend_from_slice(tail);
            }
        }
        output.flush()?;
    }
}

/// Codex 主进程只为 openai provider 的会话生成输入框预测，预测前会读取会话设置。
/// 主进程与页面的请求均使用 `thread/resume:<uuid>`，不能据此区分来源。
/// 官方账号线路兼容预测时改报 openai，同时用 codeyRuntimeModelProvider 保留真实
/// provider，供页面恢复运行时状态；预测的 thread/fork 仍走本地路由。
#[cfg(any(windows, target_os = "macos", test))]
pub(super) fn rewrite_prediction_resume_response(line: &[u8]) -> Option<Vec<u8>> {
    const RESUME_ID_PREFIX: &str = "thread/resume:";
    if memmem::find(line, b"\"thread/resume:").is_none() {
        return None;
    }
    let mut message = serde_json::from_slice::<serde_json::Value>(line).ok()?;
    if !message.get("id")?.as_str()?.starts_with(RESUME_ID_PREFIX) {
        return None;
    }
    let result = message.get_mut("result")?.as_object_mut()?;
    let official = result.get("modelProvider")?.as_str()?
        == crate::local_router::ROUTER_PROVIDER_ID
        && crate::model_id::parse_alias(result.get("model")?.as_str()?)
            .is_some_and(|alias| crate::config::is_official_profile_id(alias.provider_key));
    if !official {
        return None;
    }
    let runtime_provider = result.get("modelProvider")?.clone();
    result.insert("codeyRuntimeModelProvider".into(), runtime_provider);
    result.insert("modelProvider".into(), "openai".into());
    serde_json::to_vec(&message).ok()
}

#[cfg(any(windows, target_os = "macos", test))]
pub(super) fn remember_app_server_settings(line: &[u8], home: &Path) {
    if memmem::find(line, b"thread/settings/updated").is_none() {
        return;
    }
    let Ok(message) = serde_json::from_slice::<serde_json::Value>(line) else {
        return;
    };
    if message.get("method").and_then(serde_json::Value::as_str) != Some("thread/settings/updated")
        || message.get("id").is_some()
    {
        return;
    }
    let Some(thread_id) = message
        .pointer("/params/threadId")
        .and_then(serde_json::Value::as_str)
    else {
        return;
    };
    let Some(model) = message
        .pointer("/params/threadSettings/model")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
    else {
        return;
    };
    let provider = message
        .pointer("/params/threadSettings/modelProvider")
        .and_then(serde_json::Value::as_str);
    let selected_at_ms = message
        .get("emittedAtMs")
        .and_then(serde_json::Value::as_i64);
    let effort = message
        .pointer("/params/threadSettings/effort")
        .or_else(|| {
            message.pointer("/params/threadSettings/collaborationMode/settings/reasoning_effort")
        });
    if let Err(error) = crate::session_model::remember_settings(
        home,
        thread_id,
        model,
        provider,
        selected_at_ms,
        effort,
    ) {
        eprintln!("Codey 保存会话模型失败：{error:#}");
    }
}

#[cfg(any(windows, target_os = "macos", test))]
#[derive(serde::Deserialize)]
struct TrackedInputEnvelope<'a> {
    #[serde(borrow, default)]
    method: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    params: Option<TrackedInputParams<'a>>,
}

#[cfg(any(windows, target_os = "macos", test))]
#[derive(serde::Deserialize)]
struct TrackedInputParams<'a> {
    #[serde(borrow, default, rename = "threadId")]
    thread_id: Option<Cow<'a, str>>,
}

#[cfg(any(windows, target_os = "macos", test))]
fn effective_misc_model<'a>(fallback: Option<&'a str>) -> Option<Cow<'a, str>> {
    if let Some(path) = std::env::var_os(super::CLI_WRAPPER_MISC_MODEL_FILE_ENV)
        .filter(|value| !value.is_empty())
        && let Ok(bytes) = crate::fs_util::read_bounded(Path::new(&path), 1024)
    {
        let trimmed = String::from_utf8_lossy(&bytes).trim().to_string();
        return (!trimmed.is_empty()).then_some(Cow::Owned(trimmed));
    }
    fallback
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(Cow::Borrowed)
}

#[cfg(any(windows, target_os = "macos", test))]
pub(super) fn forward_app_server_input(
    mut input: impl std::io::BufRead,
    mut output: impl std::io::Write,
    runtime_overrides: &[String],
    codex_home: Option<&Path>,
    local_router_enabled: bool,
    misc_model: Option<&str>,
) -> std::io::Result<()> {
    let mut line = Vec::new();
    let mut live_threads = HashSet::<String>::new();
    loop {
        clear_and_shrink_line(&mut line);
        if input.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        // 大部分高频请求（如流式增量与工具确认）既不改写也不影响活跃会话索引，跳过 JSON 解析。
        let may_track_or_rewrite = memmem::find(&line, b"\"thread/").is_some()
            || memmem::find(&line, b"\"turn/start\"").is_some();
        if !may_track_or_rewrite {
            output.write_all(&line)?;
            output.flush()?;
            continue;
        }
        let mut rewritten_message = None;
        if let Ok(envelope) = serde_json::from_slice::<TrackedInputEnvelope<'_>>(&line)
            && let Some(method_name) = envelope.method.as_deref()
        {
            match method_name {
                "thread/unsubscribe" | "thread/archive" => {
                    if let Some(thread_id) = envelope
                        .params
                        .as_ref()
                        .and_then(|params| params.thread_id.as_deref())
                    {
                        live_threads.remove(thread_id);
                    }
                }
                "thread/settings/update" | "turn/start" => {
                    if let Some(thread_id) = envelope
                        .params
                        .as_ref()
                        .and_then(|params| params.thread_id.as_deref())
                        && !live_threads.contains(thread_id)
                    {
                        // 活跃会话的选择保留在原生进程内，磁盘上的最后一轮可能更旧。
                        live_threads.insert(thread_id.to_owned());
                    }
                }
                "thread/start" | "thread/resume" | "thread/fork" => {
                    if let Ok(mut message) = serde_json::from_slice::<serde_json::Value>(&line) {
                        let resolved_misc_model =
                            matches!(method_name, "thread/start" | "thread/fork")
                                .then(|| effective_misc_model(misc_model))
                                .flatten();
                        let mut changed = rewrite_title_generation_request(
                            &mut message,
                            runtime_overrides,
                            resolved_misc_model.as_deref(),
                        );
                        if local_router_enabled {
                            let method = method_name.to_owned();
                            if message.get("params").is_none_or(serde_json::Value::is_null) {
                                message["params"] = serde_json::json!({});
                            }
                            if let Some(params) = message
                                .get_mut("params")
                                .and_then(serde_json::Value::as_object_mut)
                            {
                                if method == "thread/resume"
                                    && params
                                        .get("model")
                                        .and_then(serde_json::Value::as_str)
                                        .is_some_and(|model| !model.trim().is_empty())
                                    && let Some(thread_id) =
                                        params.get("threadId").and_then(serde_json::Value::as_str)
                                {
                                    live_threads.insert(thread_id.to_owned());
                                }
                                if method == "thread/resume"
                                    && params.get("model").is_none_or(|model| {
                                        model.is_null()
                                            || model
                                                .as_str()
                                                .is_some_and(|model| model.trim().is_empty())
                                    })
                                    && let Some(home) = codex_home
                                    && let Some(thread_id) =
                                        params.get("threadId").and_then(serde_json::Value::as_str)
                                    && !live_threads.contains(thread_id)
                                {
                                    match restored_resume_settings(
                                        home,
                                        thread_id,
                                        runtime_overrides,
                                    ) {
                                        Ok(Some((model, effort))) => {
                                            params.insert("model".into(), model.into());
                                            if let Some(effort) = effort {
                                                if effort.is_null() {
                                                    if let Some(config) = params
                                                        .get_mut("config")
                                                        .and_then(serde_json::Value::as_object_mut)
                                                    {
                                                        config.remove("model_reasoning_effort");
                                                    }
                                                } else if params
                                                    .get("config")
                                                    .is_none_or(serde_json::Value::is_null)
                                                {
                                                    params.insert(
                                                        "config".into(),
                                                        serde_json::json!({"model_reasoning_effort":effort}),
                                                    );
                                                } else if let Some(config) = params
                                                    .get_mut("config")
                                                    .and_then(serde_json::Value::as_object_mut)
                                                {
                                                    config.insert(
                                                        "model_reasoning_effort".into(),
                                                        effort,
                                                    );
                                                }
                                            }
                                        }
                                        Ok(None) => {}
                                        // 单向转发器无法发送 RPC 错误响应；保留请求和后续消息，错误仅写 stderr。
                                        Err(error) => eprintln!("Codey 恢复会话模型失败：{error:#}"),
                                    }
                                }
                                let provider = super::router_provider_for_params(
                                    &method,
                                    params,
                                    runtime_overrides,
                                );
                                params.insert("modelProvider".into(), provider.into());
                                params.remove("model_provider");
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
                                changed = true;
                            }
                        }
                        if changed {
                            rewritten_message = Some(message);
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(message) = rewritten_message {
            line = serde_json::to_vec(&message).map_err(std::io::Error::other)?;
            line.push(b'\n');
        }
        output.write_all(&line)?;
        output.flush()?;
    }
}

#[cfg(test)]
pub(super) fn route_local_app_server_input(
    input: impl std::io::BufRead,
    output: impl std::io::Write,
    runtime_overrides: &[String],
    codex_home: Option<&Path>,
) -> std::io::Result<()> {
    forward_app_server_input(input, output, runtime_overrides, codex_home, true, None)
}

/// Desktop 用来源和临时会话标记识别标题任务，也可能从已有会话分支。
/// 后续标题 turn/start 传 model:null，沿用临时会话的模型。
#[cfg(any(windows, target_os = "macos", test))]
pub(super) fn rewrite_title_generation_request(
    message: &mut serde_json::Value,
    runtime_overrides: &[String],
    misc_model: Option<&str>,
) -> bool {
    if !matches!(
        message.get("method").and_then(serde_json::Value::as_str),
        Some("thread/start" | "thread/fork")
    ) {
        return false;
    }
    let Some(params) = message
        .get_mut("params")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return false;
    };
    if params.get("ephemeral").and_then(serde_json::Value::as_bool) != Some(true)
        || params
            .get("threadSource")
            .and_then(serde_json::Value::as_str)
            != Some("thread_title")
    {
        return false;
    }
    let mut changed = false;
    if let Some(model) = misc_model.map(str::trim).filter(|model| !model.is_empty()) {
        params.insert("model".into(), model.into());
        changed = true;
    }
    // Desktop 用 {enabled:false} 禁用 MCP。CLI 注入的 FastCtx 传输字段在
    // 此覆盖层缺失时会使配置解析失败；补齐禁用条目的 command，不启动服务。
    if let Some(server) = params
        .get_mut("config")
        .and_then(|config| config.get_mut("mcp_servers"))
        .and_then(|servers| servers.get_mut("codey_fastctx"))
        .and_then(serde_json::Value::as_object_mut)
        && server.get("enabled").and_then(serde_json::Value::as_bool) == Some(false)
        && !server.contains_key("command")
        && !server.contains_key("url")
        && let Some(command) =
            super::runtime_override_string(runtime_overrides, "mcp_servers.codey_fastctx.command")
                .filter(|command| !command.trim().is_empty())
    {
        server.insert("command".into(), command.into());
        changed = true;
    }
    changed
}

#[cfg(any(windows, target_os = "macos", test))]
pub(crate) fn app_server_input_relay_required(
    overrides: &[String],
    misc_model: Option<&str>,
) -> bool {
    super::local_router_runtime_enabled(overrides)
        || misc_model.is_some_and(|model| !model.trim().is_empty())
        || super::runtime_override_string(overrides, "mcp_servers.codey_fastctx.command").is_some()
}

#[cfg(any(windows, target_os = "macos", test))]
pub(super) fn restored_resume_settings(
    home: &Path,
    thread_id: &str,
    overrides: &[String],
) -> Result<Option<(String, Option<serde_json::Value>)>> {
    let saved = crate::session_model::read(home, thread_id)?;
    let model = resolve_resume_model(&saved, overrides)?;
    Ok(model.map(|model| (model, saved.get("reasoningEffort").cloned())))
}

#[cfg(test)]
pub(super) fn restored_resume_model(
    home: &Path,
    thread_id: &str,
    overrides: &[String],
) -> Result<Option<String>> {
    Ok(restored_resume_settings(home, thread_id, overrides)?.map(|(model, _)| model))
}

#[cfg(any(windows, target_os = "macos", test))]
fn cached_catalog_slugs(catalog_path: &str) -> Result<Vec<String>> {
    type CatalogCache = (String, Vec<u8>, Vec<String>);
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Option<CatalogCache>>> =
        std::sync::OnceLock::new();
    let bytes = std::fs::read(catalog_path)?;
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(guard) = cache.lock()
        && let Some((cached_path, cached_bytes, cached_slugs)) = guard.as_ref()
        && cached_path == catalog_path
        && *cached_bytes == bytes
    {
        return Ok(cached_slugs.clone());
    }
    let catalog: serde_json::Value = serde_json::from_slice(&bytes)?;
    let models = catalog
        .get("models")
        .and_then(serde_json::Value::as_array)
        .context("模型目录缺少 models")?;
    let slugs = models
        .iter()
        .filter_map(|entry| entry.get("slug")?.as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((catalog_path.to_owned(), bytes, slugs.clone()));
    }
    Ok(slugs)
}

#[cfg(any(windows, target_os = "macos", test))]
fn resolve_resume_model(saved: &serde_json::Value, overrides: &[String]) -> Result<Option<String>> {
    let Some(model) = saved.get("model").and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };
    let Some(catalog_path) = super::runtime_override_string(overrides, "model_catalog_json")
    else {
        return Ok(None);
    };
    let slugs = cached_catalog_slugs(&catalog_path)?;
    // 当前稳定 ID 优先，不把包含斜杠的上游型号误解为失效的线路 ID。
    if let Some(slug) = slugs
        .iter()
        .find(|slug| crate::model_id::equal(slug, model))
    {
        return Ok(Some(slug.clone()));
    }
    let provider = saved
        .get("modelProvider")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|provider| !provider.is_empty());
    if let Some(provider) =
        provider.filter(|provider| !crate::local_router::is_router_provider(provider))
    {
        let alias = crate::model_id::model_alias(provider, model);
        return Ok(slugs
            .iter()
            .find(|slug| crate::model_id::equal(slug, &alias))
            .cloned());
    }
    let mut matches = slugs.iter().filter(|slug| {
        crate::model_id::parse_alias(slug)
            .is_some_and(|alias| crate::model_id::equal(alias.upstream_model, model))
    });
    let first = matches.next();
    Ok(if matches.next().is_none() {
        first.cloned()
    } else {
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn oversized_output_line_streams_before_one_megabyte_and_preserves_bytes() {
        struct Chunked<'a>(&'a [u8]);
        impl Read for Chunked<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let take = 4096.min(buffer.len()).min(self.0.len());
                buffer[..take].copy_from_slice(&self.0[..take]);
                self.0 = &self.0[take..];
                Ok(take)
            }
        }
        let home = tempfile::tempdir().unwrap();
        let big = "a".repeat(PREDICTION_RESUME_LINE_LIMIT + 10_000);
        let tail = serde_json::json!({
            "method": "thread/settings/updated",
            "params": { "threadId": "after-big", "threadSettings": { "model": "route/ok" } }
        });
        let input = format!("{big}\n{tail}\n");
        let mut output = Vec::new();
        observe_local_app_server_output(Chunked(input.as_bytes()), &mut output, home.path())
            .unwrap();
        assert_eq!(output, input.as_bytes());
        assert_eq!(
            crate::session_model::read(home.path(), "after-big").unwrap()["model"],
            "route/ok"
        );
    }

    #[test]
    fn unrelated_input_lines_bypass_json_parsing_and_turn_start_tracks_live_thread() {
        let home = tempfile::tempdir().unwrap();
        let db = rusqlite::Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE threads (id TEXT, model TEXT, model_provider TEXT, rollout_path TEXT);
            INSERT INTO threads VALUES ('t1', 'route/saved', 'codey_router', 't1.jsonl');",
        )
        .unwrap();
        std::fs::write(
            home.path().join("t1.jsonl"),
            format!(
                "{}\n{}\n",
                serde_json::json!({"type":"session_meta","payload":{"id":"t1","model_provider":"codey_router"}}),
                serde_json::json!({"type":"turn_context","payload":{"model":"route/saved"}})
            ),
        )
        .unwrap();
        let catalog = home.path().join("catalog.json");
        std::fs::write(&catalog, r#"{"models":[{"slug":"route/saved"}]}"#).unwrap();
        let overrides = vec![format!(
            "model_catalog_json={}",
            toml_edit::Value::from(catalog.to_str().unwrap())
        )];

        // 非 thread/ / turn/start 行即便不是合法 JSON 也应原样直通，不触发解析错误或分配。
        let unrelated_bad_json = b"{\"method\":\"item/delta\",\"raw\":not-valid-json}\n";
        // turn/start 标记活跃；thread/resume 不覆盖模型；thread/unsubscribe 清除活跃，后续 resume 恢复模型。
        let stream = [
            unrelated_bad_json.as_slice(),
            br#"{"id":1,"method":"turn/start","params":{"threadId":"t1","largePayload":"ignored"}}"#,
            b"\n",
            br#"{"id":2,"method":"thread/resume","params":{"threadId":"t1","model":null}}"#,
            b"\n",
            br#"{"id":3,"method":"thread/unsubscribe","params":{"threadId":"t1"}}"#,
            b"\n",
            br#"{"id":4,"method":"thread/resume","params":{"threadId":"t1","model":null}}"#,
            b"\n",
        ]
        .concat();
        let mut output = Vec::new();
        route_local_app_server_input(stream.as_slice(), &mut output, &overrides, Some(home.path()))
            .unwrap();
        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0], std::str::from_utf8(unrelated_bad_json).unwrap().trim_end());
        assert_eq!(
            lines[1],
            r#"{"id":1,"method":"turn/start","params":{"threadId":"t1","largePayload":"ignored"}}"#
        );
        let resume_live: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert!(resume_live["params"]["model"].is_null());
        let resume_after_unsub: serde_json::Value = serde_json::from_str(lines[4]).unwrap();
        assert_eq!(resume_after_unsub["params"]["model"], "route/saved");
    }

    #[test]
    fn in_process_stdio_relays_round_trip_and_bound_drain_timeout() {
        let home = tempfile::tempdir().unwrap();
        let (in_read, mut in_write) = std::io::pipe().unwrap();
        let (mut mid_in_read, mid_in_write) = std::io::pipe().unwrap();
        let (mid_out_read, mut mid_out_write) = std::io::pipe().unwrap();
        let (mut out_read, out_write) = std::io::pipe().unwrap();

        let overrides = vec![r#"model_provider="codey_router""#.to_string()];
        let relays = InProcessStdioRelays::spawn_with_streams(
            Some((
                Box::new(std::io::BufReader::new(in_read)),
                Box::new(mid_in_write),
            )),
            Some((Box::new(mid_out_read), Box::new(out_write))),
            home.path().to_path_buf(),
            overrides,
            None,
        )
        .unwrap();

        in_write
            .write_all(br#"{"id":1,"method":"thread/start","params":{"model":"route/x"}}"#)
            .unwrap();
        in_write.write_all(b"\n").unwrap();
        drop(in_write);
        relays
            .input_done
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();

        let mut routed = String::new();
        mid_in_read.read_to_string(&mut routed).unwrap();
        let routed_json: serde_json::Value = serde_json::from_str(routed.trim()).unwrap();
        assert_eq!(routed_json["params"]["modelProvider"], "codey_router");

        mid_out_write
            .write_all(br#"{"method":"thread/settings/updated","params":{"threadId":"ip1","threadSettings":{"model":"route/x"}}}"#)
            .unwrap();
        mid_out_write.write_all(b"\n").unwrap();
        drop(mid_out_write);

        relays.finish(Duration::from_secs(2));
        let mut observed = String::new();
        out_read.read_to_string(&mut observed).unwrap();
        assert!(observed.contains("thread/settings/updated"));
        assert_eq!(
            crate::session_model::read(home.path(), "ip1").unwrap()["model"],
            "route/x"
        );

        // 写端未关闭时，finish 必须在有界时间内返回而非无限阻塞。
        let (leaked_read, _leaked_write) = std::io::pipe().unwrap();
        let (_sink_read, sink_write) = std::io::pipe().unwrap();
        let bounded = InProcessStdioRelays::spawn_with_streams(
            None,
            Some((Box::new(leaked_read), Box::new(sink_write))),
            home.path().to_path_buf(),
            Vec::new(),
            None,
        )
        .unwrap();
        let started = std::time::Instant::now();
        bounded.finish(Duration::from_millis(30));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
