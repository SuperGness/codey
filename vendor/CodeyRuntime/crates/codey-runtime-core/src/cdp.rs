use anyhow::{Context, bail};
use futures_util::{StreamExt, stream::FuturesUnordered};
use serde::Deserialize;
use std::sync::OnceLock;
use std::time::Duration;

const CDP_HTTP_TIMEOUT: Duration = Duration::from_secs(3);
static CDP_HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct CdpTarget {
    pub id: String,
    #[serde(rename = "type")]
    pub target_type: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default, rename = "webSocketDebuggerUrl")]
    pub web_socket_debugger_url: Option<String>,
}

pub async fn list_targets(debug_port: u16) -> anyhow::Result<Vec<CdpTarget>> {
    let client = cdp_http_client()?;
    // Chrome 150+ rejects DevTools HTTP when the Host header is an IP literal
    // and answers the same listener for `localhost`. Older builds and an
    // IPv6-only socket still need the explicit loopback URLs.
    let urls = loopback_urls(debug_port, "/json");
    query_targets_urls(client, &urls).await
}

async fn query_targets_urls(
    client: &reqwest::Client,
    urls: &[String],
) -> anyhow::Result<Vec<CdpTarget>> {
    first_loopback_success(urls, |url| query_targets_url(client, url))
        .await
        .map_err(|errors| {
            anyhow::anyhow!(
                "failed to query CDP targets on loopback addresses: {}",
                errors.join("; ")
            )
        })
}

/// 浏览器端点（`/json/version`）的 WebSocket 地址：Target 域的自动挂载
/// 只能在这条会话上做，page 级会话拿不到 Target 协议。
pub async fn browser_websocket_url(debug_port: u16) -> anyhow::Result<String> {
    let client = cdp_http_client()?;
    // 与 list_targets 相同的回环地址：Chrome 150+ 拒绝 IP 字面量 Host。
    let urls = loopback_urls(debug_port, "/json/version");
    first_loopback_success(&urls, |url| query_browser_websocket_url(client, url))
        .await
        .map_err(|errors| {
            anyhow::anyhow!(
                "failed to query CDP browser endpoint on loopback addresses: {}",
                errors.join("; ")
            )
        })
}

fn loopback_urls(debug_port: u16, path: &str) -> [String; 3] {
    [
        format!("http://localhost:{debug_port}{path}"),
        format!("http://127.0.0.1:{debug_port}{path}"),
        format!("http://[::1]:{debug_port}{path}"),
    ]
}

/// 同时探测全部回环地址，取最先成功的结果。Windows 上连接未监听的回环端口
/// 会重试 SYN、约 2 秒才失败，`localhost` 又优先解析到 DevTools 不监听的
/// `::1`；逐个尝试会让一个慢失败的地址拖住可用地址。
async fn first_loopback_success<'a, T, F, Fut>(
    urls: &'a [String],
    probe: F,
) -> Result<T, Vec<String>>
where
    F: Fn(&'a str) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let mut probes: FuturesUnordered<_> = urls
        .iter()
        .map(|url| {
            let probe = probe(url);
            async move { (url, probe.await) }
        })
        .collect();
    let mut errors = Vec::new();
    while let Some((url, result)) = probes.next().await {
        match result {
            Ok(value) => return Ok(value),
            Err(error) => errors.push(format!("{url}: {error:#}")),
        }
    }
    Err(errors)
}

async fn query_browser_websocket_url(
    client: &reqwest::Client,
    url: &str,
) -> anyhow::Result<String> {
    let payload = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    payload
        .get("webSocketDebuggerUrl")
        .and_then(serde_json::Value::as_str)
        .filter(|url| !url.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("响应缺少 webSocketDebuggerUrl"))
}

fn cdp_http_client() -> anyhow::Result<&'static reqwest::Client> {
    if let Some(client) = CDP_HTTP_CLIENT.get() {
        return Ok(client);
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        // CDP 发现只访问回环 HTTP，避免在异步线程上读取系统 TLS 证书。
        .tls_built_in_root_certs(false)
        .timeout(CDP_HTTP_TIMEOUT)
        .build()
        .context("failed to build CDP HTTP client")?;
    let _ = CDP_HTTP_CLIENT.set(client);
    Ok(CDP_HTTP_CLIENT
        .get()
        .expect("CDP HTTP client must be initialized"))
}

async fn query_targets_url(client: &reqwest::Client, url: &str) -> anyhow::Result<Vec<CdpTarget>> {
    let response = client
        .get(url)
        .send()
        .await
        .context("failed to query CDP targets")?
        .error_for_status()
        .context("CDP target query failed")?;

    response
        .json::<Vec<CdpTarget>>()
        .await
        .context("failed to deserialize CDP targets")
}

/// Codex 在主窗口之外还会创建隐藏的预热窗口（分离会话、快捷窗口、Spotlight、
/// 宠物浮层）。它们与主窗口同源、标题也可能是应用名，`/json` 又按最近活动
/// 排序，新建的预热窗口常排在主窗口前面；注入到这些窗口里，主窗口就拿不到
/// 桥接，请求入口也永远找不到会话管理器。
const AUXILIARY_WINDOW_ROUTES: [&str; 4] = [
    "/avatar-overlay",
    "/detached-window",
    "/hotkey-window",
    "/spotlight",
];

/// 按主窗口优先选择注入目标：未带路由参数的主文档优先，其余候选保持 `/json`
/// 原有顺序。
pub fn pick_injectable_codex_page_target(targets: &[CdpTarget]) -> anyhow::Result<CdpTarget> {
    let mut best: Option<(u8, &CdpTarget)> = None;
    for target in targets
        .iter()
        .filter(|target| is_injectable_page_target(target) && is_primary_codex_page_target(target))
    {
        let rank = primary_page_rank(&target.url);
        if best.is_none_or(|(best_rank, _)| rank > best_rank) {
            best = Some((rank, target));
        }
    }
    match best {
        Some((_, target)) => Ok(target.clone()),
        None => bail!("No injectable Codex page target found"),
    }
}

fn primary_page_rank(url: &str) -> u8 {
    let (path, query) = split_app_url(url);
    let main_document = path.eq_ignore_ascii_case("app://-/index.html");
    match (main_document, query_param(query, "initialRoute")) {
        (true, None) => 2,
        (true, Some(route)) if route.is_empty() || route == "/" => 2,
        _ => 1,
    }
}

pub fn is_injectable_page_target(target: &CdpTarget) -> bool {
    target.target_type == "page"
        && target
            .web_socket_debugger_url
            .as_deref()
            .is_some_and(|url| !url.is_empty())
}

pub fn is_codex_page_target(target: &CdpTarget) -> bool {
    if target.target_type != "page"
        || target
            .url
            .trim()
            .to_ascii_lowercase()
            .starts_with("data:text/html")
    {
        return false;
    }
    let haystack = format!("{} {}", target.title, target.url).to_lowercase();
    haystack.contains("codex")
        || is_chatgpt_desktop_page(&target.title, &target.url)
        || is_packaged_codex_page(&target.url)
}

pub fn is_primary_codex_page_target(target: &CdpTarget) -> bool {
    is_codex_page_target(target) && !is_auxiliary_codex_window_url(&target.url)
}

pub fn is_avatar_overlay_page_target(target: &CdpTarget) -> bool {
    is_injectable_page_target(target)
        && auxiliary_codex_window_route(&target.url) == Some("/avatar-overlay")
}

/// 页面地址是否属于 Codex 的辅助窗口。也用于注入后按页面实际地址复核目标。
pub fn is_auxiliary_codex_window_url(url: &str) -> bool {
    auxiliary_codex_window_route(url).is_some()
}

/// 辅助窗口既可能以 `index.html?initialRoute=/x` 打开，也可能使用独立文档
/// （`detached-window.html`）或直接以路由为路径。
fn auxiliary_codex_window_route(url: &str) -> Option<&'static str> {
    let (path, query) = split_app_url(url);
    let path = path.to_ascii_lowercase();
    let document = path.strip_prefix("app://-/")?;
    let document = document.strip_suffix(".html").unwrap_or(document);
    let route = query_param(query, "initialRoute").map(|route| route.to_ascii_lowercase());
    AUXILIARY_WINDOW_ROUTES
        .into_iter()
        .find(|candidate| route.as_deref() == Some(*candidate) || document == &candidate[1..])
}

/// 拆出不含片段的地址主体与查询串。
fn split_app_url(url: &str) -> (&str, &str) {
    let url = url.trim();
    let url = url.split_once('#').map_or(url, |(head, _)| head);
    url.split_once('?').unwrap_or((url, ""))
}

fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        key.eq_ignore_ascii_case(name)
            .then(|| percent_decode(value))
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escaped = (bytes[index] == b'%')
            .then(|| bytes.get(index + 1..index + 3))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (escaped, bytes[index]) {
            (Some(byte), _) => {
                decoded.push(byte);
                index += 3;
            }
            (None, b'+') => {
                decoded.push(b' ');
                index += 1;
            }
            (None, byte) => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn is_chatgpt_desktop_page(title: &str, url: &str) -> bool {
    let title = title.trim().to_ascii_lowercase();
    let url = url.trim().to_ascii_lowercase();
    title == "chatgpt"
        && (url == "https://chatgpt.com"
            || url.starts_with("https://chatgpt.com/")
            || url == "https://chat.openai.com"
            || url.starts_with("https://chat.openai.com/"))
}

fn is_packaged_codex_page(url: &str) -> bool {
    let url = url.trim().to_ascii_lowercase();
    url == "app://-/index.html"
        || url.starts_with("app://-/index.html?")
        || url.starts_with("app://-/index.html#")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    const TARGETS_RESPONSE: &str = "HTTP/1.1 200 OK\r\nContent-Length: 31\r\nConnection: close\r\n\r\n[{\"id\":\"page-1\",\"type\":\"page\"}]";
    const REJECTED_RESPONSE: &str =
        "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    /// 回环端点：`Some` 读完请求后应答一次；`None` 接受连接后一直不应答。
    fn loopback_endpoint(response: Option<&'static str>, path: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}{path}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request);
            match response {
                Some(response) => {
                    let _ = stream.write_all(response.as_bytes());
                }
                None => std::thread::sleep(Duration::from_secs(30)),
            }
        });
        url
    }

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .timeout(CDP_HTTP_TIMEOUT)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn target_discovery_does_not_wait_for_a_stalled_loopback_address() {
        let client = test_client();
        for stalled_first in [true, false] {
            let stalled = loopback_endpoint(None, "/json");
            let ready = loopback_endpoint(Some(TARGETS_RESPONSE), "/json");
            let urls = if stalled_first {
                [stalled, ready]
            } else {
                [ready, stalled]
            };
            let targets =
                tokio::time::timeout(Duration::from_secs(1), query_targets_urls(&client, &urls))
                    .await
                    .expect("可用地址不得等待卡住的地址")
                    .unwrap();
            assert_eq!(targets[0].id, "page-1");
        }
    }

    #[tokio::test]
    async fn target_discovery_keeps_success_after_an_address_rejects_the_request() {
        let client = test_client();
        let urls = [
            loopback_endpoint(Some(REJECTED_RESPONSE), "/json"),
            loopback_endpoint(Some(TARGETS_RESPONSE), "/json"),
        ];
        let targets = query_targets_urls(&client, &urls).await.unwrap();
        assert_eq!(targets[0].id, "page-1");
    }

    #[tokio::test]
    async fn target_discovery_reports_all_failed_addresses() {
        let client = test_client();
        let invalid = loopback_endpoint(
            Some("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"),
            "/json",
        );
        let rejected = loopback_endpoint(Some(REJECTED_RESPONSE), "/json");
        let error = query_targets_urls(&client, &[invalid.clone(), rejected.clone()])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(&invalid));
        assert!(error.contains(&rejected));
        assert!(error.contains("failed to deserialize CDP targets"));
        assert!(error.contains("CDP target query failed"));
    }

    #[tokio::test]
    async fn browser_endpoint_discovery_skips_addresses_without_a_websocket_url() {
        let client = test_client();
        let body = r#"{"webSocketDebuggerUrl":"ws://127.0.0.1:9/devtools/browser/x"}"#;
        let ready: &'static str = Box::leak(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .into_boxed_str(),
        );
        let urls = [
            loopback_endpoint(None, "/json/version"),
            loopback_endpoint(
                Some("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"),
                "/json/version",
            ),
            loopback_endpoint(Some(ready), "/json/version"),
        ];
        let url = tokio::time::timeout(
            Duration::from_secs(1),
            first_loopback_success(&urls, |url| query_browser_websocket_url(&client, url)),
        )
        .await
        .expect("可用地址不得等待卡住的地址")
        .unwrap();
        assert_eq!(url, "ws://127.0.0.1:9/devtools/browser/x");
    }

    #[test]
    fn cdp_http_client_is_reused() {
        let first = cdp_http_client().unwrap();
        let second = cdp_http_client().unwrap();

        assert!(std::ptr::eq(first, second));
    }
}
