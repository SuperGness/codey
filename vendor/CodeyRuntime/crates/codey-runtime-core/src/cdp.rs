use anyhow::{Context, bail};
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
    let urls = [
        format!("http://localhost:{debug_port}/json"),
        format!("http://127.0.0.1:{debug_port}/json"),
        format!("http://[::1]:{debug_port}/json"),
    ];
    let mut errors = Vec::new();
    for url in urls {
        match query_targets_url(client, &url).await {
            Ok(targets) => return Ok(targets),
            Err(error) => errors.push(format!("{url}: {error:#}")),
        }
    }

    bail!(
        "failed to query CDP targets on loopback addresses: {}",
        errors.join("; ")
    )
}

/// 浏览器端点（`/json/version`）的 WebSocket 地址：Target 域的自动挂载
/// 只能在这条会话上做，page 级会话拿不到 Target 协议。
pub async fn browser_websocket_url(debug_port: u16) -> anyhow::Result<String> {
    let client = cdp_http_client()?;
    // 与 list_targets 相同的回环地址顺序：Chrome 150+ 拒绝 IP 字面量 Host。
    let urls = [
        format!("http://localhost:{debug_port}/json/version"),
        format!("http://127.0.0.1:{debug_port}/json/version"),
        format!("http://[::1]:{debug_port}/json/version"),
    ];
    let mut errors = Vec::new();
    for url in urls {
        match client.get(&url).send().await {
            Ok(response) => match response.error_for_status() {
                Ok(response) => match response.json::<serde_json::Value>().await {
                    Ok(payload) => {
                        if let Some(ws_url) = payload
                            .get("webSocketDebuggerUrl")
                            .and_then(serde_json::Value::as_str)
                            .filter(|url| !url.is_empty())
                        {
                            return Ok(ws_url.to_string());
                        }
                        errors.push(format!("{url}: 响应缺少 webSocketDebuggerUrl"));
                    }
                    Err(error) => errors.push(format!("{url}: {error:#}")),
                },
                Err(error) => errors.push(format!("{url}: {error:#}")),
            },
            Err(error) => errors.push(format!("{url}: {error:#}")),
        }
    }
    anyhow::bail!(
        "failed to query CDP browser endpoint on loopback addresses: {}",
        errors.join("; ")
    )
}

fn cdp_http_client() -> anyhow::Result<&'static reqwest::Client> {
    if let Some(client) = CDP_HTTP_CLIENT.get() {
        return Ok(client);
    }
    let client = reqwest::Client::builder()
        .no_proxy()
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

    #[test]
    fn cdp_http_client_is_reused() {
        let first = cdp_http_client().unwrap();
        let second = cdp_http_client().unwrap();

        assert!(std::ptr::eq(first, second));
    }
}
