use std::time::Duration;

#[cfg(test)]
use anyhow::Context;
use anyhow::Result;
use chrono::{DateTime, Local, TimeZone};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::config::WebhookConfig;

const FEISHU_TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WebhookEvent {
    pub event_id: String,
    pub event: String,
    pub timestamp: String,
    pub session_id: String,
    pub session_name: String,
    pub profile_id: String,
    pub model: String,
    pub reasoning_effort: String,
    pub duration_ms: u128,
    pub error: Option<String>,
}

impl WebhookEvent {
    pub fn new(
        event: impl Into<String>,
        session_id: impl Into<String>,
        profile_id: impl Into<String>,
        model: impl Into<String>,
        duration_ms: u128,
        error: Option<String>,
    ) -> Self {
        Self {
            event_id: Uuid::new_v4().to_string(),
            event: event.into(),
            timestamp: local_timestamp_now(),
            session_id: session_id.into(),
            session_name: "未命名会话".to_string(),
            profile_id: profile_id.into(),
            model: model.into(),
            reasoning_effort: "默认".to_string(),
            duration_ms,
            error,
        }
    }

    pub fn with_session_name(mut self, session_name: impl Into<String>) -> Self {
        let session_name = session_name.into();
        self.session_name = if session_name.trim().is_empty() {
            "未命名会话".to_string()
        } else {
            session_name
        };
        self
    }

    pub fn with_reasoning_effort(mut self, reasoning_effort: impl Into<String>) -> Self {
        let reasoning_effort = reasoning_effort.into();
        self.reasoning_effort = if reasoning_effort.trim().is_empty() {
            "默认".to_string()
        } else {
            reasoning_effort
        };
        self
    }
}

#[derive(Clone)]
pub struct WebhookDispatcher {
    client: Client,
    config: WebhookConfig,
}

impl WebhookDispatcher {
    #[cfg(test)]
    pub fn new(config: WebhookConfig) -> Result<Self> {
        let client = Client::builder()
            .user_agent("Codey/0.1")
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(8))
            .build()
            .context("创建 Webhook HTTP 客户端失败")?;
        Ok(Self::with_client(client, config))
    }

    pub fn with_client(client: Client, config: WebhookConfig) -> Self {
        Self { client, config }
    }

    pub async fn send(&self, event: &WebhookEvent) -> Result<()> {
        self.send_with_attempts(event, 2).await
    }

    async fn send_with_attempts(&self, event: &WebhookEvent, attempts: u32) -> Result<()> {
        if !self.config.enabled || self.config.url.trim().is_empty() {
            return Ok(());
        }
        let body = feishu_body(event)?;
        let mut last_error = None;
        for attempt in 0..attempts.max(1) {
            let request = self
                .client
                .post(self.config.url.trim())
                .header("Content-Type", "application/json; charset=utf-8")
                .json(&body);
            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    let response_body = response.text().await.unwrap_or_default();
                    if status.is_success() {
                        match feishu_response_error(&response_body) {
                            None => return Ok(()),
                            Some(error) => last_error = Some(error),
                        }
                    } else {
                        last_error = Some(format!(
                            "飞书机器人返回 HTTP {status}：{}",
                            response_body.chars().take(300).collect::<String>()
                        ));
                    }
                }
                Err(error) => last_error = Some(error.to_string()),
            }
            if attempt + 1 < attempts.max(1) {
                tokio::time::sleep(Duration::from_millis(250 * 2u64.pow(attempt))).await;
            }
        }
        Err(anyhow::anyhow!(
            "飞书机器人消息发送失败：{}",
            last_error.unwrap_or_else(|| "未知错误".to_string())
        ))
    }

    pub async fn test(&self) -> Result<Value> {
        if self.config.url.trim().is_empty() {
            anyhow::bail!("请先填写飞书机器人 Webhook 地址");
        }
        let event = WebhookEvent::new(
            "codey.test",
            "test-session",
            "test-profile",
            "Codex",
            0,
            None,
        )
        .with_session_name("飞书卡片测试")
        .with_reasoning_effort("high");
        let mut tester = self.clone();
        tester.config.enabled = true;
        // A test click must finish promptly and report the real first error;
        // background completion notifications retain one retry.
        tester.send_with_attempts(&event, 1).await?;
        Ok(json!({"status":"ok", "eventId": event.event_id}))
    }
}

fn feishu_body(event: &WebhookEvent) -> Result<Value> {
    let (title, template) = match event.event.as_str() {
        "session.completed" => ("Codex会话完成", "green"),
        "session.failed" => ("Codex会话失败", "red"),
        "session.waiting" | "codey.test" => ("Codex会话等待介入", "orange"),
        _ => ("Codex会话等待介入", "orange"),
    };
    let session_name = feishu_markdown_value(&event.session_name, "未命名会话");
    let model = feishu_markdown_value(&event.model, "Codex");
    let reasoning_effort = feishu_markdown_value(&event.reasoning_effort, "默认");
    let sent_at = feishu_markdown_value(&format_feishu_timestamp(&event.timestamp), "未知");
    let body = json!({
        "msg_type": "interactive",
        "card": {
            "config": {"wide_screen_mode": true},
            "header": {
                "template": template,
                "title": {
                    "tag": "plain_text",
                    "content": title,
                },
            },
            "elements": [{
                "tag": "div",
                "fields": [
                    {
                        "is_short": false,
                        "text": {
                            "tag": "lark_md",
                            "content": format!("**会话标题**\n{session_name}"),
                        },
                    },
                    {
                        "is_short": true,
                        "text": {
                            "tag": "lark_md",
                            "content": format!("**使用模型**\n{model}"),
                        },
                    },
                    {
                        "is_short": true,
                        "text": {
                            "tag": "lark_md",
                            "content": format!("**推理深度**\n{reasoning_effort}"),
                        },
                    },
                    {
                        "is_short": false,
                        "text": {
                            "tag": "lark_md",
                            "content": format!("**发送时间**\n{sent_at}"),
                        },
                    },
                    {
                        "is_short": false,
                        "text": {
                            "tag": "lark_md",
                            "content": format!("**耗时**\n{}", format_duration(event.duration_ms)),
                        },
                    },
                ],
            }],
        },
    });
    Ok(body)
}

fn feishu_markdown_value(value: &str, fallback: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let value = if normalized.is_empty() {
        fallback
    } else {
        normalized.as_str()
    };
    value
        .replace('\\', "\\\\")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('`', "\\`")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('<', "＜")
        .replace('>', "＞")
}

fn format_duration(duration_ms: u128) -> String {
    if duration_ms == 0 {
        return "0 秒".to_string();
    }
    if duration_ms < 1_000 {
        return format!("{duration_ms} 毫秒");
    }
    if duration_ms < 60_000 {
        if duration_ms.is_multiple_of(1_000) {
            return format!("{} 秒", duration_ms / 1_000);
        }
        return format!("{:.1} 秒", duration_ms as f64 / 1_000.0);
    }
    let total_seconds = duration_ms / 1_000;
    let seconds = total_seconds % 60;
    let total_minutes = total_seconds / 60;
    if total_minutes < 60 {
        return format!("{total_minutes} 分 {seconds} 秒");
    }
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    format!("{hours} 小时 {minutes} 分 {seconds} 秒")
}

fn feishu_response_error(body: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let code = value
        .get("code")
        .or_else(|| value.get("StatusCode"))
        .and_then(Value::as_i64)?;
    if code == 0 {
        return None;
    }
    let message = value
        .get("msg")
        .or_else(|| value.get("StatusMessage"))
        .and_then(Value::as_str)
        .unwrap_or("未知错误");
    Some(format!("飞书机器人返回错误 {code}：{message}"))
}

fn local_timestamp_now() -> String {
    Local::now().format(FEISHU_TIMESTAMP_FORMAT).to_string()
}

fn format_feishu_timestamp(timestamp: &str) -> String {
    let timestamp = timestamp.trim();
    if let Some(datetime) = timestamp
        .strip_prefix("unix-ms:")
        .and_then(|millis| millis.trim().parse::<i64>().ok())
        .and_then(|millis| Local.timestamp_millis_opt(millis).single())
    {
        return datetime.format(FEISHU_TIMESTAMP_FORMAT).to_string();
    }
    if let Ok(datetime) = DateTime::parse_from_str(timestamp, "%Y-%m-%d %H:%M:%S %:z") {
        return datetime.format(FEISHU_TIMESTAMP_FORMAT).to_string();
    }
    if let Ok(datetime) = DateTime::parse_from_rfc3339(timestamp) {
        return datetime.format(FEISHU_TIMESTAMP_FORMAT).to_string();
    }
    timestamp.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_have_unique_ids_and_do_not_include_prompt_fields() {
        let first = WebhookEvent::new("session.completed", "s1", "p1", "gpt-5", 10, None);
        let second = WebhookEvent::new("session.completed", "s1", "p1", "gpt-5", 10, None);
        assert_ne!(first.event_id, second.event_id);
        let value = serde_json::to_value(first).unwrap();
        assert!(value.get("prompt").is_none());
        assert!(value.get("messages").is_none());
    }

    #[test]
    fn feishu_card_body_uses_custom_bot_schema() {
        let mut event =
            WebhookEvent::new("session.completed", "s1", "p1", "gpt-5.4", 192_300, None)
                .with_session_name("发布 Codey 版本")
                .with_reasoning_effort("xhigh");
        event.timestamp = "2026-07-21 20:30:00".to_string();
        let body = feishu_body(&event).unwrap();
        assert_eq!(body["msg_type"], "interactive");
        assert_eq!(body["card"]["header"]["title"]["content"], "Codex会话完成");
        assert_eq!(body["card"]["header"]["template"], "green");
        let fields = body["card"]["elements"][0]["fields"].as_array().unwrap();
        assert_eq!(fields.len(), 5);
        assert!(
            fields[0]["text"]["content"]
                .as_str()
                .unwrap()
                .contains("会话标题")
        );
        assert!(
            fields[0]["text"]["content"]
                .as_str()
                .unwrap()
                .contains("发布 Codey 版本")
        );
        assert!(
            fields[1]["text"]["content"]
                .as_str()
                .unwrap()
                .contains("gpt-5.4")
        );
        assert!(
            fields[2]["text"]["content"]
                .as_str()
                .unwrap()
                .contains("xhigh")
        );
        assert!(
            fields[3]["text"]["content"]
                .as_str()
                .unwrap()
                .contains("发送时间")
        );
        assert!(
            fields[3]["text"]["content"]
                .as_str()
                .unwrap()
                .contains("2026-07-21 20:30:00")
        );
        assert!(
            fields[4]["text"]["content"]
                .as_str()
                .unwrap()
                .contains("3 分 12 秒")
        );
        assert!(body.get("content").is_none());
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(!serialized.contains("\"session_id\""));
        assert!(!serialized.contains("\"profile_id\""));
        assert!(!serialized.contains("\"error\""));
        assert!(body.get("sign").is_none());
    }

    #[test]
    fn feishu_timestamp_is_normalized_for_display() {
        assert_eq!(
            format_feishu_timestamp("2026-07-21 20:30:00 +08:00"),
            "2026-07-21 20:30:00"
        );
        assert_eq!(
            format_feishu_timestamp("2026-07-21T20:30:00+08:00"),
            "2026-07-21 20:30:00"
        );
        let legacy = format_feishu_timestamp("unix-ms:1784646600000");
        assert_eq!(legacy.len(), 19);
        assert!(!legacy.contains("unix-ms"));
        assert_eq!(&legacy[4..5], "-");
        assert_eq!(&legacy[7..8], "-");
        assert_eq!(&legacy[10..11], " ");
    }

    #[test]
    fn feishu_waiting_and_failure_cards_have_distinct_titles_and_colors() {
        let waiting = WebhookEvent::new("session.waiting", "s1", "p1", "Codex", 0, None);
        let waiting_body = feishu_body(&waiting).unwrap();
        assert_eq!(
            waiting_body["card"]["header"]["title"]["content"],
            "Codex会话等待介入"
        );
        assert_eq!(waiting_body["card"]["header"]["template"], "orange");

        let failed = WebhookEvent::new("session.failed", "s1", "p1", "Codex", 500, None);
        let failed_body = feishu_body(&failed).unwrap();
        assert_eq!(
            failed_body["card"]["header"]["title"]["content"],
            "Codex会话失败"
        );
        assert_eq!(failed_body["card"]["header"]["template"], "red");
    }

    #[test]
    fn feishu_response_checks_business_error_code() {
        assert!(feishu_response_error(r#"{"code":0,"msg":"success"}"#).is_none());
        assert!(
            feishu_response_error(r#"{"code":19021,"msg":"sign fail"}"#)
                .unwrap()
                .contains("19021")
        );
    }

    #[tokio::test]
    async fn webhook_test_requires_a_configured_url() {
        let dispatcher = WebhookDispatcher::new(WebhookConfig::default()).unwrap();
        assert!(
            dispatcher
                .test()
                .await
                .unwrap_err()
                .to_string()
                .contains("Webhook")
        );
    }
}
