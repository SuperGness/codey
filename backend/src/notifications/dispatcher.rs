use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::{Client, StatusCode, redirect};
use serde_json::{Value, json};

use super::channels::{NotificationChannelAdapter, adapter_for};
use super::{NotificationChannelConfig, NotificationEvent};

const MAX_NOTIFICATION_RESPONSE_BYTES: usize = 256 * 1024;

#[derive(Debug)]
pub struct NotificationDeliveryError {
    message: String,
    settle_delivery: bool,
    retry_with_fresh_context: bool,
    stale_token: bool,
}

impl NotificationDeliveryError {
    fn retryable(message: String) -> Self {
        Self {
            message,
            settle_delivery: false,
            retry_with_fresh_context: false,
            stale_token: false,
        }
    }

    fn settled(message: String) -> Self {
        Self {
            message,
            settle_delivery: true,
            retry_with_fresh_context: false,
            stale_token: false,
        }
    }

    fn retry_with_fresh_context(message: String) -> Self {
        Self {
            message,
            settle_delivery: false,
            retry_with_fresh_context: true,
            stale_token: false,
        }
    }

    fn stale_token(message: String) -> Self {
        Self {
            message,
            settle_delivery: false,
            retry_with_fresh_context: false,
            stale_token: true,
        }
    }

    pub fn should_settle_delivery(&self) -> bool {
        self.settle_delivery
    }

    pub fn should_retry_with_fresh_context(&self) -> bool {
        self.retry_with_fresh_context
    }

    pub fn indicates_stale_token(&self) -> bool {
        self.stale_token
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

    pub async fn send(
        &self,
        event: &NotificationEvent,
    ) -> std::result::Result<(), NotificationDeliveryError> {
        if !self.config.enabled || !self.config.is_configured() {
            return Ok(());
        }
        let adapter = adapter_for(&self.config);
        self.send_with_attempts(event, adapter.as_ref(), 2).await
    }

    async fn send_with_attempts(
        &self,
        event: &NotificationEvent,
        adapter: &dyn NotificationChannelAdapter,
        attempts: u32,
    ) -> std::result::Result<(), NotificationDeliveryError> {
        let preparation_error = self.prepare_channel(adapter).await;
        let mut last_error = None;
        for attempt in 0..attempts.max(1) {
            let request = adapter
                .build_request(&self.client, event)
                .map_err(|error| {
                    NotificationDeliveryError::retryable(format!(
                        "{}消息发送失败：{}",
                        adapter.display_name(),
                        adapter.sanitize_error(&error.to_string())
                    ))
                })?;
            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    match crate::http_response::read_bounded_body(
                        response,
                        MAX_NOTIFICATION_RESPONSE_BYTES,
                        "通知服务响应",
                    )
                    .await
                    {
                        Ok(response_body) => {
                            let response_body = String::from_utf8_lossy(&response_body);
                            match validate_http_response(adapter, status, &response_body) {
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
                                            "{}消息发送失败：{error}",
                                            adapter.display_name()
                                        )));
                                    }
                                    last_error = Some(error);
                                }
                            }
                        }
                        Err(error) => {
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
            if attempt + 1 < attempts.max(1) {
                tokio::time::sleep(Duration::from_millis(250 * 2u64.pow(attempt))).await;
            }
        }
        let mut error = last_error.unwrap_or_else(|| "未知错误".to_string());
        if let Some(preparation_error) = preparation_error {
            error.push_str("（iLink 激活检查未完成：");
            error.push_str(&preparation_error);
            error.push('）');
        }
        Err(NotificationDeliveryError::retryable(format!(
            "{}消息发送失败：{}",
            adapter.display_name(),
            adapter.sanitize_error(&error)
        )))
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
            .send_with_attempts(&event, adapter.as_ref(), 1)
            .await
            .map_err(anyhow::Error::new)?;
        Ok(json!({"status":"ok", "eventId": event.event_id}))
    }

    async fn prepare_channel(&self, adapter: &dyn NotificationChannelAdapter) -> Option<String> {
        let request = adapter.prepare_request(&self.client)?;
        let request = match request {
            Ok(request) => request,
            Err(error) => return Some(adapter.sanitize_error(&error.to_string())),
        };
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => return Some(adapter.sanitize_error(&error.to_string())),
        };
        let status = response.status();
        let body = match crate::http_response::read_bounded_body(
            response,
            MAX_NOTIFICATION_RESPONSE_BYTES,
            "通知渠道准备响应",
        )
        .await
        {
            Ok(body) => String::from_utf8_lossy(&body).into_owned(),
            Err(error) => return Some(adapter.sanitize_error(&error.to_string())),
        };
        match validate_http_response(adapter, status, &body) {
            Ok(()) => {
                adapter.mark_prepared();
                None
            }
            Err(error) => Some(adapter.sanitize_error(&error)),
        }
    }
}

pub(crate) fn notification_http_client() -> Result<Client> {
    Client::builder()
        .user_agent(format!("Codey/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .redirect(redirect::Policy::none())
        .build()
        .context("创建通知 HTTP 客户端失败")
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

        let error = dispatcher.send(&event).await.unwrap_err();

        assert!(error.should_settle_delivery());
        assert!(error.to_string().contains("已停止自动重试"));
        server.await.unwrap();
        assert_eq!(request_count.load(Ordering::Acquire), 1);
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

        let error = dispatcher.send(&event).await.unwrap_err();

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

        let error = dispatcher.send(&event).await.unwrap_err();

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

        let error = dispatcher.send(&event).await.unwrap_err();

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
