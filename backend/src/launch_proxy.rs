//! Codex 启动代理解析。
//!
//! 配置的代理地址被翻译成两份产物：客户端的 Chromium `--proxy-server`
//! 启动参数，以及注入启动环境、由 app-server 与其子进程继承的代理变量。
//! Chromium 命令行代理不认识 `socks5h`（其 socks5 本身就走远程 DNS），
//! 因此参数侧统一转写；环境变量保留地址的原始 scheme，由 reqwest 等客户
//! 端按自身规则理解。
//!
//! 带用户名密码的地址无法直接交给 Chromium 命令行，统一由
//! `launch_proxy_relay` 在回环地址起本地认证中继：客户端与子进程都使用
//! 无凭据的 `http://127.0.0.1:<port>`，凭据只在中继侧注入上游，也不再
//! 随环境变量泄漏给 Codex 的子进程。

/// 保存时经 `config::validate_codex_launch_proxy` 校验；手工编辑出的无效
/// 值在启动解析处仍会失败，不会带着半个代理配置拉起 Codex。
#[derive(Clone, Debug)]
pub(crate) struct CodexLaunchProxy {
    /// 写入代理环境变量的地址；带凭据的上游已被中继改写为回环地址。
    url: String,
    /// Chromium `--proxy-server=` 的值；socks5h 已转写为 socks5。
    chromium_server: String,
}

/// `resolve` 的产物：无凭据地址直连上游，带凭据地址经本地认证中继。
pub(crate) enum LaunchProxyPlan {
    /// 直连上游，`upstream` 仅用于启动前的可达性探活。
    Direct {
        proxy: CodexLaunchProxy,
        upstream: crate::launch_proxy_relay::UpstreamProxy,
    },
    Relayed(crate::launch_proxy_relay::UpstreamProxy),
}

impl LaunchProxyPlan {
    /// 启动入口：先对上游做探活——代理不可达时立刻报错，而不是让 Codex
    /// 带着死代理静默挂起；带凭据的上游再启动回环认证中继，Codex 侧拿到
    /// 的始终是无凭据地址。
    pub(crate) async fn start(self) -> anyhow::Result<CodexLaunchProxy> {
        match self {
            Self::Direct { proxy, upstream } => {
                crate::launch_proxy_relay::preflight(&upstream).await?;
                Ok(proxy)
            }
            Self::Relayed(upstream) => {
                let port = crate::launch_proxy_relay::ensure_running(&upstream).await?;
                Ok(CodexLaunchProxy::relayed(port))
            }
        }
    }
}

impl CodexLaunchProxy {
    pub(crate) fn resolve(value: &str) -> anyhow::Result<Option<LaunchProxyPlan>> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(None);
        }
        crate::config::validate_codex_launch_proxy(value).map_err(anyhow::Error::msg)?;
        let url = reqwest::Url::parse(value).expect("启动代理地址已通过校验");
        let upstream = crate::launch_proxy_relay::UpstreamProxy::parse(&url, value)
            .expect("启动代理地址已通过校验");
        if upstream.has_credentials() {
            return Ok(Some(LaunchProxyPlan::Relayed(upstream)));
        }
        Ok(Some(LaunchProxyPlan::Direct {
            proxy: Self::from_parsed(value, &url),
            upstream,
        }))
    }

    fn from_parsed(raw: &str, url: &reqwest::Url) -> Self {
        let scheme = if url.scheme() == "socks5h" {
            "socks5"
        } else {
            url.scheme()
        };
        let host = url.host_str().expect("启动代理地址已通过校验");
        let port = url
            .port_or_known_default()
            .expect("启动代理协议具有默认端口");
        Self {
            url: raw.to_string(),
            chromium_server: format!("{scheme}://{host}:{port}"),
        }
    }

    /// 认证中继监听在回环地址，客户端与 app-server 统一用无凭据的 http
    /// 代理地址访问它。
    fn relayed(port: u16) -> Self {
        let url = format!("http://127.0.0.1:{port}");
        Self {
            url: url.clone(),
            chromium_server: url,
        }
    }

    /// 追加给 Codex 客户端的 Chromium 启动参数。
    pub(crate) fn chromium_argument(&self) -> String {
        format!("--proxy-server={}", self.chromium_server)
    }

    /// 注入 Codex 启动环境的代理变量。
    pub(crate) fn environment(&self) -> Vec<(String, String)> {
        proxy_environment(&self.url)
    }
}

/// `NO_PROXY` 保留已有绕过规则并合并回环地址，保证本地路由、CDP 与 CLI
/// 握手始终直连。
fn proxy_environment(url: &str) -> Vec<(String, String)> {
    let mut environment = vec![
        ("HTTP_PROXY".to_string(), url.to_string()),
        ("HTTPS_PROXY".to_string(), url.to_string()),
        ("ALL_PROXY".to_string(), url.to_string()),
    ];
    #[cfg(windows)]
    // Windows 环境变量名不分大小写，只写一份规范键。
    environment.push((
        "NO_PROXY".to_string(),
        merge_loopback_no_proxy(
            std::env::var("NO_PROXY")
                .ok()
                .or_else(|| std::env::var("no_proxy").ok())
                .as_deref(),
        ),
    ));
    #[cfg(unix)]
    {
        let uppercase = std::env::var("NO_PROXY").ok();
        let lowercase = std::env::var("no_proxy").ok();
        environment.extend([
            ("http_proxy".to_string(), url.to_string()),
            ("https_proxy".to_string(), url.to_string()),
            ("all_proxy".to_string(), url.to_string()),
            (
                "NO_PROXY".to_string(),
                merge_loopback_no_proxy(uppercase.as_deref()),
            ),
            (
                "no_proxy".to_string(),
                merge_loopback_no_proxy(lowercase.as_deref().or(uppercase.as_deref())),
            ),
        ]);
    }
    environment
}

/// 保留已有绕过规则并追加回环地址。本地路由、CDP 和 CLI 握手终结在
/// Codey 的回环监听上，这一跳必须留在代理之外。
pub(crate) fn merge_loopback_no_proxy(existing: Option<&str>) -> String {
    const LOOPBACK: [&str; 3] = ["127.0.0.1", "localhost", "::1"];
    let mut entries = existing
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for required in LOOPBACK {
        if !entries.iter().any(|entry| {
            entry
                .trim_matches(['[', ']'])
                .eq_ignore_ascii_case(required)
        }) {
            entries.push(required.to_string());
        }
    }
    entries.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve_direct(value: &str) -> CodexLaunchProxy {
        match CodexLaunchProxy::resolve(value).unwrap().unwrap() {
            LaunchProxyPlan::Direct { proxy, .. } => proxy,
            LaunchProxyPlan::Relayed(_) => panic!("{value} 应直连上游"),
        }
    }

    #[test]
    fn resolve_rejects_invalid_addresses() {
        assert!(CodexLaunchProxy::resolve("").unwrap().is_none());
        assert!(CodexLaunchProxy::resolve("  ").unwrap().is_none());
        for rejected in [
            "not-a-url",
            "ftp://127.0.0.1:21",
            "http://127.0.0.1:7890/path",
            "http://127.0.0.1:7890?x=1",
            "socks5://127.0.0.1",
            "http://0.0.0.0:7890",
        ] {
            assert!(CodexLaunchProxy::resolve(rejected).is_err(), "{rejected}");
        }
    }

    #[test]
    fn credentialed_addresses_resolve_to_the_auth_relay_plan() {
        for relayed in [
            "http://user:pass@proxy.example.com:8080",
            "https://user:pass@proxy.example.com:8443",
            "socks5://user@127.0.0.1:1080",
            "socks5h://user:pass@proxy.example.com:1080",
        ] {
            match CodexLaunchProxy::resolve(relayed).unwrap().unwrap() {
                LaunchProxyPlan::Relayed(upstream) => {
                    assert!(upstream.has_credentials(), "{relayed}");
                }
                LaunchProxyPlan::Direct { .. } => panic!("{relayed} 应走认证中继"),
            }
        }
    }

    #[test]
    fn chromium_argument_rewrites_socks5h_and_keeps_explicit_ports() {
        for (input, expected) in [
            (
                "http://127.0.0.1:7890",
                "--proxy-server=http://127.0.0.1:7890",
            ),
            (
                "socks5h://127.0.0.1:1080",
                "--proxy-server=socks5://127.0.0.1:1080",
            ),
            (
                "https://proxy.example.com",
                "--proxy-server=https://proxy.example.com:443",
            ),
            ("http://[::1]:7890", "--proxy-server=http://[::1]:7890"),
        ] {
            assert_eq!(
                resolve_direct(input).chromium_argument(),
                expected,
                "{input}"
            );
        }
    }

    #[test]
    fn relayed_proxy_uses_loopback_address_everywhere() {
        let proxy = CodexLaunchProxy::relayed(4321);
        assert_eq!(
            proxy.chromium_argument(),
            "--proxy-server=http://127.0.0.1:4321"
        );
        let environment = proxy.environment();
        assert!(
            environment
                .iter()
                .all(|(key, value)| !key.to_lowercase().ends_with("_proxy")
                    || key.eq_ignore_ascii_case("no_proxy")
                    || value == "http://127.0.0.1:4321"),
            "代理变量应全部指向回环中继：{environment:?}"
        );
    }

    #[test]
    fn environment_covers_http_schemes_and_bypasses_loopback() {
        let proxy = resolve_direct("socks5h://127.0.0.1:1080");
        let environment = proxy.environment();
        let get = |name: &str| {
            environment
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(get("HTTP_PROXY"), Some("socks5h://127.0.0.1:1080"));
        assert_eq!(get("HTTPS_PROXY"), Some("socks5h://127.0.0.1:1080"));
        assert_eq!(get("ALL_PROXY"), Some("socks5h://127.0.0.1:1080"));
        #[cfg(windows)]
        {
            assert!(get("no_proxy").is_none());
            assert!(get("http_proxy").is_none());
        }
        #[cfg(unix)]
        {
            assert_eq!(get("http_proxy"), Some("socks5h://127.0.0.1:1080"));
            assert_eq!(get("no_proxy"), get("NO_PROXY"));
        }
        let bypass = get("NO_PROXY").unwrap();
        for loopback in ["127.0.0.1", "localhost", "::1"] {
            assert!(bypass.split(',').any(|entry| entry == loopback));
        }
    }

    #[test]
    fn loopback_bypass_merges_without_duplicating_entries() {
        assert_eq!(
            merge_loopback_no_proxy(Some("corp.internal, 127.0.0.1")),
            "corp.internal,127.0.0.1,localhost,::1"
        );
        assert_eq!(
            merge_loopback_no_proxy(Some("[::1],localhost")),
            "[::1],localhost,127.0.0.1"
        );
        assert_eq!(merge_loopback_no_proxy(None), "127.0.0.1,localhost,::1");
    }
}
