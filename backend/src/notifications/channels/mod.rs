mod feishu;
mod telegram;
mod wechat_claw;
mod wecom;

use anyhow::Result;
use reqwest::{Client, RequestBuilder};

use super::{NotificationChannelConfig, NotificationChannelKind, NotificationEvent};

pub(super) trait NotificationChannelAdapter: Send + Sync {
    fn display_name(&self) -> &'static str;
    fn configuration_error(&self) -> Option<&'static str>;
    fn settle_on_success_status_error(&self, _body: &str) -> bool {
        false
    }
    fn retry_with_fresh_context_on_success_status_error(&self, _body: &str) -> bool {
        false
    }
    fn pause_on_stale_token_success_status_error(&self, _body: &str) -> bool {
        false
    }
    fn build_request(&self, client: &Client, event: &NotificationEvent) -> Result<RequestBuilder>;
    fn validate_response(&self, body: &str) -> std::result::Result<(), String>;
    fn sanitize_error(&self, error: &str) -> String;
}

pub(super) fn bounded_remote_message(message: &str) -> String {
    let normalized = message.split_whitespace().collect::<Vec<_>>().join(" ");
    let truncated = normalized.chars().take(200).collect::<String>();
    if truncated.is_empty() {
        "未知错误".to_string()
    } else {
        truncated
    }
}

pub(super) fn adapter_for(
    config: &NotificationChannelConfig,
) -> Box<dyn NotificationChannelAdapter + '_> {
    match config.kind {
        NotificationChannelKind::Feishu => Box::new(feishu::FeishuChannel::new(config)),
        NotificationChannelKind::Wecom => Box::new(wecom::WecomChannel::new(config)),
        NotificationChannelKind::Telegram => Box::new(telegram::TelegramChannel::new(config)),
        NotificationChannelKind::WechatClaw => {
            Box::new(wechat_claw::WechatClawChannel::new(config))
        }
    }
}
