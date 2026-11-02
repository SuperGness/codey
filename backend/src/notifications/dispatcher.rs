use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::{Client, StatusCode, redirect};
use serde_json::{Value, json};

use super::channels::{NotificationChannelAdapter, adapter_for};
use super::{NotificationChannelConfig, NotificationEvent};
use crate::system_proxy_client::SystemProxyClient;

const MAX_NOTIFICATION_RESPONSE_BYTES: usize = 256 * 1024;

#[derive(Debug)]
pub struct NotificationDeliveryError {
    message: String,
    settle_delivery: bool,
    retry_with_fresh_context: bool,
    stale_token: bool,
    uncertain: bool,
    retry_after_ms: Option<u64>,
}

impl NotificationDeliveryError {
    fn retryable(message: String) -> Self {
        Self {
            message,
            settle_delivery: false,
            retry_with_fresh_context: false,
            stale_token: false,
            uncertain: false,
            retry_after_ms: None,
        }
    }

    fn settled(message: String) -> Self {
        Self {
            message,
            settle_delivery: true,
            retry_with_fresh_context: false,
            stale_token: false,
            uncertain: true,
            retry_after_ms: None,
        }
    }

    fn retry_with_fresh_context(message: String) -> Self {
        Self {
            message,
            settle_delivery: false,
            retry_with_fresh_context: true,
            stale_token: false,
            uncertain: false,
            retry_after_ms: None,
        }
    }

    fn stale_token(message: String) -> Self {
        Self {
            message,
            settle_delivery: false,
            retry_with_fresh_context: false,
            stale_token: true,
            uncertain: false,
            retry_after_ms: None,
        }
    }

    #[cfg(test)]
    pub fn should_settle_delivery(&self) -> bool {
        self.settle_delivery
    }

    pub fn should_retry_with_fresh_context(&self) -> bool {
        self.retry_with_fresh_context
    }

    pub fn indicates_stale_token(&self) -> bool {
        self.stale_token
    }

    pub(crate) fn outcome(&self) -> super::outbox::DeliveryOutcome {
        if self.uncertain {
            super::outbox::DeliveryOutcome::Uncertain
        } else if self.settle_delivery {
            super::outbox::DeliveryOutcome::PermanentFailure
        } else {
            super::outbox::DeliveryOutcome::Retryable {
                retry_after_ms: self.retry_after_ms,
            }
        }
    }
}

impl std::fmt::Display for NotificationDeliveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for NotificationDeliveryError {}

#[derive(Clone)]
pub struct NotificationDispatcher {
    client: Client,
    config: NotificationChannelConfig,
}

impl NotificationDispatcher {
    #[cfg(test)]
    pub fn new(config: NotificationChannelConfig) -> Result<Self> {
        let client = notification_http_client()?;
        Ok(Self::with_client(client, config))
    }

    pub fn with_client(client: Client, config: NotificationChannelConfig) -> Self {
        Self { client, config }
    }

    /// 自动通知由持久队列安排重试，一次认领只执行一次 HTTP 投递。
    pub(crate) async fn send_once(
        &self,
        event: &NotificationEvent,
    ) -> std::result::Result<(), NotificationDeliveryError> {
        let adapter = adapter_for(&self.config);
        self.send_with_adapter(event, adapter.as_ref()).await
    }

    async fn send_with_adapter(
        &self,
        event: &NotificationEvent,
        adapter: &dyn NotificationChannelAdapter,
    ) -> std::result::Result<(), NotificationDeliveryError> {
        let last_error;
        let mut retry_after_ms = None;
        {
            let request = adapter
                .build_request(&self.client, event)
                .map_err(|error| {
                    self.record_attempt(event, 1, "request_build_failed", None, None);
                    let mut failure = NotificationDeliveryError::settled(format!(
                        "{}消息发送失败：{}",
                        adapter.display_name(),
                        adapter.sanitize_error(&error.to_string())
                    ));
                    failure.uncertain = false;
                    failure
                })?;
            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    retry_after_ms = response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok())
                        .and_then(parse_retry_after_ms);
                    match crate::http_response::read_bounded_body(
                        response,
                        MAX_NOTIFICATION_RESPONSE_BYTES,
                        "通知服务响应",
                    )
                    .await
                    {
                        Ok(response_body) => {
                            let response_body = String::from_utf8_lossy(&response_body);
                            let validation =
                                validate_http_response(adapter, status, &response_body);
                            let outcome = if validation.is_ok() {
                                "delivered"
                            } else if status.is_success()
                                && (adapter
                                    .pause_on_stale_token_success_status_error(&response_body)
                                    || adapter.retry_with_fresh_context_on_success_status_error(
                                        &response_body,
                                    ))
                            {
                                "retryable_error"
                            } else if (status.is_success()
                                && adapter.settle_on_success_status_error(&response_body))
                                || is_permanent_client_error(status)
                            {
                                "settled_error"
                            } else {
                                "retryable_error"
                            };
                            self.record_attempt(
                                event,
                                1,
                                outcome,
                                Some(status),
                                notification_response_code(&response_body),
                            );
                            match validation {
                                Ok(()) => return Ok(()),
                                Err(error) => {
                                    if status.is_success()
                                        && adapter.pause_on_stale_token_success_status_error(
                                            &response_body,
                                        )
                                    {
                                        return Err(NotificationDeliveryError::stale_token(
                                            format!(
                                                "{}消息发送失败：{error}",
                                                adapter.display_name()
                                            ),
                                        ));
                                    }
                                    if status.is_success()
                                        && adapter.retry_with_fresh_context_on_success_status_error(
                                            &response_body,
                                        )
                                    {
                                        return Err(
                                            NotificationDeliveryError::retry_with_fresh_context(
                                                format!(
                                                    "{}消息发送失败：{error}",
                                                    adapter.display_name()
                                                ),
                                            ),
                                        );
                                    }
                                    if status.is_success()
                                        && adapter.settle_on_success_status_error(&response_body)
                                    {
                                        return Err(NotificationDeliveryError::settled(format!(
                                            "{}消息发送失败，已停止自动重试：{error}",
                                            adapter.display_name()
                                        )));
                                    }
                                    if is_permanent_client_error(status) {
                                        // A rejected token, unknown chat or malformed
                                        // payload will not fix itself; retrying every
                                        // scan only burns the ledger with fsyncs.
                                        let mut failure = NotificationDeliveryError::settled(
                                            format!(
                                                "{}消息发送失败（HTTP {}，已停止自动重试）：{error}",
                                                adapter.display_name(),
                                                status.as_u16()
                                            ),
                                        );
                                        failure.uncertain = false;
                                        return Err(failure);
                                    }
                                    last_error = Some(error);
                                }
                            }
                        }
                        Err(error) => {
                            self.record_attempt(
                                event,
                                1,
                                "response_read_uncertain",
                                Some(status),
                                None,
                            );
                            return Err(NotificationDeliveryError::settled(format!(
                                "{}消息发送结果不确定，已停止自动重试：{}",
                                adapter.display_name(),
                                adapter.sanitize_error(&format!(
                                    "{}响应读取失败：{error}",
                                    adapter.display_name()
                                ))
                            )));
                        }
                    }
                }
                Err(error) => {
                    let outcome = if error.is_timeout() {
                        "timeout_uncertain"
                    } else if error.is_connect() {
                        "connect_failed"
                    } else {
                        "transport_uncertain"
                    };
                    self.record_attempt(event, 1, outcome, None, None);
                    if error.is_timeout() || !error.is_connect() {
                        return Err(NotificationDeliveryError::settled(format!(
                            "{}消息发送结果不确定，已停止自动重试：{}",
                            adapter.display_name(),
                            adapter.sanitize_error(&error.to_string())
                        )));
                    }
                    last_error = Some(adapter.sanitize_error(&error.to_string()));
                }
            }
        }
        let error = last_error.unwrap_or_else(|| "未知错误".to_string());
        let mut failure = NotificationDeliveryError::retryable(format!(
            "{}消息发送失败：{}",
            adapter.display_name(),
            adapter.sanitize_error(&error)
        ));
        failure.retry_after_ms = retry_after_ms;
        Err(failure)
    }

    fn record_attempt(
        &self,
        event: &NotificationEvent,
        attempt: u32,
        outcome: &'static str,
        status: Option<StatusCode>,
        response_code: Option<i64>,
    ) {
        let detail = notification_attempt_diagnostic(
            event,
            self.config.kind,
            attempt,
            outcome,
            status,
            response_code,
        );
        // 只记录固定分类和数值状态，不写入渠道凭据、消息正文或远端错误文本。
        #[cfg(not(test))]
        let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
            "notification.delivery_attempt",
            detail,
        );
        #[cfg(test)]
        let _ = detail;
    }

    pub async fn test(&self) -> Result<Value> {
        let adapter = adapter_for(&self.config);
        if let Some(error) = adapter.configuration_error() {
            anyhow::bail!(error);
        }
        let event = NotificationEvent::new(
            "codey.test",
            "test-session",
            "test-profile",
            "Codex",
            0,
            None,
        )
        .with_session_name("通知渠道测试")
        .with_reasoning_effort("high");
        let mut tester = self.clone();
        tester.config.enabled = true;
        // A test click must finish promptly and report the real first error.
        // Background notifications only retry failures known not to be timeouts.
        let adapter = adapter_for(&tester.config);
        tester
            .send_with_adapter(&event, adapter.as_ref())
            .await
            .map_err(anyhow::Error::new)?;
        Ok(json!({"status":"ok", "eventId": event.event_id}))
    }
}

fn parse_retry_after_ms(value: &str) -> Option<u64> {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(seconds.saturating_mul(1_000));
    }
    chrono::DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|date| {
            date.timestamp_millis()
                .saturating_sub(chrono::Utc::now().timestamp_millis())
                .max(0) as u64
        })
}

/// 4xx responses other than request-timeout and rate-limit describe a request
/// the service will keep rejecting until the configuration changes.
fn is_permanent_client_error(status: StatusCode) -> bool {
    status.is_client_error()
        && !matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
        )
}

fn notification_http_client_builder() -> reqwest::ClientBuilder {
    Client::builder()
        .user_agent(format!("Codey/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .redirect(redirect::Policy::none())
}

#[cfg(test)]
pub(crate) fn notification_http_client() -> Result<Client> {
    notification_http_client_builder()
        .build()
        .context("创建通知 HTTP 客户端失败")
}

/// How long a built client is reused before the root store is loaded again.
const SHARED_CLIENT_MAX_AGE: Duration = Duration::from_secs(10 * 60);

/// Building a reqwest client loads the operating-system root certificates,
/// which is measurable on macOS, so delivery shares one client. It is rebuilt
/// when the shared system proxy snapshot changes, and at least every 10 minutes
/// to pick up root store changes. Channels have no fixed host, so the proxy is
/// compared for an arbitrary host.
pub(crate) async fn shared_notification_http_client() -> Result<Client> {
    static SHARED: std::sync::OnceLock<SystemProxyClient> = std::sync::OnceLock::new();
    let shared = match SHARED.get() {
        Some(shared) => shared,
        None => {
            let client = SystemProxyClient::new_blocking(notification_http_client_builder)
                .context("创建通知 HTTP 客户端失败")?
                .with_max_age(SHARED_CLIENT_MAX_AGE);
            SHARED.get_or_init(|| client)
        }
    };
    Ok(shared.current_for_any_host().await)
}

fn notification_response_code(body: &str) -> Option<i64> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    ["code", "StatusCode", "errcode", "ret", "error_code"]
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_i64))
}

fn notification_attempt_diagnostic(
    event: &NotificationEvent,
    channel: super::NotificationChannelKind,
    attempt: u32,
    outcome: &'static str,
    status: Option<StatusCode>,
    response_code: Option<i64>,
) -> Value {
    let event_type = match event.event.as_str() {
        "session.completed" | "session.failed" | "session.waiting" | "codey.test" => {
            event.event.as_str()
        }
        _ => "unknown",
    };
    json!({
        "eventId": event.event_id,
        "eventType": event_type,
        "channelType": channel,
        "attempt": attempt,
        "outcome": outcome,
        "httpStatus": status.map(|status| status.as_u16()),
        "responseCode": response_code,
    })
}

fn validate_http_response(
    adapter: &dyn NotificationChannelAdapter,
    status: StatusCode,
    body: &str,
) -> std::result::Result<(), String> {
    let channel_result = adapter.validate_response(body);
    if status.is_success() {
        return channel_result.map_err(|error| adapter.sanitize_error(&error));
    }

    let error = match channel_result {
        Ok(()) => format!("{}返回 HTTP {status}", adapter.display_name()),
        Err(detail) => format!("{}返回 HTTP {status}：{detail}", adapter.display_name()),
    };
    Err(adapter.sanitize_error(&error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::NotificationChannelKind;

    #[tokio::test]
    async fn test_requires_a_configured_channel() {
        let dispatcher = NotificationDispatcher::new(NotificationChannelConfig::default()).unwrap();
        assert!(
            dispatcher
                .test()
                .await
                .unwrap_err()
                .to_string()
                .contains("Webhook")
        );
    }

    #[tokio::test]
    async fn an_ambiguous_http_timeout_is_never_retried() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        use tokio::io::AsyncReadExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let request_count = Arc::new(AtomicUsize::new(0));
        let server_request_count = Arc::clone(&request_count);
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let bytes_read = socket.read(&mut request).await.unwrap();
            assert!(bytes_read > 0);
            server_request_count.fetch_add(1, Ordering::AcqRel);
            tokio::time::sleep(Duration::from_millis(1_500)).await;
            drop(socket);

            if let Ok(Ok((_retry, _))) =
                tokio::time::timeout(Duration::from_millis(400), listener.accept()).await
            {
                server_request_count.fetch_add(1, Ordering::AcqRel);
            }
        });
        let client = Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        let config = NotificationChannelConfig {
            id: "timeout-feishu".to_string(),
            kind: NotificationChannelKind::Feishu,
            enabled: true,
            url: format!("http://{address}"),
            allow_insecure_test_url: true,
            ..NotificationChannelConfig::default()
        };
        let dispatcher = NotificationDispatcher::with_client(client, config);
        let event = NotificationEvent::new(
            "session.waiting",
            "session-timeout",
            "profile-timeout",
            "Codex",
            0,
            None,
        );

        let error = dispatcher.send_once(&event).await.unwrap_err();

        assert!(error.should_settle_delivery());
        assert!(error.to_string().contains("已停止自动重试"));
        assert!(matches!(
            error.outcome(),
            super::super::outbox::DeliveryOutcome::Uncertain
        ));
        server.await.unwrap();
        assert_eq!(request_count.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn a_permanent_client_error_settles_the_delivery_without_retrying() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = 0;
            while let Ok(Ok((mut socket, _))) =
                tokio::time::timeout(Duration::from_millis(400), listener.accept()).await
            {
                let mut request = [0_u8; 4096];
                let _ = socket.read(&mut request).await.unwrap();
                requests += 1;
                let body = r#"{"ok":false,"description":"Unauthorized"}"#;
                let response = format!(
                    "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        let config = NotificationChannelConfig {
            id: "unauthorized-feishu".to_string(),
            kind: NotificationChannelKind::Feishu,
            enabled: true,
            url: format!("http://{address}"),
            allow_insecure_test_url: true,
            ..NotificationChannelConfig::default()
        };
        let dispatcher = NotificationDispatcher::new(config).unwrap();
        let event = NotificationEvent::new(
            "session.waiting",
            "session-unauthorized",
            "profile-unauthorized",
            "Codex",
            0,
            None,
        );

        let error = dispatcher.send_once(&event).await.unwrap_err();

        assert!(error.should_settle_delivery());
        assert!(error.to_string().contains("HTTP 401"));
        assert!(matches!(
            error.outcome(),
            super::super::outbox::DeliveryOutcome::PermanentFailure
        ));
        assert_eq!(server.await.unwrap(), 1);
    }

    #[tokio::test]
    async fn rate_limit_returns_retry_after_to_the_queue_without_an_internal_retry() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _n = socket.read(&mut request).await.unwrap();
            socket.write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 7\r\nContent-Length: 10\r\nConnection: close\r\n\r\n{\"code\":0}").await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(350), listener.accept())
                    .await
                    .is_err()
            );
        });
        let dispatcher = NotificationDispatcher::with_client(
            Client::builder().no_proxy().build().unwrap(),
            NotificationChannelConfig {
                url: format!("http://{address}"),
                allow_insecure_test_url: true,
                ..NotificationChannelConfig::default()
            },
        );
        let event =
            NotificationEvent::new("session.failed", "session", "profile", "model", 0, None);
        let error = dispatcher.send_once(&event).await.unwrap_err();
        assert!(matches!(
            error.outcome(),
            super::super::outbox::DeliveryOutcome::Retryable {
                retry_after_ms: Some(7_000)
            }
        ));
        assert_eq!(parse_retry_after_ms("invalid"), None);
        assert_eq!(
            parse_retry_after_ms("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(0)
        );
        server.await.unwrap();
    }

    #[test]
    fn only_timeouts_and_rate_limits_stay_retryable_among_client_errors() {
        assert!(is_permanent_client_error(StatusCode::BAD_REQUEST));
        assert!(is_permanent_client_error(StatusCode::UNAUTHORIZED));
        assert!(is_permanent_client_error(StatusCode::NOT_FOUND));
        assert!(!is_permanent_client_error(StatusCode::REQUEST_TIMEOUT));
        assert!(!is_permanent_client_error(StatusCode::TOO_MANY_REQUESTS));
        assert!(!is_permanent_client_error(StatusCode::BAD_GATEWAY));
        assert!(!is_permanent_client_error(StatusCode::OK));
    }

    #[tokio::test]
    async fn clawbot_does_not_retry_after_an_ambiguous_success_status() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut send_requests = 0;
            let response_body = "not json";
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 8192];
            let bytes_read = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..bytes_read]);
            if request.contains("POST /ilink/bot/sendmessage ") {
                send_requests += 1;
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                response_body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();

            if let Ok(Ok((mut socket, _))) =
                tokio::time::timeout(Duration::from_millis(500), listener.accept()).await
            {
                let mut request = [0_u8; 8192];
                let bytes_read = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..bytes_read]);
                if request.contains("POST /ilink/bot/sendmessage ") {
                    send_requests += 1;
                }
            }
            send_requests
        });
        let config = NotificationChannelConfig {
            id: "ambiguous-clawbot".to_string(),
            kind: NotificationChannelKind::WechatClaw,
            enabled: true,
            url: format!("http://{address}"),
            bot_token: "test-bot-token".to_string(),
            context_token: "test-context-token".to_string(),
            chat_id: "recipient@im.wechat".to_string(),
            allow_insecure_test_url: true,
            ..NotificationChannelConfig::default()
        };
        let dispatcher = NotificationDispatcher::new(config).unwrap();
        let event = NotificationEvent::new(
            "session.completed",
            "session-clawbot",
            "profile-clawbot",
            "Codex",
            0,
            None,
        );

        let error = dispatcher.send_once(&event).await.unwrap_err();

        assert!(error.should_settle_delivery());
        assert!(error.to_string().contains("无法解析"));
        assert_eq!(server.await.unwrap(), 1);
    }

    #[tokio::test]
    async fn clawbot_surfaces_stale_token_without_retrying_the_request() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let response_body = r#"{"errcode":-14,"errmsg":"token expired"}"#;
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 8192];
            let bytes_read = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..bytes_read]);
            assert!(request.contains("POST /ilink/bot/sendmessage "));
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                response_body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();

            tokio::time::timeout(Duration::from_millis(500), listener.accept())
                .await
                .is_err()
        });
        let config = NotificationChannelConfig {
            id: "stale-clawbot".to_string(),
            kind: NotificationChannelKind::WechatClaw,
            enabled: true,
            url: format!("http://{address}"),
            bot_token: "test-bot-token".to_string(),
            context_token: "test-context-token".to_string(),
            chat_id: "recipient@im.wechat".to_string(),
            allow_insecure_test_url: true,
            ..NotificationChannelConfig::default()
        };
        let dispatcher = NotificationDispatcher::new(config).unwrap();
        let event = NotificationEvent::new(
            "session.completed",
            "session-clawbot",
            "profile-clawbot",
            "Codex",
            0,
            None,
        );

        let error = dispatcher.send_once(&event).await.unwrap_err();

        assert!(error.indicates_stale_token());
        assert!(!error.should_settle_delivery());
        assert!(error.to_string().contains("-14"));
        assert!(server.await.unwrap());
    }

    #[tokio::test]
    async fn clawbot_requests_fresh_context_after_an_explicit_prepare_failure() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let response_body = r#"{"ret":-2,"errmsg":"prepare failed"}"#;
            let mut send_requests = 0;
            for _ in 0..1 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 8192];
                let bytes_read = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..bytes_read]);
                if request.contains("POST /ilink/bot/sendmessage ") {
                    send_requests += 1;
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                    response_body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            send_requests
        });
        let config = NotificationChannelConfig {
            id: "unprepared-clawbot".to_string(),
            kind: NotificationChannelKind::WechatClaw,
            enabled: true,
            url: format!("http://{address}"),
            bot_token: "test-bot-token".to_string(),
            context_token: "test-context-token".to_string(),
            chat_id: "recipient@im.wechat".to_string(),
            allow_insecure_test_url: true,
            ..NotificationChannelConfig::default()
        };
        let dispatcher = NotificationDispatcher::new(config).unwrap();
        let event = NotificationEvent::new(
            "session.completed",
            "session-clawbot",
            "profile-clawbot",
            "Codex",
            0,
            None,
        );

        let error = dispatcher.send_once(&event).await.unwrap_err();

        assert!(!error.should_settle_delivery());
        assert!(error.should_retry_with_fresh_context());
        assert!(error.to_string().contains("暂时无法准备投递"));
        assert_eq!(server.await.unwrap(), 1);
    }

    #[tokio::test]
    async fn notification_client_never_follows_redirects() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let redirect_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect_address = redirect_listener.local_addr().unwrap();
        let target_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_address = target_listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = redirect_listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            let response = format!(
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target_address}/leak\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let response = notification_http_client()
            .unwrap()
            .post(format!("http://{redirect_address}/webhook"))
            .body("notification-secret")
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        server.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), target_listener.accept())
                .await
                .is_err()
        );
    }

    #[test]
    fn successful_http_status_still_requires_valid_channel_confirmation() {
        let config = NotificationChannelConfig {
            kind: NotificationChannelKind::Feishu,
            url: "https://open.feishu.cn/open-apis/bot/v2/hook/secret".to_string(),
            ..NotificationChannelConfig::default()
        };
        let adapter = adapter_for(&config);

        let error =
            validate_http_response(adapter.as_ref(), StatusCode::OK, "not json").unwrap_err();

        assert!(error.contains("无法解析"));
    }

    #[test]
    fn attempt_diagnostics_exclude_message_content_and_remote_text() {
        let event = NotificationEvent::new(
            "session.failed",
            "private-session",
            "private-profile",
            "private-model",
            10,
            Some("private-error".to_string()),
        )
        .with_session_name("private-title");
        let body = r#"{"code":9499,"msg":"private-remote-text"}"#;
        let detail = notification_attempt_diagnostic(
            &event,
            NotificationChannelKind::Feishu,
            2,
            "retryable_error",
            Some(StatusCode::OK),
            notification_response_code(body),
        );
        assert_eq!(detail["eventId"], event.event_id);
        assert_eq!(detail["responseCode"], 9499);
        assert_eq!(detail["httpStatus"], 200);
        assert!(!detail.to_string().contains("private-"));
        assert_eq!(
            notification_response_code(r#"{"code":"private-token"}"#),
            None
        );
    }

    #[test]
    fn response_errors_are_bounded_and_do_not_expose_credentials() {
        let secret_url = "https://open.feishu.cn/open-apis/bot/v2/hook/private-secret";
        let config = NotificationChannelConfig {
            kind: NotificationChannelKind::Feishu,
            url: secret_url.to_string(),
            ..NotificationChannelConfig::default()
        };
        let adapter = adapter_for(&config);
        let response = serde_json::json!({
            "code": 19021,
            "msg": format!("{secret_url} {}", "x".repeat(500)),
        })
        .to_string();

        let error = validate_http_response(adapter.as_ref(), StatusCode::BAD_REQUEST, &response)
            .unwrap_err();

        assert!(!error.contains(secret_url));
        assert!(error.contains("***"));
        assert!(error.chars().count() < 300);
    }
}
