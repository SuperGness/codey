//! Bounded, side-effect-limited MCP handshake checks. No business tools are called.
use std::{collections::BTreeMap, path::PathBuf, process::Stdio, time::Duration};

use anyhow::{Result, anyhow, bail};
use reqwest::{
    Client, Response, Url,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
};

const MAX_BODY: usize = 1024 * 1024;
const MAX_MESSAGES: usize = 32;
const PROTOCOLS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
struct ProbeSuccess {
    message: String,
    protocol: String,
    server_info: Value,
}

/// `config` is an internal, unredacted server table and must never be logged.
pub async fn test_mcp(config: Value, project_dir: Option<PathBuf>) -> Value {
    let result = tokio::time::timeout(TIMEOUT, probe(&config, project_dir)).await;
    match result {
        Ok(Ok(result)) => {
            json!({"ok":true,"summary":"MCP 协议检查通过","protocolVersion":result.protocol,"serverInfo":result.server_info,"checks":[{"name":"握手与能力检查","ok":true,"message":result.message}]})
        }
        Ok(Err(error)) => failure(error.to_string()),
        Err(_) => failure("检查超过 15 秒，已停止本次连接和自行启动的进程".into()),
    }
}

fn failure(message: String) -> Value {
    json!({"ok":false,"summary":"MCP 检查未通过","checks":[{"name":"握手与能力检查","ok":false,"message":message}]})
}

async fn probe(config: &Value, project_dir: Option<PathBuf>) -> Result<ProbeSuccess> {
    if !config.is_object() {
        bail!("MCP 配置必须是对象");
    }
    let command = config.get("command").is_some();
    let url = config.get("url").is_some();
    if command == url {
        bail!("请仅配置 command 或 url 中的一项");
    }
    if command {
        probe_stdio(config, project_dir).await
    } else {
        probe_http(config).await
    }
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("配置字段 {field} 必须是非空字符串"))
}

fn string_map(value: Option<&Value>, field: &str) -> Result<BTreeMap<String, String>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let map = value
        .as_object()
        .ok_or_else(|| anyhow!("{field} 必须是字符串映射"))?;
    map.iter()
        .map(|(key, value)| {
            // Do not reflect an untrusted key, which could itself contain a secret.
            let value = value
                .as_str()
                .ok_or_else(|| anyhow!("{field} 包含非字符串值"))?;
            Ok((key.clone(), value.to_owned()))
        })
        .collect()
}

fn environment(name: &str) -> Result<String> {
    if name.is_empty()
        || name.len() > 128
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        bail!("凭证引用必须是有效的环境变量名");
    }
    std::env::var(name).map_err(|_| anyhow!("凭证环境变量未设置或不是有效文本"))
}

fn initialize() -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":PROTOCOLS[0],"capabilities":{},"clientInfo":{"name":"codey-connectivity-check","version":"1"}}})
}

fn initialized() -> Value {
    json!({"jsonrpc":"2.0","method":"notifications/initialized"})
}
fn tools_request() -> Value {
    json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}})
}

/// Reject malformed envelopes and mismatched IDs rather than accepting arbitrary JSON.
fn rpc_result(message: Value, id: u64) -> Result<Option<Value>> {
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || !message.is_object() {
        bail!("服务返回了无效的 JSON-RPC 消息");
    }
    if message.get("id").is_none() {
        if message.get("method").and_then(Value::as_str).is_some()
            && message.get("result").is_none()
            && message.get("error").is_none()
        {
            return Ok(None);
        }
        bail!("服务返回了无效的 JSON-RPC 通知");
    }
    if message.get("id").and_then(Value::as_u64) != Some(id) || message.get("method").is_some() {
        bail!("服务返回了不匹配的 JSON-RPC 响应标识");
    }
    if message.get("error").is_some() {
        bail!("服务返回了 JSON-RPC 错误；请查看服务自身日志");
    }
    message
        .get("result")
        .cloned()
        .map(Some)
        .ok_or_else(|| anyhow!("JSON-RPC 响应缺少 result"))
}

fn handshake(value: &Value) -> Result<(String, bool, Value)> {
    let protocol = value
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("握手响应缺少协议版本"))?;
    if !PROTOCOLS.contains(&protocol) {
        bail!("服务协商了当前检查器不支持的 MCP 协议版本");
    }
    let capabilities = value
        .get("capabilities")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("握手响应缺少有效的 capabilities"))?;
    let info = value
        .get("serverInfo")
        .ok_or_else(|| anyhow!("握手响应缺少 serverInfo"))?;
    if info.get("name").and_then(Value::as_str).is_none()
        || info.get("version").and_then(Value::as_str).is_none()
    {
        bail!("握手响应中的 serverInfo 无效");
    }
    if capabilities.get("tools").is_some_and(|v| !v.is_object()) {
        bail!("tools 能力声明无效");
    }
    let clean = |field: &str| {
        info[field]
            .as_str()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_control())
            .take(128)
            .collect::<String>()
    };
    Ok((
        protocol.into(),
        capabilities.contains_key("tools"),
        json!({"name":clean("name"),"version":clean("version")}),
    ))
}

fn tools_summary(result: &Value) -> Result<String> {
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("tools/list 响应缺少 tools 数组"))?;
    if tools.len() > 1000
        || tools.iter().any(|t| {
            t.get("name").and_then(Value::as_str).is_none()
                || !t.get("inputSchema").is_some_and(Value::is_object)
        })
    {
        bail!("tools/list 返回的工具结构无效或数量超过检查上限");
    }
    Ok(format!(
        "握手完成，已检查首批 {} 个工具的描述；未执行工具",
        tools.len()
    ))
}

/// fill_buf is capped by BufReader, and accumulation is checked before extending.
async fn bounded_line<R: AsyncBufRead + Unpin>(reader: &mut R, line: &mut Vec<u8>) -> Result<bool> {
    line.clear();
    loop {
        let buf = reader
            .fill_buf()
            .await
            .map_err(|_| anyhow!("读取服务输出失败"))?;
        if buf.is_empty() {
            return Ok(!line.is_empty());
        }
        let count = buf
            .iter()
            .position(|b| *b == b'\n')
            .map_or(buf.len(), |n| n + 1);
        if line.len() + count > MAX_BODY {
            bail!("服务消息超过 1 MiB 检查上限");
        }
        let complete = buf[count - 1] == b'\n';
        line.extend_from_slice(&buf[..count]);
        reader.consume(count);
        if complete {
            return Ok(true);
        }
    }
}

async fn stdio_response<R: AsyncBufRead + Unpin>(reader: &mut R, id: u64) -> Result<Value> {
    let mut line = Vec::new();
    let mut total = 0;
    for _ in 0..MAX_MESSAGES {
        if !bounded_line(reader, &mut line).await? {
            bail!("服务在完成握手前关闭了输出");
        }
        total += line.len();
        if total > MAX_BODY {
            bail!("服务响应累计超过 1 MiB 检查上限");
        }
        let message = serde_json::from_slice(&line)
            .map_err(|_| anyhow!("服务标准输出不是有效的 JSON-RPC；日志应写入标准错误"))?;
        if let Some(result) = rpc_result(message, id)? {
            return Ok(result);
        }
    }
    bail!("等待响应期间收到过多消息，已停止检查")
}

async fn write_message(writer: &mut (impl AsyncWriteExt + Unpin), message: Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(&message).map_err(|_| anyhow!("无法构造检查请求"))?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .await
        .map_err(|_| anyhow!("发送检查请求失败"))?;
    writer
        .flush()
        .await
        .map_err(|_| anyhow!("发送检查请求失败"))
}

struct OwnedProcess {
    child: Option<Child>,
    #[cfg(unix)]
    group: i32,
    #[cfg(windows)]
    job: windows::Win32::Foundation::HANDLE,
}

// Windows HANDLE represents a uniquely owned job object, only closed by this guard.
#[cfg(windows)]
unsafe impl Send for OwnedProcess {}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            if self.group != 0 {
                libc::kill(-self.group, libc::SIGKILL);
            }
        }
        #[cfg(windows)]
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.job);
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
                });
            }
        }
    }
}

#[cfg(windows)]
fn attach_job(child: &Child) -> Result<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::{
        Foundation::CloseHandle,
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
    };
    unsafe {
        let job = CreateJobObjectW(None, None).map_err(|_| anyhow!("无法创建检查进程隔离组"))?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let result = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            std::mem::size_of_val(&limits) as u32,
        )
        .and_then(|_| {
            AssignProcessToJobObject(
                job,
                HANDLE(child.raw_handle().unwrap_or(std::ptr::null_mut())),
            )
        });
        if result.is_err() {
            let _ = CloseHandle(job);
            bail!("无法隔离检查进程，已取消启动");
        }
        Ok(job)
    }
}

/// The child has not executed user code yet: attach its Job before resuming it.
#[cfg(windows)]
fn resume_process(child: &Child) -> Result<()> {
    use windows::Win32::{
        Foundation::CloseHandle,
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First,
                Thread32Next,
            },
            Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
        },
    };
    let pid = child.id().ok_or_else(|| anyhow!("无法读取检查进程标识"))?;
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)
            .map_err(|_| anyhow!("无法读取暂停的检查线程"))?;
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut next = Thread32First(snapshot, &mut entry).is_ok();
        let mut thread_id = None;
        while next {
            if entry.th32OwnerProcessID == pid {
                thread_id = Some(entry.th32ThreadID);
                break;
            }
            next = Thread32Next(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
        let thread_id = thread_id.ok_or_else(|| anyhow!("无法找到暂停的检查线程"))?;
        let thread = OpenThread(THREAD_SUSPEND_RESUME, false, thread_id)
            .map_err(|_| anyhow!("无法打开暂停的检查线程"))?;
        let resumed = ResumeThread(thread);
        let _ = CloseHandle(thread);
        if resumed == u32::MAX {
            bail!("无法恢复检查进程");
        }
    }
    Ok(())
}

async fn probe_stdio(config: &Value, project_dir: Option<PathBuf>) -> Result<ProbeSuccess> {
    let executable = required_string(config, "command")?;
    #[cfg(windows)]
    if [
        "cmd",
        "cmd.exe",
        "powershell",
        "powershell.exe",
        "pwsh",
        "pwsh.exe",
    ]
    .contains(
        &executable
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
    ) || executable.to_ascii_lowercase().ends_with(".cmd")
        || executable.to_ascii_lowercase().ends_with(".bat")
    {
        bail!("Windows 检查不经命令解释器启动；请配置 node.exe 或其他原生可执行文件及独立参数");
    }
    let mut command = Command::new(executable);
    if let Some(args) = config.get("args") {
        let args = args
            .as_array()
            .ok_or_else(|| anyhow!("args 必须是字符串数组"))?;
        for arg in args {
            command.arg(arg.as_str().ok_or_else(|| anyhow!("args 包含非字符串值"))?);
        }
    }
    command.env_clear();
    // Codex-compatible minimal environment; unrelated credentials are not inherited.
    for name in [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "TMPDIR",
        "TMP",
        "TEMP",
        "SystemRoot",
        "SYSTEMROOT",
        "WINDIR",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "PATHEXT",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    if let Some(names) = config.get("env_vars") {
        for name in names
            .as_array()
            .ok_or_else(|| anyhow!("env_vars 必须是字符串数组"))?
        {
            let name = name
                .as_str()
                .ok_or_else(|| anyhow!("env_vars 包含非字符串值"))?;
            command.env(name, environment(name)?);
        }
    }
    for (name, value) in string_map(config.get("env"), "env")? {
        command.env(name, value);
    }
    let cwd = match config.get("cwd") {
        Some(value) => Some(PathBuf::from(
            value.as_str().ok_or_else(|| anyhow!("cwd 必须是字符串"))?,
        )),
        None => project_dir,
    };
    if let Some(cwd) = cwd {
        if !cwd.is_absolute() || !cwd.is_dir() {
            bail!("工作目录必须是存在的绝对目录");
        }
        command.current_dir(cwd);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(
        windows::Win32::System::Threading::CREATE_SUSPENDED.0
            | windows::Win32::System::Threading::CREATE_NO_WINDOW.0,
    );
    let child = command
        .spawn()
        .map_err(|_| anyhow!("无法启动 MCP 程序，请检查可执行文件、权限和工作目录"))?;
    #[cfg(windows)]
    let job = attach_job(&child)?;
    let mut process = OwnedProcess {
        #[cfg(unix)]
        group: child.id().ok_or_else(|| anyhow!("无法读取检查进程标识"))? as i32,
        #[cfg(windows)]
        job,
        child: Some(child),
    };
    #[cfg(windows)]
    resume_process(process.child.as_ref().unwrap())?;
    let mut input = process
        .child
        .as_mut()
        .unwrap()
        .stdin
        .take()
        .ok_or_else(|| anyhow!("无法连接服务标准输入"))?;
    let mut output = BufReader::new(
        process
            .child
            .as_mut()
            .unwrap()
            .stdout
            .take()
            .ok_or_else(|| anyhow!("无法连接服务标准输出"))?,
    );
    let result = async {
        write_message(&mut input, initialize()).await?;
        let (protocol, has_tools, server_info) = handshake(&stdio_response(&mut output, 1).await?)?;
        write_message(&mut input, initialized()).await?;
        let message = if has_tools {
            write_message(&mut input, tools_request()).await?;
            tools_summary(&stdio_response(&mut output, 2).await?)?
        } else {
            "握手完成；服务未声明 tools 能力，未执行工具".into()
        };
        Ok(ProbeSuccess {
            message,
            protocol,
            server_info,
        })
    }
    .await;
    // Drop closes the complete owned group, including on timeout or task cancellation.
    #[cfg(unix)]
    unsafe {
        libc::kill(-process.group, libc::SIGKILL);
        process.group = 0;
    }
    let _ = process.child.as_mut().unwrap().start_kill();
    let _ = process.child.as_mut().unwrap().wait().await;
    result
}

fn http_settings(config: &Value) -> Result<(Url, HeaderMap)> {
    let url = Url::parse(required_string(config, "url")?).map_err(|_| anyhow!("MCP URL 无效"))?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        bail!("MCP URL 不得包含用户信息或片段");
    }
    let local = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if url.scheme() != "https" && !(url.scheme() == "http" && local) {
        bail!("请使用 HTTPS；仅回环地址允许 HTTP 开发检查");
    }
    let mut headers = HeaderMap::new();
    let mut values = string_map(config.get("http_headers"), "http_headers")?;
    if let Some(token) = config.get("bearer_token") {
        let token = token
            .as_str()
            .ok_or_else(|| anyhow!("bearer_token 必须是字符串"))?;
        values.insert("Authorization".into(), format!("Bearer {token}"));
    }
    for (name, variable) in string_map(config.get("env_http_headers"), "env_http_headers")? {
        values.insert(name, environment(&variable)?);
    }
    if let Some(variable) = config.get("bearer_token_env_var") {
        let variable = variable
            .as_str()
            .ok_or_else(|| anyhow!("bearer_token_env_var 必须是字符串"))?;
        values.insert(
            "Authorization".into(),
            format!("Bearer {}", environment(variable)?),
        );
    }
    for (name, value) in values {
        let name =
            HeaderName::from_bytes(name.as_bytes()).map_err(|_| anyhow!("HTTP 请求头名称无效"))?;
        if [
            "host",
            "content-length",
            "transfer-encoding",
            "connection",
            "mcp-session-id",
            "mcp-protocol-version",
        ]
        .contains(&name.as_str())
        {
            bail!("配置包含由传输层管理的 HTTP 请求头");
        }
        let mut value = HeaderValue::from_str(&value).map_err(|_| anyhow!("HTTP 请求头值无效"))?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    headers.insert(
        "accept",
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    Ok((url, headers))
}

fn status(response: &Response) -> Result<()> {
    match response.status().as_u16() {
        401 | 403 => bail!(
            "服务需要认证；请使用 Codex 原生认证，并检查环境变量凭证。检查器不会复制 Codex OAuth 凭证"
        ),
        300..=399 => bail!("服务返回重定向；检查器不会携带凭证跳转，请直接配置最终地址"),
        200..=299 => Ok(()),
        _ => bail!(
            "服务返回 HTTP {}，请查看服务自身日志",
            response.status().as_u16()
        ),
    }
}

async fn http_response(mut response: Response, id: u64) -> Result<Value> {
    status(&response)?;
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    let sse = content_type.eq_ignore_ascii_case("text/event-stream");
    if !sse && !content_type.eq_ignore_ascii_case("application/json") {
        bail!("服务响应必须为 JSON 或 SSE");
    }
    let mut bytes = Vec::new();
    let mut data = Vec::new();
    let mut total = 0;
    let mut messages = 0;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("读取 HTTP 服务响应失败"))?
    {
        total += chunk.len();
        if total > MAX_BODY {
            bail!("HTTP 响应超过 1 MiB 检查上限");
        }
        // Retained SSE bytes have already been searched for a newline.
        let mut search_from = bytes.len();
        bytes.extend_from_slice(&chunk);
        if sse {
            let mut consumed = 0;
            while let Some(end) = bytes[search_from..].iter().position(|b| *b == b'\n') {
                let end = search_from + end;
                let line = &bytes[consumed..end];
                consumed = end + 1;
                search_from = consumed;
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                if line.is_empty() && !data.is_empty() {
                    messages += 1;
                    if messages > MAX_MESSAGES {
                        bail!("SSE 消息数量超过检查上限");
                    }
                    let message = serde_json::from_slice(&data)
                        .map_err(|_| anyhow!("SSE data 不是有效的 JSON-RPC"))?;
                    data.clear();
                    if let Some(result) = rpc_result(message, id)? {
                        return Ok(result);
                    }
                } else if let Some(value) = line.strip_prefix(b"data:") {
                    let value = value.strip_prefix(b" ").unwrap_or(value);
                    if !data.is_empty() {
                        data.push(b'\n');
                    }
                    data.extend_from_slice(value);
                }
            }
            bytes.drain(..consumed);
        }
    }
    if sse {
        bail!("SSE 流在返回匹配的响应前结束");
    }
    let message =
        serde_json::from_slice(&bytes).map_err(|_| anyhow!("HTTP 响应不是有效的 JSON-RPC"))?;
    rpc_result(message, id)?.ok_or_else(|| anyhow!("HTTP 响应未包含请求结果"))
}

struct HttpSession {
    cleanup: Option<(Client, Url, HeaderMap)>,
}

impl HttpSession {
    async fn close(&mut self) -> bool {
        // Retain ownership during await so cancellation can still schedule cleanup.
        let Some((client, url, headers)) = self.cleanup.clone() else {
            return true;
        };
        let result = close_http_session(client, url, headers).await;
        self.cleanup = None;
        result
    }
}

async fn close_http_session(client: Client, url: Url, headers: HeaderMap) -> bool {
    let cleanup = tokio::time::timeout(
        Duration::from_secs(2),
        client.delete(url).headers(headers).send(),
    )
    .await;
    matches!(cleanup, Ok(Ok(ref response)) if response.status().is_success() || response.status().as_u16() == 404)
}

impl Drop for HttpSession {
    fn drop(&mut self) {
        if let Some((client, url, headers)) = self.cleanup.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            // Cancellation is best effort; normal completion awaits and reports cleanup.
            runtime.spawn(close_http_session(client, url, headers));
        }
    }
}

async fn probe_http(config: &Value) -> Result<ProbeSuccess> {
    let (url, mut headers) = http_settings(config)?;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .build()
        .map_err(|_| anyhow!("无法创建 HTTP 检查连接"))?;
    let response = client
        .post(url.clone())
        .headers(headers.clone())
        .json(&initialize())
        .send()
        .await
        .map_err(|_| anyhow!("无法连接 HTTP 服务，请检查地址、网络和 TLS 配置"))?;
    status(&response)?;
    let session = response.headers().get("mcp-session-id").cloned();
    if session.as_ref().is_some_and(|value| {
        value.as_bytes().is_empty() || !value.as_bytes().iter().all(|b| (0x21..=0x7e).contains(b))
    }) {
        bail!("服务返回的会话标识无效");
    }
    if let Some(mut session) = session.clone() {
        session.set_sensitive(true);
        headers.insert("mcp-session-id", session);
    }
    let mut session_guard = HttpSession {
        cleanup: session
            .as_ref()
            .map(|_| (client.clone(), url.clone(), headers.clone())),
    };
    let outcome = tokio::time::timeout(Duration::from_secs(12), async {
        let (protocol, has_tools, server_info) = handshake(&http_response(response, 1).await?)?;
        headers.insert(
            "mcp-protocol-version",
            HeaderValue::from_str(&protocol).map_err(|_| anyhow!("协商协议版本无效"))?,
        );
        if let Some((_, _, cleanup_headers)) = session_guard.cleanup.as_mut() {
            *cleanup_headers = headers.clone();
        }
        let mut response = client
            .post(url.clone())
            .headers(headers.clone())
            .json(&initialized())
            .send()
            .await
            .map_err(|_| anyhow!("发送初始化通知失败"))?;
        status(&response)?;
        if ![200, 202, 204].contains(&response.status().as_u16()) {
            bail!("服务未按协议确认初始化通知");
        }
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow!("读取初始化通知响应失败"))?
        {
            if !chunk.is_empty() {
                bail!("初始化通知响应必须为空");
            }
        }
        if !has_tools {
            return Ok(ProbeSuccess {
                message: "握手完成；服务未声明 tools 能力，未执行工具".into(),
                protocol,
                server_info,
            });
        }
        let response = client
            .post(url.clone())
            .headers(headers.clone())
            .json(&tools_request())
            .send()
            .await
            .map_err(|_| anyhow!("发送工具描述查询失败"))?;
        Ok(ProbeSuccess {
            message: tools_summary(&http_response(response, 2).await?)?,
            protocol,
            server_info,
        })
    })
    .await
    .unwrap_or_else(|_| Err(anyhow!("HTTP 握手超时，已停止检查")));
    if !session_guard.close().await && outcome.is_ok() {
        bail!("握手通过，但无法确认检查会话已关闭；服务可能保留该会话直到超时");
    }
    outcome
}

#[cfg(test)]
#[path = "probe/tests.rs"]
mod tests;
