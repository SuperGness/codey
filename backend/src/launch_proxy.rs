//! Codex 启动代理解析。
//!
//! 配置的代理地址被翻译成两份产物：客户端的 Chromium `--proxy-server`
//! 启动参数，以及注入启动环境、由 app-server 与其子进程继承的代理变量。
//! Chromium 命令行代理不支持认证，也不认识 `socks5h`（其 socks5 本身
//! 就走远程 DNS），因此参数侧统一转写；环境变量保留用户填写的原始
//! scheme，由 reqwest 等客户端按自身规则理解。

use std::sync::RwLock;

/// 保存时经 `config::validate_codex_launch_proxy` 校验；手工编辑出的无效
/// 值在启动解析处仍会失败，不会带着半个代理配置拉起 Codex。
#[derive(Clone, Debug)]
pub(crate) struct CodexLaunchProxy {
    /// 用户填写的原始代理地址，写入代理环境变量。
    url: String,
    /// Chromium `--proxy-server=` 的值；socks5h 已转写为 socks5。
    chromium_server: String,
}

/// 本次 Codey 启动的运行时所使用的代理。Codey 自己拉起的共享 app-server
/// 也继承这份环境；配置保存后仍要到下次启动才刷新。
static ACTIVE: RwLock<Option<CodexLaunchProxy>> = RwLock::new(None);

impl CodexLaunchProxy {
    pub(crate) fn resolve(value: &str) -> anyhow::Result<Option<Self>> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(None);
        }
        crate::config::validate_codex_launch_proxy(value).map_err(anyhow::Error::msg)?;
        let url = reqwest::Url::parse(value).expect("启动代理地址已通过校验");
        let scheme = if url.scheme() == "socks5h" {
            "socks5"
        } else {
            url.scheme()
        };
        let host = url.host_str().expect("启动代理地址已通过校验");
        let host = if host.parse::<std::net::Ipv6Addr>().is_ok() {
            format!("[{host}]")
        } else {
            host.to_string()
        };
        let port = url
            .port_or_known_default()
            .expect("启动代理协议具有默认端口");
        Ok(Some(Self {
            url: value.to_string(),
            chromium_server: format!("{scheme}://{host}:{port}"),
        }))
    }

    /// 追加给 Codex 客户端的 Chromium 启动参数。
    pub(crate) fn chromium_argument(&self) -> String {
        format!("--proxy-server={}", self.chromium_server)
    }

    /// 注入 Codex 启动环境的代理变量。`NO_PROXY` 保留已有绕过规则并合并
    /// 回环地址，保证本地路由、CDP 与 CLI 握手始终直连。
    pub(crate) fn environment(&self) -> Vec<(String, String)> {
        let mut environment = vec![
            ("HTTP_PROXY".to_string(), self.url.clone()),
            ("HTTPS_PROXY".to_string(), self.url.clone()),
            ("ALL_PROXY".to_string(), self.url.clone()),
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
                ("http_proxy".to_string(), self.url.clone()),
                ("https_proxy".to_string(), self.url.clone()),
                ("all_proxy".to_string(), self.url.clone()),
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
}

/// 记录本次 Codey 启动的运行时代理；只对 Codey 拉起的进程生效。
pub(crate) fn set_active(proxy: Option<CodexLaunchProxy>) {
    *ACTIVE
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = proxy;
}

/// Codey 自己拉起的共享 app-server 继承同一份启动代理环境。共享实例
/// 按需创建，可能早于本次 Codex 启动，因此活动值为空时回读已保存的配置。
pub(crate) fn shared_app_server_environment() -> Vec<(String, String)> {
    resolve_shared_environment(|| {
        crate::config::ConfigStore::default()
            .load()
            .map(|config| config.codex_launch_proxy)
            .unwrap_or_default()
    })
}

fn resolve_shared_environment(saved: impl FnOnce() -> String) -> Vec<(String, String)> {
    if let Some(proxy) = ACTIVE
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return proxy.environment();
    }
    CodexLaunchProxy::resolve(&saved())
        .ok()
        .flatten()
        .map(|proxy| proxy.environment())
        .unwrap_or_default()
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

    #[test]
    fn resolve_rejects_invalid_or_credential_bearing_addresses() {
        assert!(CodexLaunchProxy::resolve("").unwrap().is_none());
        assert!(CodexLaunchProxy::resolve("  ").unwrap().is_none());
        for rejected in [
            "not-a-url",
            "ftp://127.0.0.1:21",
            "http://user:pass@127.0.0.1:7890",
            "http://127.0.0.1:7890/path",
            "http://127.0.0.1:7890?x=1",
            "socks5://127.0.0.1",
            "http://0.0.0.0:7890",
        ] {
            assert!(CodexLaunchProxy::resolve(rejected).is_err(), "{rejected}");
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
            let proxy = CodexLaunchProxy::resolve(input).unwrap().unwrap();
            assert_eq!(proxy.chromium_argument(), expected, "{input}");
        }
    }

    #[test]
    fn environment_covers_http_schemes_and_bypasses_loopback() {
        let proxy = CodexLaunchProxy::resolve("socks5h://127.0.0.1:1080")
            .unwrap()
            .unwrap();
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

    #[test]
    fn shared_app_server_environment_follows_active_then_saved_proxy() {
        let saved = || "http://127.0.0.1:7890".to_string();
        set_active(None);
        assert!(resolve_shared_environment(String::new).is_empty());
        // 无活动值（例如共享实例先于 Codex 启动创建）回读已保存配置。
        let environment = resolve_shared_environment(saved);
        assert!(
            environment
                .iter()
                .any(|(key, value)| key == "HTTPS_PROXY" && value == "http://127.0.0.1:7890")
        );
        // 活动值优先：本次启动已解析的代理覆盖磁盘上的旧配置。
        set_active(CodexLaunchProxy::resolve("socks5://127.0.0.1:1080").unwrap());
        let environment = resolve_shared_environment(saved);
        assert!(
            environment
                .iter()
                .any(|(key, value)| key == "HTTPS_PROXY" && value == "socks5://127.0.0.1:1080")
        );
        set_active(None);
    }
}
