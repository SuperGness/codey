use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use hyper_util::client::proxy::matcher::Matcher as SystemProxyMatcher;
use tokio_tungstenite::tungstenite::http::Uri;

/// 系统代理的复查间隔。读取一次约 0.15 ms，只在间隔到期后的首个请求上做。
const SYSTEM_PROXY_RECHECK_INTERVAL: Duration = Duration::from_secs(10);

/// 跟随系统代理变化的长期 HTTP 客户端。
///
/// reqwest 只在构建客户端时读取一次系统代理，运行期间开关代理软件后旧客户端
/// 仍按旧设置连接。这里按间隔复查请求地址的代理判定，变化时在阻塞线程池重建
/// 客户端；新客户端仍由 reqwest 自行读取系统代理及其认证信息。
pub(crate) struct SystemProxyClient {
    state: Mutex<SystemProxyClientState>,
    builder: fn() -> reqwest::ClientBuilder,
    read_system: fn() -> SystemProxyMatcher,
    recheck_interval: Duration,
}

struct SystemProxyClientState {
    client: reqwest::Client,
    /// 当前客户端构建时的系统代理；只在重建时更新，保证每次都和客户端实际使用的设置比较。
    built_with: Arc<SystemProxyMatcher>,
    next_check: Instant,
    #[cfg(test)]
    rebuilds: usize,
}

impl SystemProxyClient {
    /// 同步构建：会加载系统证书与代理配置，调用方需在可阻塞的线程上执行。
    pub(crate) fn new_blocking(builder: fn() -> reqwest::ClientBuilder) -> Result<Self> {
        Self::with_source(
            builder,
            SystemProxyMatcher::from_system,
            SYSTEM_PROXY_RECHECK_INTERVAL,
        )
    }

    /// 在阻塞线程池构建，不占用异步工作线程。
    pub(crate) async fn new(builder: fn() -> reqwest::ClientBuilder) -> Result<Self> {
        tokio::task::spawn_blocking(move || Self::new_blocking(builder))
            .await
            .context("创建 HTTP 客户端任务异常退出")?
    }

    fn with_source(
        builder: fn() -> reqwest::ClientBuilder,
        read_system: fn() -> SystemProxyMatcher,
        recheck_interval: Duration,
    ) -> Result<Self> {
        let built_with = read_system();
        let client = builder().build().context("创建 HTTP 客户端失败")?;
        Ok(Self {
            state: Mutex::new(SystemProxyClientState {
                client,
                built_with: Arc::new(built_with),
                next_check: Instant::now() + recheck_interval,
                #[cfg(test)]
                rebuilds: 0,
            }),
            builder,
            read_system,
            recheck_interval,
        })
    }

    /// 测试用：包住给定客户端且永不复查系统代理。
    #[cfg(test)]
    pub(crate) fn fixed(client: reqwest::Client) -> Self {
        let never = Duration::from_secs(365 * 24 * 60 * 60);
        Self {
            state: Mutex::new(SystemProxyClientState {
                client,
                built_with: Arc::new(SystemProxyMatcher::builder().build()),
                next_check: Instant::now() + never,
                rebuilds: 0,
            }),
            builder: reqwest::Client::builder,
            read_system: || SystemProxyMatcher::builder().build(),
            recheck_interval: never,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SystemProxyClientState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 不复查系统代理，直接返回当前客户端。
    pub(crate) fn cached(&self) -> reqwest::Client {
        self.lock().client.clone()
    }

    /// 返回访问 `url` 应使用的客户端；复查间隔到期时先确认系统代理是否变化。
    pub(crate) async fn current(&self, url: &str) -> reqwest::Client {
        let Ok(uri) = url.parse::<Uri>() else {
            return self.cached();
        };
        let (client, built_with) = {
            let mut state = self.lock();
            let now = Instant::now();
            if now < state.next_check {
                return state.client.clone();
            }
            // 先占住本轮复查：并发请求继续用现有客户端；本次请求被取消时，
            // 下一轮到期后仍会复查。
            state.next_check = now + self.recheck_interval;
            (state.client.clone(), Arc::clone(&state.built_with))
        };
        let (builder, read_system) = (self.builder, self.read_system);
        let rebuilt = tokio::task::spawn_blocking(move || {
            let latest = read_system();
            if same_system_proxy(&built_with, &latest, &uri) {
                return None;
            }
            // 构建失败时保留旧客户端；基准不变，下一轮会再次发现变化并重试。
            builder()
                .build()
                .ok()
                .map(|client| (client, Arc::new(latest)))
        })
        .await
        .ok()
        .flatten();
        let Some((rebuilt, latest)) = rebuilt else {
            return client;
        };
        let mut state = self.lock();
        state.client = rebuilt.clone();
        state.built_with = latest;
        #[cfg(test)]
        {
            state.rebuilds += 1;
        }
        rebuilt
    }
}

fn same_system_proxy(left: &SystemProxyMatcher, right: &SystemProxyMatcher, uri: &Uri) -> bool {
    match (left.intercept(uri), right.intercept(uri)) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.uri() == right.uri()
                && left.basic_auth() == right.basic_auth()
                && left.raw_auth() == right.raw_auth()
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU8, Ordering};

    fn matcher_for(state: &AtomicU8) -> SystemProxyMatcher {
        match state.load(Ordering::SeqCst) {
            0 => SystemProxyMatcher::builder().build(),
            1 => SystemProxyMatcher::builder()
                .https("http://127.0.0.1:7890")
                .build(),
            _ => SystemProxyMatcher::builder()
                .https("http://user:secret@127.0.0.1:7890")
                .build(),
        }
    }

    // 每个测试独占一份假系统代理，并行运行互不影响。
    static CHANGES: AtomicU8 = AtomicU8::new(0);
    fn changes_system() -> SystemProxyMatcher {
        matcher_for(&CHANGES)
    }

    static INTERVAL: AtomicU8 = AtomicU8::new(0);
    fn interval_system() -> SystemProxyMatcher {
        matcher_for(&INTERVAL)
    }

    fn rebuilds(client: &SystemProxyClient) -> usize {
        client.lock().rebuilds
    }

    #[tokio::test]
    async fn rebuilds_only_when_the_request_target_proxy_changes() {
        let client = SystemProxyClient::with_source(
            reqwest::Client::builder,
            changes_system,
            Duration::ZERO,
        )
        .unwrap();
        let url = "https://api.example.com/v1/responses";

        client.current(url).await;
        assert_eq!(
            rebuilds(&client),
            0,
            "unchanged proxy keeps the pooled client"
        );

        CHANGES.store(1, Ordering::SeqCst);
        client.current("http://api.example.com/v1/responses").await;
        assert_eq!(
            rebuilds(&client),
            0,
            "an https-only proxy does not affect http targets"
        );
        client.current(url).await;
        assert_eq!(rebuilds(&client), 1, "a proxy turned on is picked up");
        client.current(url).await;
        assert_eq!(rebuilds(&client), 1);

        CHANGES.store(2, Ordering::SeqCst);
        client.current(url).await;
        assert_eq!(
            rebuilds(&client),
            2,
            "changed proxy credentials are picked up"
        );

        CHANGES.store(0, Ordering::SeqCst);
        client.current(url).await;
        assert_eq!(rebuilds(&client), 3, "a proxy turned off is picked up");
    }

    #[tokio::test]
    async fn rechecks_at_most_once_per_interval() {
        let client = SystemProxyClient::with_source(
            reqwest::Client::builder,
            interval_system,
            Duration::from_secs(3600),
        )
        .unwrap();
        INTERVAL.store(1, Ordering::SeqCst);
        client.current("https://api.example.com/v1/responses").await;
        assert_eq!(
            rebuilds(&client),
            0,
            "the change waits for the next recheck"
        );
        client.lock().next_check = Instant::now();
        client.current("https://api.example.com/v1/responses").await;
        assert_eq!(rebuilds(&client), 1);
    }
}
