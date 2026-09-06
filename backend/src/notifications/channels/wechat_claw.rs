use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use reqwest::{Client, RequestBuilder};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{NotificationChannelAdapter, bounded_remote_message};
use crate::notifications::formatting::{format_duration, format_timestamp, plain_text_value};
use crate::notifications::ilink;
use crate::notifications::{
    NotificationChannelConfig, NotificationChannelSessionStatus, NotificationEvent,
};

const MAX_WECHAT_TEXT_CHARS: usize = 1_800;

pub(super) struct WechatClawChannel<'a> {
    config: &'a NotificationChannelConfig,
}

impl<'a> WechatClawChannel<'a> {
    pub(super) fn new(config: &'a NotificationChannelConfig) -> Self {
        Self { config }
    }

    fn ilink_post(&self, client: &Client, endpoint: &str, body: &Value) -> Result<RequestBuilder> {
        let base_url = self
            .config
            .wechat_claw_base_url()
            .map_err(anyhow::Error::msg)?;
        let endpoint = base_url.join(endpoint).map_err(anyhow::Error::from)?;
        Ok(client
            .post(endpoint)
            .header("Content-Type", "application/json")
            .headers(ilink::headers(Some(self.config.bot_token.trim())))
            .json(&body))
    }
}

impl NotificationChannelAdapter for WechatClawChannel<'_> {
    fn display_name(&self) -> &'static str {
        "微信 ClawBot"
    }

    fn configuration_error(&self) -> Option<&'static str> {
        if self.config.session_status == NotificationChannelSessionStatus::Expired {
            Some("微信 ClawBot 登录已失效，请重新扫码")
        } else if self.config.bot_token.trim().is_empty() {
            Some("请先通过扫码登录微信 ClawBot")
        } else if self.config.context_token.trim().is_empty() {
            Some("请先在微信中向 ClawBot 发送一条消息完成激活")
        } else if self.config.chat_id.trim().is_empty() {
            Some("请先填写接收通知的 iLink 用户 ID")
        } else {
            self.config.wechat_claw_base_url().err()
        }
    }

    fn build_request(&self, client: &Client, event: &NotificationEvent) -> Result<RequestBuilder> {
        self.ilink_post(
            client,
            "ilink/bot/sendmessage",
            &wechat_claw_body(event, &self.config.chat_id, &self.config.context_token),
        )
    }

    fn settle_on_success_status_error(&self, body: &str) -> bool {
        !wechat_claw_response_has_explicit_failure(body)
    }

    fn retry_with_fresh_context_on_success_status_error(&self, body: &str) -> bool {
        wechat_claw_response_needs_context_refresh(body)
    }

    fn pause_on_stale_token_success_status_error(&self, body: &str) -> bool {
        wechat_claw_response_has_code(body, -14)
    }

    fn validate_response(&self, body: &str) -> std::result::Result<(), String> {
        validate_wechat_claw_response(body)
    }

    fn sanitize_error(&self, error: &str) -> String {
        let token = self.config.bot_token.trim();
        let mut sanitized = if token.is_empty() {
            error.to_string()
        } else {
            error.replace(token, "***")
        };
        let context_token = self.config.context_token.trim();
        if !context_token.is_empty() {
            sanitized = sanitized.replace(context_token, "***");
        }
        let url = self.config.url.trim();
        if !url.is_empty() {
            sanitized = sanitized.replace(url, "***");
            if let Ok(normalized) = reqwest::Url::parse(url) {
                sanitized = sanitized.replace(normalized.as_str(), "***");
            }
        }
        sanitized
    }
}

fn wechat_claw_body(event: &NotificationEvent, recipient_id: &str, context_token: &str) -> Value {
    json!({
        "msg": {
            "from_user_id": "",
            "to_user_id": recipient_id.trim(),
            "context_token": context_token.trim(),
            "client_id": wechat_claw_client_id(),
            "message_type": 2,
            "message_state": 2,
            "item_list": [{
                "type": 1,
                "text_item": { "text": wechat_claw_text(event) },
            }],
        },
        "base_info": ilink::base_info(),
    })
}

fn wechat_claw_client_id() -> String {
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let random = Uuid::new_v4().simple().to_string();
    format!("codey:{timestamp_ms}-{}", &random[..8])
}

fn wechat_claw_text(event: &NotificationEvent) -> String {
    let title = match event.event.as_str() {
        "session.completed" => "✅ Codex 会话完成",
        "session.failed" => "❌ Codex 会话失败",
        "session.waiting" => "⏳ Codex 会话等待介入",
        "codey.test" => "🔔 Codey 通知测试",
        _ => "🔔 Codex 会话通知",
    };
    let session_name = plain_text_value(&event.session_name, "未命名会话");
    let model = plain_text_value(&event.model, "Codex");
    let reasoning_effort = plain_text_value(&event.reasoning_effort, "默认");
    let sent_at = plain_text_value(&format_timestamp(&event.timestamp), "未知");
    truncate_text(&format!(
        "{title}\n\n会话标题：{session_name}\n使用模型：{model}\n推理深度：{reasoning_effort}\n发送时间：{sent_at}\n耗时：{}",
        format_duration(event.duration_ms)
    ))
}

fn truncate_text(text: &str) -> String {
    let mut chars = text.chars();
    let prefix = chars
        .by_ref()
        .take(MAX_WECHAT_TEXT_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!(
            "{}…",
            prefix
                .chars()
                .take(MAX_WECHAT_TEXT_CHARS - 1)
                .collect::<String>()
        )
    } else {
        prefix
    }
}

fn validate_wechat_claw_response(body: &str) -> std::result::Result<(), String> {
    if body.trim().is_empty() {
        return Ok(());
    }
    let value = serde_json::from_str::<Value>(body)
        .map_err(|_| "微信 ClawBot 返回了无法解析的响应".to_string())?;
    let Some(payload) = value.as_object() else {
        return Err("微信 ClawBot 返回了无效的响应结构".to_string());
    };
    if payload.is_empty() {
        return Ok(());
    }
    let message = value
        .get("errmsg")
        .or_else(|| value.get("err_msg"))
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("未知错误");
    for key in ["ret", "errcode", "err_code"] {
        let Some(raw_result) = value.get(key) else {
            continue;
        };
        let result = wechat_claw_response_code(raw_result)
            .ok_or_else(|| "微信 ClawBot 返回了无效的业务状态".to_string())?;
        if result != 0 {
            if result == -2 && message.trim().eq_ignore_ascii_case("prepare failed") {
                return Err(
                    "微信 ClawBot 暂时无法准备投递。请重新扫码，并按提示先在微信中向 ClawBot 发送一条消息完成激活；若仍失败，请稍后重试".to_string(),
                );
            }
            return Err(format!(
                "微信 ClawBot 返回错误 {result}：{}",
                bounded_remote_message(message)
            ));
        }
    }
    Ok(())
}

fn wechat_claw_response_has_explicit_failure(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    ["ret", "errcode", "err_code"]
        .into_iter()
        .filter_map(|key| value.get(key))
        .filter_map(wechat_claw_response_code)
        .any(|result| result != 0)
}

fn wechat_claw_response_needs_context_refresh(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    let message = value
        .get("errmsg")
        .or_else(|| value.get("err_msg"))
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    ["ret", "errcode", "err_code"]
        .into_iter()
        .filter_map(|key| value.get(key))
        .filter_map(wechat_claw_response_code)
        .any(|result| result == -2 && message.eq_ignore_ascii_case("prepare failed"))
}

fn wechat_claw_response_has_code(body: &str, expected: i64) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    ["ret", "errcode", "err_code"]
        .into_iter()
        .filter_map(|key| value.get(key))
        .filter_map(wechat_claw_response_code)
        .any(|result| result == expected)
}

fn wechat_claw_response_code(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str()?.trim().parse::<i64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::{NotificationChannelKind, NotificationEvent};

    fn configured_channel() -> NotificationChannelConfig {
        NotificationChannelConfig {
            kind: NotificationChannelKind::WechatClaw,
            url: "https://ilinkai.weixin.qq.com".to_string(),
            bot_token: "ilink-secret-token".to_string(),
            context_token: "context-secret-token".to_string(),
            chat_id: "recipient@im.wechat".to_string(),
            ..NotificationChannelConfig::default()
        }
    }

    #[test]
    fn request_uses_the_official_ilink_schema_and_protective_headers() {
        let config = configured_channel();
        let event = NotificationEvent::new("codey.test", "s1", "p1", "Codex", 0, None);
        let request = WechatClawChannel::new(&config)
            .build_request(&Client::new(), &event)
            .unwrap()
            .build()
            .unwrap();

        assert_eq!(
            request.url().as_str(),
            "https://ilinkai.weixin.qq.com/ilink/bot/sendmessage"
        );
        assert_eq!(request.headers()["authorizationtype"], "ilink_bot_token");
        assert_eq!(
            request.headers()["authorization"],
            "Bearer ilink-secret-token"
        );
        assert!(request.headers().contains_key("x-wechat-uin"));
        let body = request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
            .unwrap();
        assert_eq!(body["msg"]["to_user_id"], "recipient@im.wechat");
        assert_eq!(body["msg"]["context_token"], "context-secret-token");
        assert_eq!(body["msg"]["message_type"], 2);
        assert_eq!(body["msg"]["item_list"][0]["type"], 1);
        assert!(
            body["msg"]["client_id"]
                .as_str()
                .is_some_and(|value| value.starts_with("codey:"))
        );
        assert!(
            body["msg"]["item_list"][0]["text_item"]["text"]
                .as_str()
                .unwrap()
                .contains("Codey 通知测试")
        );
    }

    #[test]
    fn clawbot_settles_an_ambiguous_success_response() {
        let config = configured_channel();
        let channel = WechatClawChannel::new(&config);

        assert!(channel.settle_on_success_status_error("not json"));
        assert!(channel.settle_on_success_status_error("null"));
        assert!(!channel.settle_on_success_status_error(r#"{"ret":-2,"errmsg":"prepare failed"}"#));
        assert!(!channel.settle_on_success_status_error(r#"{"ret":-14,"errmsg":"token expired"}"#));
        assert!(channel.retry_with_fresh_context_on_success_status_error(
            r#"{"ret":-2,"errmsg":"prepare failed"}"#
        ));
        assert!(!channel.retry_with_fresh_context_on_success_status_error(
            r#"{"ret":-14,"errmsg":"token expired"}"#
        ));
        assert!(
            channel.pause_on_stale_token_success_status_error(
                r#"{"ret":-14,"errmsg":"token expired"}"#
            )
        );
        assert!(
            !channel.pause_on_stale_token_success_status_error(
                r#"{"ret":-2,"errmsg":"prepare failed"}"#
            )
        );
    }

    #[test]
    fn channel_is_not_ready_until_activation_context_is_available() {
        let mut config = configured_channel();
        config.context_token.clear();

        assert_eq!(
            WechatClawChannel::new(&config).configuration_error(),
            Some("请先在微信中向 ClawBot 发送一条消息完成激活")
        );
        assert!(!config.is_configured());
    }

    #[test]
    fn expired_session_reports_rebind_instead_of_missing_secret() {
        let mut config = configured_channel();
        config.session_status = NotificationChannelSessionStatus::Expired;
        config.bot_token.clear();
        config.context_token.clear();

        assert_eq!(
            WechatClawChannel::new(&config).configuration_error(),
            Some("微信 ClawBot 登录已失效，请重新扫码")
        );
        assert!(!config.is_configured());
    }

    #[test]
    fn response_accepts_http_success_without_a_redundant_result_field() {
        assert!(validate_wechat_claw_response("").is_ok());
        assert!(validate_wechat_claw_response("{}").is_ok());
        assert!(validate_wechat_claw_response("null").is_err());
        assert!(validate_wechat_claw_response(r#"{"ret":0}"#).is_ok());
        assert!(validate_wechat_claw_response(r#"{"ret":0,"errcode":0}"#).is_ok());
        assert!(validate_wechat_claw_response(r#"{"ret":0,"errcode":null}"#).is_err());
        assert!(validate_wechat_claw_response(r#"{"ret":0,"errcode":-1}"#).is_err());
        assert!(validate_wechat_claw_response(r#"{"errcode":-1}"#).is_err());
        assert!(
            validate_wechat_claw_response(r#"{"err_code":"-2","err_msg":"prepare failed"}"#)
                .is_err()
        );
        assert!(
            validate_wechat_claw_response(r#"{"ret":-14,"errmsg":"token expired"}"#)
                .unwrap_err()
                .contains("-14")
        );
        assert!(
            validate_wechat_claw_response(r#"{"ret":-2,"errmsg":"prepare failed"}"#)
                .unwrap_err()
                .contains("重新扫码")
        );
        assert!(validate_wechat_claw_response("not json").is_err());
    }

    #[test]
    fn each_outbound_message_uses_a_fresh_client_id() {
        let event = NotificationEvent::new("session.completed", "s1", "p1", "Codex", 0, None);
        let first = wechat_claw_body(&event, "recipient@im.wechat", "context-token");
        let second = wechat_claw_body(&event, "recipient@im.wechat", "context-token");

        assert_ne!(first["msg"]["client_id"], second["msg"]["client_id"]);
    }

    #[test]
    fn errors_never_expose_the_login_token_or_base_url() {
        let config = configured_channel();
        let channel = WechatClawChannel::new(&config);
        let error = channel.sanitize_error(
            "request https://ilinkai.weixin.qq.com failed with ilink-secret-token and context-secret-token",
        );
        assert!(!error.contains("ilink-secret-token"));
        assert!(!error.contains("context-secret-token"));
        assert!(!error.contains("ilinkai.weixin.qq.com"));
    }

    #[test]
    fn notification_text_is_bounded_to_one_message() {
        let event = NotificationEvent::new("session.completed", "s1", "p1", "Codex", 0, None)
            .with_session_name("测".repeat(2_000));
        assert!(wechat_claw_text(&event).chars().count() <= MAX_WECHAT_TEXT_CHARS);
    }

    #[test]
    fn empty_token_does_not_expand_error_text_during_sanitization() {
        let mut config = configured_channel();
        config.bot_token.clear();
        let error = WechatClawChannel::new(&config).sanitize_error("normal error");

        assert_eq!(error, "normal error");
    }
}
