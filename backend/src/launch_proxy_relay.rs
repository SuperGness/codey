//! Codex 启动代理的本地认证中继。
//!
//! Chromium 的 `--proxy-server` 不支持凭据，子进程对凭据 URL 的理解也
//! 不一致；上游地址带用户名密码时，Codey 在回环地址起一个只服务本机的
//! 转发代理，客户端与子进程统一使用无凭据的 `http://127.0.0.1:<port>`，
//! 由中继向上游完成认证：http/https 上游注入 `Proxy-Authorization`，
//! socks5 上游走 RFC 1929 用户名密码握手（socks5h 仍由上游解析目标域名）。

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use anyhow::{Context, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OnceCell, Semaphore};

/// 直连与认证中继共用上游解析和探活；原始地址仅用作中继复用键，
/// 凭据由中继发给上游，不写入 Codex 或共享 app-server 的环境变量。
#[derive(Clone)]
pub(crate) struct UpstreamProxy {
    raw: String,
    scheme: UpstreamScheme,
    host: String,
    port: u16,
    username: Vec<u8>,
    password: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UpstreamScheme {
    Http,
    /// https 指用 TLS 连接代理本身。
    Https,
    Socks5,
    /// 目标域名交给上游解析。
    Socks5h,
}

impl UpstreamProxy {
    /// 解析已通过 `validate_codex_launch_proxy` 校验的地址；host/port
    /// 缺失等异常情况返回 None。
    pub(crate) fn parse(url: &reqwest::Url, raw: &str) -> Option<Self> {
        let scheme = match url.scheme() {
            "http" => UpstreamScheme::Http,
            "https" => UpstreamScheme::Https,
            "socks5" => UpstreamScheme::Socks5,
            "socks5h" => UpstreamScheme::Socks5h,
            _ => return None,
        };
        Some(Self {
            raw: raw.to_string(),
            scheme,
            host: unbracketed_host(url.host_str()?).to_string(),
            port: url.port_or_known_default()?,
            username: percent_decode(url.username()),
            password: percent_decode(url.password().unwrap_or_default()),
        })
    }

    /// 地址是否携带用户名（决定走本地认证中继还是直连上游）。
    pub(crate) fn has_credentials(&self) -> bool {
        !self.username.is_empty() || !self.password.is_empty()
    }

    /// 不带凭据的展示地址，用于错误信息。
    fn display_addr(&self) -> String {
        match self.host.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V6(_)) => format!("[{}]:{}", self.host, self.port),
            _ => format!("{}:{}", self.host, self.port),
        }
    }

    fn basic_auth_header(&self) -> Option<String> {
        if !self.has_credentials() {
            return None;
        }
        let mut credentials = self.username.clone();
        credentials.push(b':');
        credentials.extend_from_slice(&self.password);
        Some(format!("Basic {}", BASE64.encode(credentials)))
    }
}

pub(crate) fn percent_decode(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (
                (bytes[index + 1] as char).to_digit(16),
                (bytes[index + 2] as char).to_digit(16),
            )
        {
            decoded.push(((high << 4) | low) as u8);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    decoded
}

/// URL 中的 IPv6 主机带方括号；网络连接与 SOCKS 地址编码使用裸 IP。
fn unbracketed_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host)
}

/// RFC 1929 的 ULEN/PLEN 只有一个字节。
pub(crate) fn socks_credentials_too_long(username: &[u8], password: &[u8]) -> bool {
    username.len() > u8::MAX as usize || password.len() > u8::MAX as usize
}

const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HEAD_READ_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_HEAD_BYTES: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 128;

/// 同一上游复用仍在运行的监听任务；配置变化本来就要求重启，旧中继不主动
/// 关闭，避免正在运行的 Codex 断网。
static RELAYS: Mutex<BTreeMap<String, Arc<OnceCell<RunningRelay>>>> = Mutex::new(BTreeMap::new());

struct RunningRelay {
    port: u16,
    task: tokio::task::JoinHandle<()>,
}

struct RelayRegistration<'a> {
    key: &'a str,
    relay: Option<Arc<OnceCell<RunningRelay>>>,
}

impl Drop for RelayRegistration<'_> {
    fn drop(&mut self) {
        let Some(relay) = self.relay.take() else {
            return;
        };
        if relay.get().is_some() {
            return;
        }
        let mut relays = RELAYS.lock().unwrap_or_else(|error| error.into_inner());
        // 同一把锁保护查找与移除，保证其他调用拿到的初始化状态仍在注册表中。
        if relay.get().is_none()
            && Arc::strong_count(&relay) == 2
            && relays
                .get(self.key)
                .is_some_and(|registered| Arc::ptr_eq(registered, &relay))
        {
            relays.remove(self.key);
        }
        // 在锁内释放本次引用，让最后一个同时退出的调用能清理空条目。
        drop(relay);
    }
}

/// 确保上游的回环认证中继在运行，返回其监听端口。
pub(crate) async fn ensure_running(upstream: &UpstreamProxy) -> anyhow::Result<u16> {
    let relay = RelayRegistration {
        key: &upstream.raw,
        relay: Some({
            let mut relays = RELAYS.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(relay) = relays.get(&upstream.raw).filter(|relay| {
                !relay
                    .get()
                    .is_some_and(|running| running.task.is_finished())
            }) {
                Arc::clone(relay)
            } else {
                let relay = Arc::new(OnceCell::new());
                relays.insert(upstream.raw.clone(), Arc::clone(&relay));
                relay
            }
        }),
    };
    // 仅同一上游等待初始化；探活失败或调用被取消后，后续调用可重试。
    let running = relay
        .relay
        .as_ref()
        .expect("中继注册引用仅在退出时释放")
        .get_or_try_init(|| async {
            preflight(upstream).await?;
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .context("启动 Codex 代理认证中继失败")?;
            spawn_relay(listener, Arc::new(upstream.clone()))
        })
        .await?;
    Ok(running.port)
}

fn spawn_relay(
    listener: TcpListener,
    upstream: Arc<UpstreamProxy>,
) -> anyhow::Result<RunningRelay> {
    let port = listener.local_addr()?.port();
    let task = tokio::spawn(async move { accept_loop(listener, upstream).await });
    Ok(RunningRelay { port, task })
}

/// 启动前先探活上游：代理连不通时立刻报错，而不是让 Codex 带着死代理
/// 静默挂起。https 顺带完成 TLS 握手，SOCKS 上游顺带完成认证协商，凭据
/// 错误在此暴露。
pub(crate) async fn preflight(upstream: &UpstreamProxy) -> anyhow::Result<()> {
    let result = tokio::time::timeout(UPSTREAM_CONNECT_TIMEOUT, async {
        match upstream.scheme {
            UpstreamScheme::Http => open_upstream_tcp(upstream).await.map(|_| ()),
            UpstreamScheme::Https => open_upstream_stream(upstream).await.map(|_| ()),
            UpstreamScheme::Socks5 | UpstreamScheme::Socks5h => {
                let mut stream = open_upstream_tcp(upstream).await?;
                socks5_negotiate(&mut stream, upstream).await
            }
        }
    })
    .await
    .context("上游代理探活超时")
    .and_then(std::convert::identity);
    result.with_context(|| format!("Codex 启动代理上游 {} 不可用", upstream.display_addr()))
}

async fn open_upstream_tcp(upstream: &UpstreamProxy) -> anyhow::Result<TcpStream> {
    let connect = TcpStream::connect((upstream.host.as_str(), upstream.port));
    Ok(tokio::time::timeout(UPSTREAM_CONNECT_TIMEOUT, connect)
        .await
        .context("连接超时")??)
}

enum UpstreamIo {
    Tcp(TcpStream),
    /// TLS 变体比 TCP 大一个数量级，装箱避免每条连接都在栈上搬整块状态。
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl AsyncRead for UpstreamIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for UpstreamIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

async fn open_upstream_stream(upstream: &UpstreamProxy) -> anyhow::Result<UpstreamIo> {
    let stream = open_upstream_tcp(upstream).await?;
    if upstream.scheme != UpstreamScheme::Https {
        return Ok(UpstreamIo::Tcp(stream));
    }
    let name = upstream
        .host
        .parse::<std::net::IpAddr>()
        .map(|ip| rustls::pki_types::ServerName::IpAddress(ip.into()))
        .or_else(|_| rustls::pki_types::ServerName::try_from(upstream.host.clone()))
        .context("Codex 启动代理上游主机名无效")?;
    let stream = tokio::time::timeout(
        UPSTREAM_CONNECT_TIMEOUT,
        tls_connector().connect(name, stream),
    )
    .await
    .context("上游 TLS 握手超时")?
    .context("上游 TLS 握手失败（https:// 表示用 TLS 连接代理本身）")?;
    Ok(UpstreamIo::Tls(Box::new(stream)))
}

fn tls_connector() -> tokio_rustls::TlsConnector {
    tokio_rustls::TlsConnector::from(crate::local_router::shared_client_config())
}

async fn accept_loop(listener: TcpListener, upstream: Arc<UpstreamProxy>) {
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let Ok((client, _)) = listener.accept().await else {
            return;
        };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            continue;
        };
        let upstream = Arc::clone(&upstream);
        tokio::spawn(async move {
            let _permit = permit;
            let _ = serve(client, upstream).await;
        });
    }
}

/// 读入客户端请求头，返回（全部已读字节，头部结束偏移）。头部之外的字节
/// 属于隧道或请求体，必须原样转发。
async fn read_head(stream: &mut (impl AsyncRead + Unpin)) -> std::io::Result<(Vec<u8>, usize)> {
    let deadline = tokio::time::Instant::now() + HEAD_READ_TIMEOUT;
    let mut buffer = Vec::with_capacity(2048);
    loop {
        let head_end = memchr::memmem::find(&buffer, b"\r\n\r\n").map(|index| index + 4);
        if head_end.unwrap_or(buffer.len()) > MAX_HEAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "代理请求头过长",
            ));
        }
        if let Some(end) = head_end {
            return Ok((buffer, end));
        }
        let mut chunk = [0u8; 8192];
        let read = tokio::time::timeout_at(deadline, stream.read(&mut chunk))
            .await
            .unwrap_or_else(|_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "读取代理请求头超时",
                ))
            })?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "代理请求头不完整",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

fn parse_request_line(head: &[u8]) -> Option<(&str, &str)> {
    let line_end = memchr::memmem::find(head, b"\r\n")?;
    let line = std::str::from_utf8(&head[..line_end]).ok()?;
    let (method, rest) = line.split_once(' ')?;
    let (target, _) = rest.rsplit_once(' ')?;
    Some((method, target))
}

async fn respond_status(stream: &mut TcpStream, status: u16, reason: &str) -> anyhow::Result<()> {
    stream
        .write_all(
            format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await?;
    Ok(())
}

async fn serve(mut client: TcpStream, upstream: Arc<UpstreamProxy>) -> anyhow::Result<()> {
    let (buffer, head_end) = read_head(&mut client).await?;
    let Some((method, target)) = parse_request_line(&buffer[..head_end]) else {
        return respond_status(&mut client, 400, "Bad Request").await;
    };
    if method.eq_ignore_ascii_case("CONNECT") {
        serve_connect(client, upstream, target, &buffer[head_end..]).await
    } else {
        serve_forward(client, upstream, target, &buffer, head_end).await
    }
}

/// `CONNECT` 请求：对 http/https 上游改写为带凭据的 CONNECT，对 socks5
/// 上游完成握手，之后双向拷贝隧道字节。
async fn serve_connect(
    mut client: TcpStream,
    upstream: Arc<UpstreamProxy>,
    authority: &str,
    buffered: &[u8],
) -> anyhow::Result<()> {
    let Some((host, port)) = split_authority(authority) else {
        return respond_status(&mut client, 400, "Bad Request").await;
    };
    let mut stream = match upstream.scheme {
        UpstreamScheme::Http | UpstreamScheme::Https => {
            let mut stream = match open_upstream_stream(&upstream).await {
                Ok(stream) => stream,
                Err(_) => return respond_status(&mut client, 502, "Bad Gateway").await,
            };
            let mut request =
                format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n").into_bytes();
            if let Some(auth) = upstream.basic_auth_header() {
                request.extend_from_slice(format!("Proxy-Authorization: {auth}\r\n").as_bytes());
            }
            request.extend_from_slice(b"\r\n");
            if stream.write_all(&request).await.is_err() {
                return respond_status(&mut client, 502, "Bad Gateway").await;
            }
            let (response, response_end) = match read_head(&mut stream).await {
                Ok(head) => head,
                Err(_) => return respond_status(&mut client, 502, "Bad Gateway").await,
            };
            let status = parse_status_code(&response[..response_end]);
            if !(200..300).contains(&status) {
                // 把上游的应答（典型是 407）原样交给客户端，便于排障。
                let _ = client.write_all(&response).await;
                return Ok(());
            }
            client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
            if response.len() > response_end {
                client.write_all(&response[response_end..]).await?;
            }
            if !buffered.is_empty() {
                stream.write_all(buffered).await?;
            }
            tokio::io::copy_bidirectional(&mut client, &mut stream).await?;
            return Ok(());
        }
        UpstreamScheme::Socks5 | UpstreamScheme::Socks5h => {
            match open_upstream_tcp(&upstream).await {
                Ok(stream) => stream,
                Err(_) => return respond_status(&mut client, 502, "Bad Gateway").await,
            }
        }
    };
    if socks5_connect(&mut stream, &upstream, &host, port)
        .await
        .is_err()
    {
        return respond_status(&mut client, 502, "Bad Gateway").await;
    }
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    if !buffered.is_empty() {
        stream.write_all(buffered).await?;
    }
    tokio::io::copy_bidirectional(&mut client, &mut stream).await?;
    Ok(())
}

/// 非 CONNECT 的 absolute-URI 请求（纯 http 明文转发）。http 上游原样转发
/// 请求行并注入凭据；socks 上游改写为 origin-form 直连目标。
async fn serve_forward(
    mut client: TcpStream,
    upstream: Arc<UpstreamProxy>,
    target: &str,
    buffer: &[u8],
    head_end: usize,
) -> anyhow::Result<()> {
    let Some(origin) = reqwest::Url::parse(target)
        .ok()
        .filter(|url| url.scheme().eq_ignore_ascii_case("http") && url.host_str().is_some())
    else {
        return respond_status(&mut client, 400, "Bad Request").await;
    };
    match upstream.scheme {
        UpstreamScheme::Http | UpstreamScheme::Https => {
            let mut stream = match open_upstream_stream(&upstream).await {
                Ok(stream) => stream,
                Err(_) => return respond_status(&mut client, 502, "Bad Gateway").await,
            };
            let head = rewrite_forward_head(
                &buffer[..head_end],
                None,
                None,
                upstream.basic_auth_header().as_deref(),
            );
            if stream.write_all(&head).await.is_err()
                || stream.write_all(&buffer[head_end..]).await.is_err()
            {
                return Ok(());
            }
            tokio::io::copy_bidirectional(&mut client, &mut stream).await?;
            Ok(())
        }
        UpstreamScheme::Socks5 | UpstreamScheme::Socks5h => {
            let authority = origin.host_str().unwrap_or_default();
            let host = unbracketed_host(authority);
            let port = origin.port_or_known_default().unwrap_or(80);
            let mut stream = match open_upstream_tcp(&upstream).await {
                Ok(stream) => stream,
                Err(_) => return respond_status(&mut client, 502, "Bad Gateway").await,
            };
            if socks5_connect(&mut stream, &upstream, host, port)
                .await
                .is_err()
            {
                return respond_status(&mut client, 502, "Bad Gateway").await;
            }
            let authority = match origin.port() {
                Some(port) => format!("{authority}:{port}"),
                None => authority.to_string(),
            };
            let mut path = origin.path().to_string();
            if path.is_empty() {
                path.push('/');
            }
            if let Some(query) = origin.query() {
                path.push('?');
                path.push_str(query);
            }
            let head = rewrite_forward_head(
                &buffer[..head_end],
                Some(path.as_bytes()),
                Some(&authority),
                None,
            );
            if stream.write_all(&head).await.is_err()
                || stream.write_all(&buffer[head_end..]).await.is_err()
            {
                return Ok(());
            }
            tokio::io::copy_bidirectional(&mut client, &mut stream).await?;
            Ok(())
        }
    }
}

/// 改写非 CONNECT 请求的头部：可替换请求目标为 origin-form、补 Host、注入
/// `Proxy-Authorization`，并统一 `Connection: close` —— 中继按请求处理，
/// 不承担跨请求的连接复用语义。
fn rewrite_forward_head(
    head: &[u8],
    origin_target: Option<&[u8]>,
    host_header: Option<&str>,
    basic_auth: Option<&str>,
) -> Vec<u8> {
    let Some(line_end) = memchr::memmem::find(head, b"\r\n") else {
        return head.to_vec();
    };
    let request_line = &head[..line_end];
    let mut output = Vec::with_capacity(head.len() + 160);
    match origin_target {
        Some(target) => {
            let method = request_line
                .split(|byte| *byte == b' ')
                .next()
                .unwrap_or_default();
            let version = request_line
                .rsplit(|byte| *byte == b' ')
                .next()
                .unwrap_or(b"HTTP/1.1");
            output.extend_from_slice(method);
            output.push(b' ');
            output.extend_from_slice(target);
            output.push(b' ');
            output.extend_from_slice(version);
        }
        None => output.extend_from_slice(request_line),
    }
    output.extend_from_slice(b"\r\n");
    let mut rest = &head[line_end + 2..];
    let mut saw_host = false;
    while let Some(end) = memchr::memmem::find(rest, b"\r\n") {
        let line = &rest[..end];
        rest = &rest[end + 2..];
        if line.is_empty() {
            break;
        }
        let name = line
            .iter()
            .position(|byte| *byte == b':')
            .map(|colon| &line[..colon])
            .unwrap_or(line);
        if name.eq_ignore_ascii_case(b"connection")
            || name.eq_ignore_ascii_case(b"proxy-connection")
            || name.eq_ignore_ascii_case(b"proxy-authorization")
        {
            continue;
        }
        saw_host |= name.eq_ignore_ascii_case(b"host");
        output.extend_from_slice(line);
        output.extend_from_slice(b"\r\n");
    }
    if !saw_host && let Some(host) = host_header {
        output.extend_from_slice(format!("Host: {host}\r\n").as_bytes());
    }
    if let Some(auth) = basic_auth {
        output.extend_from_slice(format!("Proxy-Authorization: {auth}\r\n").as_bytes());
    }
    output.extend_from_slice(b"Connection: close\r\n\r\n");
    output
}

fn parse_status_code(head: &[u8]) -> u16 {
    let Some(line_end) = memchr::memmem::find(head, b"\r\n") else {
        return 0;
    };
    let Ok(line) = std::str::from_utf8(&head[..line_end]) else {
        return 0;
    };
    line.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

/// `CONNECT` 目标可能是 `host:port`、`[v6]:port` 或省略端口（按 443）。
fn split_authority(authority: &str) -> Option<(String, u16)> {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        match tail.strip_prefix(':') {
            Some(port) => (host, port.parse().ok()?),
            None if tail.is_empty() => (host, 443),
            None => return None,
        }
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        if host.contains(':') {
            return None;
        }
        (host, port.parse().ok()?)
    } else {
        (authority, 443)
    };
    if host.is_empty() {
        None
    } else {
        Some((host.to_string(), port))
    }
}

/// socks5 方法协商与 RFC 1929 认证；preflight 只跑到这一步，不请求目标。
async fn socks5_negotiate(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    upstream: &UpstreamProxy,
) -> anyhow::Result<()> {
    let greeting: &[u8] = if upstream.has_credentials() {
        &[0x05, 0x02, 0x00, 0x02]
    } else {
        &[0x05, 0x01, 0x00]
    };
    stream.write_all(greeting).await?;
    let mut selection = [0u8; 2];
    stream.read_exact(&mut selection).await?;
    anyhow::ensure!(selection[0] == 0x05, "上游不是 socks5 代理");
    match selection[1] {
        0x00 => Ok(()),
        0x02 => {
            anyhow::ensure!(upstream.has_credentials(), "上游代理选择了未提供的认证方式");
            anyhow::ensure!(
                !socks_credentials_too_long(&upstream.username, &upstream.password),
                "SOCKS 用户名或密码过长"
            );
            let mut auth = vec![0x01, upstream.username.len() as u8];
            auth.extend_from_slice(&upstream.username);
            auth.push(upstream.password.len() as u8);
            auth.extend_from_slice(&upstream.password);
            stream.write_all(&auth).await?;
            let mut reply = [0u8; 2];
            stream.read_exact(&mut reply).await?;
            anyhow::ensure!(reply[0] == 0x01, "上游代理返回了无效的认证版本");
            anyhow::ensure!(reply[1] == 0x00, "上游代理认证失败");
            Ok(())
        }
        0xff => bail!("上游代理不接受可用的认证方式"),
        method => bail!("上游代理选择了不支持的认证方式 {method}"),
    }
}

/// socks5 CONNECT。socks5h 的目标域名交给上游解析；socks5 本地先解析，
/// 与 curl 等客户端对两个 scheme 的约定一致。
async fn socks5_connect(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    upstream: &UpstreamProxy,
    host: &str,
    port: u16,
) -> anyhow::Result<()> {
    tokio::time::timeout(
        UPSTREAM_CONNECT_TIMEOUT,
        socks5_connect_inner(stream, upstream, host, port),
    )
    .await
    .context("SOCKS 连接协商超时")?
}

async fn socks5_connect_inner(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    upstream: &UpstreamProxy,
    host: &str,
    port: u16,
) -> anyhow::Result<()> {
    socks5_negotiate(stream, upstream).await?;
    let mut request = vec![0x05, 0x01, 0x00];
    let resolved = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => Some(ip),
        // socks5h 把域名交给上游解析，跳过本地解析。
        Err(_) if upstream.scheme != UpstreamScheme::Socks5 => None,
        Err(_) => tokio::net::lookup_host((host, port))
            .await?
            .next()
            .map(|address| address.ip()),
    };
    match resolved {
        Some(std::net::IpAddr::V4(ip)) => {
            request.push(0x01);
            request.extend_from_slice(&ip.octets());
        }
        Some(std::net::IpAddr::V6(ip)) => {
            request.push(0x04);
            request.extend_from_slice(&ip.octets());
        }
        None => {
            anyhow::ensure!(host.len() <= u8::MAX as usize, "目标域名过长");
            request.push(0x03);
            request.push(host.len() as u8);
            request.extend_from_slice(host.as_bytes());
        }
    }
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).await?;
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    anyhow::ensure!(head[0] == 0x05, "上游不是 socks5 代理");
    anyhow::ensure!(head[2] == 0x00, "上游返回了无效的 SOCKS 保留字段");
    let bound = match head[3] {
        0x01 => 4,
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            len[0] as usize
        }
        0x04 => 16,
        atyp => bail!("上游返回了未知的地址类型 {atyp}"),
    };
    let mut tail = vec![0u8; bound + 2];
    stream.read_exact(&mut tail).await?;
    match head[1] {
        0x00 => Ok(()),
        0x02 => bail!("上游拒绝连接：不被允许"),
        0x03 => bail!("上游拒绝连接：网络不可达"),
        0x04 => bail!("上游拒绝连接：主机不可达"),
        0x05 => bail!("上游拒绝连接：连接被拒绝"),
        0x06 => bail!("上游拒绝连接：TTL 过期"),
        0x07 => bail!("上游拒绝连接：不支持的命令"),
        code => bail!("上游拒绝连接（{code}）"),
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv6Addr;

    use super::*;

    fn upstream(raw: &str) -> UpstreamProxy {
        let url = reqwest::Url::parse(raw).unwrap();
        UpstreamProxy::parse(&url, raw).expect("地址带凭据")
    }

    async fn read_request(stream: &mut TcpStream) -> String {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            buffer.extend_from_slice(&chunk[..read]);
            if buffer.windows(4).any(|w| w == b"\r\n\r\n") {
                return String::from_utf8_lossy(&buffer).to_string();
            }
        }
    }

    async fn start_test_relay(upstream: UpstreamProxy) -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        spawn_relay(listener, Arc::new(upstream)).unwrap().port
    }

    async fn test_deadline<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(2), future)
            .await
            .expect("本机代理测试应及时完成")
    }

    #[tokio::test]
    async fn ipv6_upstream_preflight_uses_unbracketed_address() {
        let listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream = upstream(&format!("http://user:pass@{address}"));
        assert_eq!(upstream.host, "::1");
        assert_eq!(upstream.display_addr(), address.to_string());
        test_deadline(preflight(&upstream)).await.unwrap();
    }

    #[tokio::test]
    async fn socks_forward_encodes_ipv6_target_and_keeps_bracketed_host_header() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.read_exact(&mut [0; 4]).await.unwrap();
            stream.write_all(&[0x05, 0x00]).await.unwrap();
            let mut target = [0; 22];
            stream.read_exact(&mut target).await.unwrap();
            assert_eq!(&target[..4], &[0x05, 0x01, 0x00, 0x04]);
            assert_eq!(&target[4..20], &Ipv6Addr::LOCALHOST.octets());
            assert_eq!(&target[20..], &8080u16.to_be_bytes());
            stream
                .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            let request = read_request(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            request
        });
        let port = start_test_relay(upstream(&format!("socks5h://user:pass@{address}"))).await;
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        client
            .write_all(b"GET http://[::1]:8080/x?y=1 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        test_deadline(client.read_to_end(&mut response))
            .await
            .unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200"));
        let request = server.await.unwrap();
        assert!(request.starts_with("GET /x?y=1 HTTP/1.1\r\n"), "{request}");
        assert!(request.contains("Host: [::1]:8080\r\n"), "{request}");
    }

    #[tokio::test]
    async fn unrelated_upstreams_initialize_while_socks_preflight_waits() {
        let slow_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let slow = upstream(&format!(
            "socks5://user:pass@{}",
            slow_listener.local_addr().unwrap()
        ));
        let pending = tokio::spawn({
            let slow = slow.clone();
            async move { ensure_running(&slow).await }
        });
        let (mut connection, _) = test_deadline(slow_listener.accept()).await.unwrap();
        test_deadline(connection.read_exact(&mut [0; 4]))
            .await
            .unwrap();

        let fast_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let fast = upstream(&format!(
            "http://user:pass@{}",
            fast_listener.local_addr().unwrap()
        ));
        let port = test_deadline(ensure_running(&fast)).await.unwrap();
        assert!(port > 0);
        assert!(!pending.is_finished());
        pending.abort();
        assert!(pending.await.unwrap_err().is_cancelled());
        let mut relays = RELAYS.lock().unwrap();
        assert!(!relays.contains_key(&slow.raw));
        relays.remove(&fast.raw);
    }

    #[tokio::test]
    async fn same_upstream_concurrent_initialization_reuses_one_listener() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let upstream = upstream(&format!(
            "socks5://user:pass@{}",
            listener.local_addr().unwrap()
        ));
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.read_exact(&mut [0; 4]).await.unwrap();
            stream.write_all(&[0x05, 0x00]).await.unwrap();
            // 只处理一次探活；重复初始化将无法成功。
        });
        let (first, second) = test_deadline(async {
            tokio::join!(ensure_running(&upstream), ensure_running(&upstream))
        })
        .await;
        assert_eq!(first.unwrap(), second.unwrap());
        server.await.unwrap();
        RELAYS.lock().unwrap().remove(&upstream.raw);
    }

    #[tokio::test]
    async fn failed_preflight_is_removed_and_can_retry() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let upstream = upstream(&format!(
            "socks5://user:pass@{}",
            listener.local_addr().unwrap()
        ));
        let server = tokio::spawn(async move {
            for method in [0xff, 0x00] {
                let (mut stream, _) = listener.accept().await.unwrap();
                stream.read_exact(&mut [0; 4]).await.unwrap();
                stream.write_all(&[0x05, method]).await.unwrap();
            }
        });
        assert!(test_deadline(ensure_running(&upstream)).await.is_err());
        assert!(!RELAYS.lock().unwrap().contains_key(&upstream.raw));
        let port = test_deadline(ensure_running(&upstream)).await.unwrap();
        test_deadline(TcpStream::connect((Ipv4Addr::LOCALHOST, port)))
            .await
            .unwrap();
        server.await.unwrap();
        RELAYS.lock().unwrap().remove(&upstream.raw);
    }

    #[tokio::test]
    async fn waiting_initializer_retries_after_first_caller_is_cancelled() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let upstream = upstream(&format!(
            "socks5://user:pass@{}",
            listener.local_addr().unwrap()
        ));
        let first = tokio::spawn({
            let upstream = upstream.clone();
            async move { ensure_running(&upstream).await }
        });
        let (mut first_stream, _) = test_deadline(listener.accept()).await.unwrap();
        test_deadline(first_stream.read_exact(&mut [0; 4]))
            .await
            .unwrap();
        let second = tokio::spawn({
            let upstream = upstream.clone();
            async move { ensure_running(&upstream).await }
        });
        test_deadline(async {
            loop {
                if Arc::strong_count(RELAYS.lock().unwrap().get(&upstream.raw).unwrap()) == 3 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        assert!(RELAYS.lock().unwrap().contains_key(&upstream.raw));
        let (mut second_stream, _) = test_deadline(listener.accept()).await.unwrap();
        test_deadline(second_stream.read_exact(&mut [0; 4]))
            .await
            .unwrap();
        second_stream.write_all(&[0x05, 0x00]).await.unwrap();
        assert!(test_deadline(second).await.unwrap().unwrap() > 0);
        RELAYS.lock().unwrap().remove(&upstream.raw);
    }

    #[tokio::test]
    async fn stopped_listener_is_recreated_before_returning_cached_port() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let upstream = upstream(&format!(
            "http://user:pass@{}",
            listener.local_addr().unwrap()
        ));
        let old_port = test_deadline(ensure_running(&upstream)).await.unwrap();
        let task = RELAYS.lock().unwrap()[&upstream.raw]
            .get()
            .unwrap()
            .task
            .abort_handle();
        task.abort();
        test_deadline(async {
            while !task.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        // 占用旧端口，确保重建不会恰好获得相同端口，掩盖失效缓存。
        let _old_port_guard = TcpListener::bind((Ipv4Addr::LOCALHOST, old_port))
            .await
            .unwrap();
        let new_port = test_deadline(ensure_running(&upstream)).await.unwrap();
        assert_ne!(old_port, new_port);
        test_deadline(TcpStream::connect((Ipv4Addr::LOCALHOST, new_port)))
            .await
            .unwrap();
        RELAYS.lock().unwrap().remove(&upstream.raw);
    }

    #[tokio::test]
    async fn socks_preflight_times_out_without_leaking_credentials_or_registration() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let upstream = upstream(&format!(
            "socks5://user:secret@{}",
            listener.local_addr().unwrap()
        ));
        let pending = tokio::spawn({
            let upstream = upstream.clone();
            async move { ensure_running(&upstream).await }
        });
        let (mut connection, _) = test_deadline(listener.accept()).await.unwrap();
        test_deadline(connection.read_exact(&mut [0; 4]))
            .await
            .unwrap();
        tokio::time::pause();
        tokio::time::advance(UPSTREAM_CONNECT_TIMEOUT).await;
        let error = pending.await.unwrap().unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("探活超时"), "{error}");
        assert!(error.contains(&upstream.display_addr()), "{error}");
        assert!(!error.contains("secret"), "{error}");
        assert!(!RELAYS.lock().unwrap().contains_key(&upstream.raw));
    }

    #[tokio::test(start_paused = true)]
    async fn socks_handshake_times_out_at_each_incomplete_response_stage() {
        let upstream = upstream("socks5h://user:pass@127.0.0.1:1080");
        for response in [
            &[][..],
            &[0x05, 0x02][..],
            &[0x05, 0x02, 0x01, 0x00][..],
            &[0x05, 0x02, 0x01, 0x00, 0x05, 0x00, 0x00, 0x03][..],
            &[0x05, 0x02, 0x01, 0x00, 0x05, 0x00, 0x00, 0x03, 0xff][..],
        ] {
            let (mut stream, mut peer) = tokio::io::duplex(1024);
            peer.write_all(response).await.unwrap();
            let start = tokio::time::Instant::now();
            let error = socks5_connect(&mut stream, &upstream, "example.com", 443)
                .await
                .unwrap_err();
            assert!(error.to_string().contains("协商超时"), "{error:#}");
            assert_eq!(start.elapsed(), UPSTREAM_CONNECT_TIMEOUT);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn socks_handshake_uses_one_deadline_across_authentication_stages() {
        let upstream = upstream("socks5h://user:pass@127.0.0.1:1080");
        let (mut stream, mut peer) = tokio::io::duplex(1024);
        let server = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(6)).await;
            peer.write_all(&[0x05, 0x02]).await.unwrap();
            tokio::time::sleep(Duration::from_secs(6)).await;
            peer.write_all(&[0x01, 0x00]).await.unwrap();
        });
        let start = tokio::time::Instant::now();
        let error = socks5_connect(&mut stream, &upstream, "example.com", 443)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("协商超时"), "{error:#}");
        assert_eq!(start.elapsed(), UPSTREAM_CONNECT_TIMEOUT);
        server.abort();
    }

    #[tokio::test]
    async fn socks_rejects_unoffered_auth_and_malformed_responses() {
        for (raw, response, expected) in [
            (
                "socks5h://127.0.0.1:1080",
                &[0x05, 0x02][..],
                "未提供的认证方式",
            ),
            (
                "socks5h://user:pass@127.0.0.1:1080",
                &[0x05, 0x02, 0x05, 0x00][..],
                "认证版本",
            ),
            (
                "socks5h://127.0.0.1:1080",
                &[0x05, 0x00, 0x05, 0x00, 0x01, 0x01][..],
                "保留字段",
            ),
        ] {
            let (mut stream, mut peer) = tokio::io::duplex(1024);
            peer.write_all(response).await.unwrap();
            let error = test_deadline(socks5_connect(
                &mut stream,
                &upstream(raw),
                "example.com",
                443,
            ))
            .await
            .unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
        }
    }

    #[tokio::test]
    async fn read_head_enforces_limit_even_when_terminator_arrives_in_same_chunk() {
        let mut oversized = vec![b'x'; MAX_HEAD_BYTES];
        oversized.extend_from_slice(b"\r\n\r\n");
        let error = read_head(&mut oversized.as_slice()).await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        let mut valid = vec![b'x'; MAX_HEAD_BYTES - 4];
        valid.extend_from_slice(b"\r\n\r\npayload");
        let (read, end) = read_head(&mut valid.as_slice()).await.unwrap();
        assert_eq!(end, MAX_HEAD_BYTES);
        assert_eq!(read, valid[..read.len()]);
    }

    #[tokio::test]
    async fn connect_relays_upstream_answer_and_injects_basic_auth() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            stream
                .write_all(
                    b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n",
                )
                .await
                .unwrap();
            request
        });
        let upstream = upstream(&format!("http://user:pass@{address}"));
        assert_eq!(upstream.basic_auth_header().unwrap(), "Basic dXNlcjpwYXNz");
        let relay_port = start_test_relay(upstream).await;
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, relay_port))
            .await
            .unwrap();
        client
            .write_all(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n")
            .await
            .unwrap();
        let response = read_request(&mut client).await;
        assert!(response.starts_with("HTTP/1.1 407"), "{response}");
        let request = server.await.unwrap();
        assert!(
            request.starts_with("CONNECT example.com:443 HTTP/1.1"),
            "{request}"
        );
        assert!(
            request.contains("Proxy-Authorization: Basic dXNlcjpwYXNz"),
            "{request}"
        );
    }

    #[tokio::test]
    async fn connect_tunnel_carries_bytes_after_upstream_200() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_request(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            let mut echoed = [0u8; 4];
            stream.read_exact(&mut echoed).await.unwrap();
            stream.write_all(&echoed).await.unwrap();
        });
        let upstream = upstream(&format!("http://user:pass@{address}"));
        let relay_port = start_test_relay(upstream).await;
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, relay_port))
            .await
            .unwrap();
        client
            .write_all(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\nping")
            .await
            .unwrap();
        let response = read_request(&mut client).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let mut echoed = [0u8; 4];
        client.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"ping");
    }

    #[tokio::test]
    async fn forward_get_rewrites_connection_and_injects_auth() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .unwrap();
            request
        });
        let upstream = upstream(&format!("http://user:pass@{address}"));
        let relay_port = start_test_relay(upstream).await;
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, relay_port))
            .await
            .unwrap();
        client
            .write_all(b"GET http://example.com/x HTTP/1.1\r\nHost: example.com\r\nProxy-Connection: keep-alive\r\n\r\n")
            .await
            .unwrap();
        let mut body = Vec::new();
        client.read_to_end(&mut body).await.unwrap();
        let response = String::from_utf8_lossy(&body).to_string();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("ok"), "{response}");
        let request = server.await.unwrap();
        assert!(
            request.contains("Proxy-Authorization: Basic dXNlcjpwYXNz"),
            "{request}"
        );
        assert!(request.contains("Connection: close"), "{request}");
        assert!(!request.contains("keep-alive"), "{request}");
    }

    #[tokio::test]
    async fn socks5_upstream_runs_rfc1929_auth_and_tunnels() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 4];
            stream.read_exact(&mut greeting).await.unwrap();
            assert_eq!(&greeting[..2], &[0x05, 0x02]);
            assert_eq!(&greeting[2..], &[0x00, 0x02]);
            stream.write_all(&[0x05, 0x02]).await.unwrap();
            let mut auth_head = [0u8; 2];
            stream.read_exact(&mut auth_head).await.unwrap();
            assert_eq!(auth_head[0], 0x01);
            let mut uname = vec![0u8; auth_head[1] as usize];
            stream.read_exact(&mut uname).await.unwrap();
            let mut plen = [0u8; 1];
            stream.read_exact(&mut plen).await.unwrap();
            let mut passwd = vec![0u8; plen[0] as usize];
            stream.read_exact(&mut passwd).await.unwrap();
            assert_eq!(uname, b"user");
            assert_eq!(passwd, b"pass");
            stream.write_all(&[0x01, 0x00]).await.unwrap();
            let mut request_head = [0u8; 4];
            stream.read_exact(&mut request_head).await.unwrap();
            assert_eq!(&request_head[..3], &[0x05, 0x01, 0x00]);
            assert_eq!(request_head[3], 0x03);
            let mut dlen = [0u8; 1];
            stream.read_exact(&mut dlen).await.unwrap();
            let mut domain = vec![0u8; dlen[0] as usize];
            stream.read_exact(&mut domain).await.unwrap();
            assert_eq!(domain, b"example.com");
            let mut port = [0u8; 2];
            stream.read_exact(&mut port).await.unwrap();
            assert_eq!(u16::from_be_bytes(port), 443);
            stream
                .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            let mut echoed = [0u8; 3];
            stream.read_exact(&mut echoed).await.unwrap();
            stream.write_all(&echoed).await.unwrap();
        });
        // socks5h：目标域名原样交给上游，本地不解析。
        let upstream = upstream(&format!("socks5h://user:pass@{address}"));
        let relay_port = start_test_relay(upstream).await;
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, relay_port))
            .await
            .unwrap();
        client
            .write_all(b"CONNECT example.com:443 HTTP/1.1\r\n\r\nabc")
            .await
            .unwrap();
        let response = read_request(&mut client).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let mut echoed = [0u8; 3];
        client.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"abc");
    }

    #[tokio::test]
    async fn socks5_upstream_rejects_bad_credentials_at_preflight() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 4];
            stream.read_exact(&mut greeting).await.unwrap();
            stream.write_all(&[0x05, 0x02]).await.unwrap();
            let mut auth_head = [0u8; 2];
            stream.read_exact(&mut auth_head).await.unwrap();
            let mut uname = vec![0u8; auth_head[1] as usize];
            stream.read_exact(&mut uname).await.unwrap();
            let mut plen = [0u8; 1];
            stream.read_exact(&mut plen).await.unwrap();
            let mut passwd = vec![0u8; plen[0] as usize];
            stream.read_exact(&mut passwd).await.unwrap();
            stream.write_all(&[0x01, 0x01]).await.unwrap();
        });
        let upstream = upstream(&format!("socks5://user:wrong@{address}"));
        let error = ensure_running(&upstream).await.unwrap_err();
        assert!(format!("{error:#}").contains("认证失败"), "{error:#}");
    }
}
