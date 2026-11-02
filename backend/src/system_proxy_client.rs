//! 跟随系统代理变化的长期 HTTP 客户端。
//!
//! reqwest 只在构建客户端时读取一次系统代理，运行期间开关代理软件后旧客户端
//! 仍按旧设置连接。全进程共用一份系统代理快照，每 10 秒最多读取一次；各客户端
//! 只和这份快照比较，请求地址的代理判定变化时，在下次使用时按自己的参数重建
//! 一次。新客户端仍由 reqwest 自行读取系统代理及其认证信息。
//!
//! 客户端各自保留连接参数（重定向策略、超时、HTTP/2 调优），不能合并成一个。

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use hyper_util::client::proxy::matcher::Matcher as SystemProxyMatcher;
use tokio_tungstenite::tungstenite::http::Uri;

/// 系统代理的复查间隔。读取一次约 0.15 ms，只在间隔到期后的首个请求上做。
const SYSTEM_PROXY_RECHECK_INTERVAL: Duration = Duration::from_secs(10);
/// 没有固定目标的客户端（通知渠道等）用它判定代理开关、地址与认证的变化；
/// 只针对个别域名的例外名单调整不会被发现。
const ANY_HOST_DESTINATION: &str = "https://codey-proxy-probe.invalid/";

static SYSTEM: SystemProxySource = SystemProxySource::new(
    SystemProxyMatcher::from_system,
    SYSTEM_PROXY_RECHECK_INTERVAL,
);

/// 系统代理快照的来源；生产环境全进程只有 [`SYSTEM`] 一份。
struct SystemProxySource {
    read: fn() -> SystemProxyMatcher,
    interval: Duration,
    state: Mutex<SourceState>,
}

struct SourceState {
    latest: Option<Arc<SystemProxyMatcher>>,
    next_read: Option<Instant>,
    #[cfg(test)]
    reads: usize,
}

impl SystemProxySource {
    const fn new(read: fn() -> SystemProxyMatcher, interval: Duration) -> Self {
        Self {
            read,
            interval,
            state: Mutex::new(SourceState {
                latest: None,
                next_read: None,
                #[cfg(test)]
                reads: 0,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, SourceState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn store(&self, latest: &Arc<SystemProxyMatcher>) {
        let mut state = self.lock();
        state.latest = Some(Arc::clone(latest));
        state.next_read = Some(Instant::now() + self.interval);
        #[cfg(test)]
        {
            state.reads += 1;
        }
    }

    /// 同步取快照：只供构建客户端时使用，那一步本身也要同步加载系统证书。
    fn latest_blocking(&self) -> Arc<SystemProxyMatcher> {
        {
            let state = self.lock();
            if let (Some(latest), Some(next_read)) = (&state.latest, state.next_read)
                && Instant::now() < next_read
            {
                return Arc::clone(latest);
            }
        }
        let latest = Arc::new((self.read)());
        self.store(&latest);
        latest
    }

    async fn latest(&self) -> Arc<SystemProxyMatcher> {
        let previous = {
            let mut state = self.lock();
            let now = Instant::now();
            match (&state.latest, state.next_read) {
                (Some(latest), Some(next_read)) if now < next_read => return Arc::clone(latest),
                (Some(latest), _) => {
                    // 先占住本轮读取：并发调用沿用旧快照；读取被取消时下一轮重试。
                    let latest = Arc::clone(latest);
                    state.next_read = Some(now + self.interval);
                    Some(latest)
                }
                (None, _) => None,
            }
        };
        let read = self.read;
        let latest = match tokio::task::spawn_blocking(read).await {
            Ok(matcher) => Arc::new(matcher),
            Err(_) => return previous.unwrap_or_else(|| Arc::new(read())),
        };
        self.store(&latest);
        latest
    }
}

/// 跟随系统代理变化的长期 HTTP 客户端，见模块说明。
pub(crate) struct SystemProxyClient {
    source: &'static SystemProxySource,
    builder: fn() -> reqwest::ClientBuilder,
    max_age: Option<Duration>,
    state: Mutex<ClientState>,
}

struct ClientState {
    client: reqwest::Client,
    /// 当前客户端构建时的系统代理快照；只在重建时更新，保证每次都和客户端
    /// 实际使用的设置比较。
    built_with: Arc<SystemProxyMatcher>,
    built_at: Instant,
    /// 正在为哪份快照重建；并发请求不重复重建，取消后等下一份快照再试。
    rebuilding_for: Option<Arc<SystemProxyMatcher>>,
    #[cfg(test)]
    rebuilds: usize,
}

impl SystemProxyClient {
    /// 同步构建：会加载系统证书与代理配置，调用方需在可阻塞的线程上执行。
    pub(crate) fn new_blocking(builder: fn() -> reqwest::ClientBuilder) -> Result<Self> {
        Self::with_source(&SYSTEM, builder)
    }

    /// 在阻塞线程池构建，不占用异步工作线程。
    pub(crate) async fn new(builder: fn() -> reqwest::ClientBuilder) -> Result<Self> {
        tokio::task::spawn_blocking(move || Self::new_blocking(builder))
            .await
            .context("创建 HTTP 客户端任务异常退出")?
    }

    /// 限制客户端最长使用时长：到期后即使代理未变也重建，顺带刷新系统证书。
    pub(crate) fn with_max_age(mut self, max_age: Duration) -> Self {
        self.max_age = Some(max_age);
        self
    }

    fn with_source(
        source: &'static SystemProxySource,
        builder: fn() -> reqwest::ClientBuilder,
    ) -> Result<Self> {
        let built_with = source.latest_blocking();
        let client = builder().build().context("创建 HTTP 客户端失败")?;
        Ok(Self {
            source,
            builder,
            max_age: None,
            state: Mutex::new(ClientState {
                client,
                built_with,
                built_at: Instant::now(),
                rebuilding_for: None,
                #[cfg(test)]
                rebuilds: 0,
            }),
        })
    }

    /// 测试用：包住给定客户端且永不复查系统代理。
    #[cfg(test)]
    pub(crate) fn fixed(client: reqwest::Client) -> Self {
        static NEVER: SystemProxySource = SystemProxySource::new(
            || SystemProxyMatcher::builder().build(),
            Duration::from_secs(365 * 24 * 60 * 60),
        );
        let built_with = NEVER.latest_blocking();
        Self {
            source: &NEVER,
            builder: reqwest::Client::builder,
            max_age: None,
            state: Mutex::new(ClientState {
                client,
                built_with,
                built_at: Instant::now(),
                rebuilding_for: None,
                rebuilds: 0,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, ClientState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 不复查系统代理，直接返回当前客户端。
    pub(crate) fn cached(&self) -> reqwest::Client {
        self.lock().client.clone()
    }

    /// 没有固定目标地址时使用，见 [`ANY_HOST_DESTINATION`]。
    pub(crate) async fn current_for_any_host(&self) -> reqwest::Client {
        self.current(ANY_HOST_DESTINATION).await
    }

    /// 返回访问 `url` 应使用的客户端；快照变化或超过最长使用时长时先重建。
    pub(crate) async fn current(&self, url: &str) -> reqwest::Client {
        let Ok(uri) = url.parse::<Uri>() else {
            return self.cached();
        };
        let latest = self.source.latest().await;
        let current = {
            let mut state = self.lock();
            let expired = self
                .max_age
                .is_some_and(|max_age| state.built_at.elapsed() >= max_age);
            let changed = !Arc::ptr_eq(&state.built_with, &latest)
                && !same_system_proxy(&state.built_with, &latest, &uri);
            let claimed = state
                .rebuilding_for
                .as_ref()
                .is_some_and(|target| Arc::ptr_eq(target, &latest));
            if !(expired || changed) || claimed {
                return state.client.clone();
            }
            state.rebuilding_for = Some(Arc::clone(&latest));
            state.client.clone()
        };
        let builder = self.builder;
        let rebuilt = tokio::task::spawn_blocking(move || builder().build())
            .await
            .ok()
            .and_then(Result::ok);
        // 构建失败时保留旧客户端，等下一份快照再试，避免每个请求都重建。
        let Some(rebuilt) = rebuilt else {
            return current;
        };
        let mut state = self.lock();
        state.client = rebuilt.clone();
        state.built_with = latest;
        state.built_at = Instant::now();
        state.rebuilding_for = None;
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

    fn rebuilds(client: &SystemProxyClient) -> usize {
        client.lock().rebuilds
    }

    fn expire(source: &SystemProxySource) {
        source.lock().next_read = Some(Instant::now());
    }

    const URL: &str = "https://api.example.com/v1/responses";

    // 每个测试独占一份假系统代理与快照来源，并行运行互不影响。
    static CHANGES: AtomicU8 = AtomicU8::new(0);
    static CHANGES_SOURCE: SystemProxySource =
        SystemProxySource::new(|| matcher_for(&CHANGES), Duration::ZERO);

    #[tokio::test]
    async fn rebuilds_only_when_the_request_target_proxy_changes() {
        let client =
            SystemProxyClient::with_source(&CHANGES_SOURCE, reqwest::Client::builder).unwrap();

        client.current(URL).await;
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
        client.current(URL).await;
        assert_eq!(rebuilds(&client), 1, "a proxy turned on is picked up");
        client.current(URL).await;
        assert_eq!(rebuilds(&client), 1);

        CHANGES.store(2, Ordering::SeqCst);
        client.current(URL).await;
        assert_eq!(
            rebuilds(&client),
            2,
            "changed proxy credentials are picked up"
        );

        CHANGES.store(0, Ordering::SeqCst);
        client.current_for_any_host().await;
        assert_eq!(
            rebuilds(&client),
            3,
            "a proxy turned off is picked up for any host"
        );
    }

    static SHARED: AtomicU8 = AtomicU8::new(0);
    static SHARED_SOURCE: SystemProxySource =
        SystemProxySource::new(|| matcher_for(&SHARED), Duration::from_secs(3600));

    #[tokio::test]
    async fn clients_share_one_snapshot_read_per_interval() {
        let first =
            SystemProxyClient::with_source(&SHARED_SOURCE, reqwest::Client::builder).unwrap();
        let second =
            SystemProxyClient::with_source(&SHARED_SOURCE, reqwest::Client::builder).unwrap();
        assert_eq!(
            SHARED_SOURCE.lock().reads,
            1,
            "construction reuses the fresh snapshot"
        );

        SHARED.store(1, Ordering::SeqCst);
        first.current(URL).await;
        second.current(URL).await;
        assert_eq!(
            rebuilds(&first) + rebuilds(&second),
            0,
            "the change waits for the next read"
        );

        expire(&SHARED_SOURCE);
        first.current(URL).await;
        second.current(URL).await;
        first.current(URL).await;
        assert_eq!(
            SHARED_SOURCE.lock().reads,
            2,
            "both clients reuse a single read"
        );
        assert_eq!(
            (rebuilds(&first), rebuilds(&second)),
            (1, 1),
            "each client rebuilds once"
        );
    }

    static AGED: AtomicU8 = AtomicU8::new(0);
    static AGED_SOURCE: SystemProxySource =
        SystemProxySource::new(|| matcher_for(&AGED), Duration::from_secs(3600));

    #[tokio::test]
    async fn max_age_rebuilds_even_without_a_proxy_change() {
        let client = SystemProxyClient::with_source(&AGED_SOURCE, reqwest::Client::builder)
            .unwrap()
            .with_max_age(Duration::from_secs(600));
        client.current_for_any_host().await;
        assert_eq!(rebuilds(&client), 0);
        client.lock().built_at = Instant::now() - Duration::from_secs(601);
        client.current_for_any_host().await;
        assert_eq!(rebuilds(&client), 1);
        client.current_for_any_host().await;
        assert_eq!(rebuilds(&client), 1);
    }
}
