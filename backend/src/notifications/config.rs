use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_NOTIFICATION_CHANNELS: usize = 32;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum NotificationChannelKind {
    #[default]
    Feishu,
    Wecom,
    Telegram,
    WechatClaw,
    Ntfy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum NotificationChannelSessionStatus {
    #[default]
    Active,
    Expired,
}

impl NotificationChannelSessionStatus {
    fn is_active(&self) -> bool {
        *self == Self::Active
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NotificationChannelConfig {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub kind: NotificationChannelKind,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub url_configured: bool,
    #[serde(default, skip_serializing)]
    pub clear_url: bool,
    #[serde(default)]
    pub bot_token: String,
    #[serde(default)]
    pub bot_token_configured: bool,
    #[serde(default, skip_serializing)]
    pub clear_bot_token: bool,
    #[serde(default)]
    pub context_token: String,
    #[serde(default)]
    pub context_token_configured: bool,
    #[serde(default, skip_serializing)]
    pub clear_context_token: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub get_updates_buf: String,
    #[serde(default)]
    pub chat_id: String,
    #[serde(
        default,
        skip_serializing_if = "NotificationChannelSessionStatus::is_active"
    )]
    pub session_status: NotificationChannelSessionStatus,
    #[cfg(test)]
    #[serde(skip)]
    pub allow_insecure_test_url: bool,
}

impl Default for NotificationChannelConfig {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            kind: NotificationChannelKind::Feishu,
            enabled: true,
            url: String::new(),
            url_configured: false,
            clear_url: false,
            bot_token: String::new(),
            bot_token_configured: false,
            clear_bot_token: false,
            context_token: String::new(),
            context_token_configured: false,
            clear_context_token: false,
            get_updates_buf: String::new(),
            chat_id: String::new(),
            session_status: NotificationChannelSessionStatus::Active,
            #[cfg(test)]
            allow_insecure_test_url: false,
        }
    }
}

impl NotificationChannelConfig {
    pub fn is_configured(&self) -> bool {
        match self.kind {
            NotificationChannelKind::Feishu => !self.url.trim().is_empty(),
            NotificationChannelKind::Wecom => !self.url.trim().is_empty(),
            NotificationChannelKind::Telegram => {
                !self.bot_token.trim().is_empty() && !self.chat_id.trim().is_empty()
            }
            NotificationChannelKind::WechatClaw => {
                self.session_status != NotificationChannelSessionStatus::Expired
                    && !self.bot_token.trim().is_empty()
                    && !self.context_token.trim().is_empty()
                    && !self.chat_id.trim().is_empty()
                    && self.wechat_claw_base_url().is_ok()
            }
            NotificationChannelKind::Ntfy => {
                self.ntfy_base_url().is_ok() && self.ntfy_topic().is_ok()
            }
        }
    }

    pub(crate) fn feishu_webhook_url(&self) -> Result<reqwest::Url, &'static str> {
        const INVALID_URL: &str =
            "飞书机器人 Webhook 必须使用 HTTPS 地址和 /open-apis/bot/v2/hook/... 路径";
        let value = self.url.trim();
        if value.is_empty() {
            return Err("请先填写飞书机器人 Webhook 地址");
        }
        let url = reqwest::Url::parse(value).map_err(|_| INVALID_URL)?;
        #[cfg(test)]
        if self.allow_insecure_test_url {
            return Ok(url);
        }
        let hook = url
            .path()
            .strip_prefix("/open-apis/bot/v2/hook/")
            .filter(|hook| !hook.is_empty() && !hook.contains('/'));
        if url.scheme() != "https"
            || url.host_str().is_none()
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || hook.is_none()
        {
            return Err(INVALID_URL);
        }
        Ok(url)
    }

    pub(crate) fn wecom_webhook_url(&self) -> Result<reqwest::Url, &'static str> {
        const INVALID_URL: &str = "企业微信机器人 Webhook 必须使用 https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=... 地址";
        let value = self.url.trim();
        if value.is_empty() {
            return Err("请先填写企业微信机器人 Webhook 地址");
        }
        let url = reqwest::Url::parse(value).map_err(|_| INVALID_URL)?;
        #[cfg(test)]
        if self.allow_insecure_test_url {
            return Ok(url);
        }
        let query = url.query_pairs().collect::<Vec<_>>();
        let valid_key = query.len() == 1 && query[0].0 == "key" && !query[0].1.trim().is_empty();
        if url.scheme() != "https"
            || url.host_str() != Some("qyapi.weixin.qq.com")
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/cgi-bin/webhook/send"
            || url.fragment().is_some()
            || !valid_key
        {
            return Err(INVALID_URL);
        }
        Ok(url)
    }

    pub(crate) fn wechat_claw_base_url(&self) -> Result<reqwest::Url, &'static str> {
        const INVALID_URL: &str = "微信 ClawBot 服务地址必须是腾讯 iLink 的 HTTPS 根地址";
        let value = self.url.trim();
        if value.is_empty() {
            return Err("请先通过扫码登录微信 ClawBot");
        }
        let url = reqwest::Url::parse(value).map_err(|_| INVALID_URL)?;
        #[cfg(test)]
        if self.allow_insecure_test_url {
            return Ok(url);
        }
        let host = url.host_str().unwrap_or_default();
        let official_host = host == "ilinkai.weixin.qq.com" || host.ends_with(".weixin.qq.com");
        if url.scheme() != "https"
            || !official_host
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
            || !matches!(url.path(), "" | "/")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(INVALID_URL);
        }
        Ok(url)
    }

    /// ntfy 服务器地址，请求发往根路径，主题放在 JSON 请求体中；允许自建服务，
    /// 因此只约束协议、主机和路径，不限定域名与端口。
    pub(crate) fn ntfy_base_url(&self) -> Result<reqwest::Url, &'static str> {
        const INVALID_URL: &str = "ntfy 服务器地址必须是 HTTPS 根地址，例如 https://ntfy.sh";
        let value = self.url.trim();
        if value.is_empty() {
            return Err("请先填写 ntfy 服务器地址");
        }
        let url = reqwest::Url::parse(value).map_err(|_| INVALID_URL)?;
        #[cfg(test)]
        if self.allow_insecure_test_url {
            return Ok(url);
        }
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || !matches!(url.path(), "" | "/")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(INVALID_URL);
        }
        Ok(url)
    }

    /// ntfy 主题沿用聊天渠道的接收方字段，取值限制与 ntfy 服务端一致。
    pub(crate) fn ntfy_topic(&self) -> Result<&str, &'static str> {
        let topic = self.chat_id.trim();
        if topic.is_empty() {
            return Err("请先填写 ntfy 主题");
        }
        if topic.len() > 64
            || !topic.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
        {
            return Err("ntfy 主题只能包含字母、数字、下划线和连字符，长度不超过 64 个字符");
        }
        Ok(topic)
    }
}

/// Notification settings retain the historic `webhook` wire name in
/// `CodeyConfig` so existing installations and renderer calls remain valid.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct WebhookConfig {
    #[serde(default)]
    pub channels: Vec<NotificationChannelConfig>,
}

impl WebhookConfig {
    pub(crate) fn normalize(&mut self) {
        let mut ids = BTreeSet::new();
        for channel in &mut self.channels {
            channel.id = channel.id.trim().to_string();
            if channel.id.is_empty() || !ids.insert(channel.id.clone()) {
                channel.id = Uuid::new_v4().to_string();
                ids.insert(channel.id.clone());
            }
            channel.url = channel.url.trim().to_string();
            channel.url_configured = !channel.url.is_empty();
            channel.clear_url = false;
            channel.bot_token = channel.bot_token.trim().to_string();
            channel.context_token = channel.context_token.trim().to_string();
            channel.get_updates_buf = channel.get_updates_buf.trim().to_string();
            channel.chat_id = channel.chat_id.trim().to_string();
            channel.bot_token_configured = !channel.bot_token.is_empty();
            channel.clear_bot_token = false;
            channel.context_token_configured = !channel.context_token.is_empty();
            channel.clear_context_token = false;
            if channel.kind != NotificationChannelKind::WechatClaw {
                channel.session_status = NotificationChannelSessionStatus::Active;
            }
        }
    }

    pub fn enabled_channel_count(&self) -> usize {
        self.channels
            .iter()
            .filter(|channel| channel.enabled && channel.is_configured())
            .count()
    }

    pub fn has_enabled_channel(&self) -> bool {
        self.enabled_channel_count() > 0
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.channels.len() > MAX_NOTIFICATION_CHANNELS {
            return Err(format!(
                "通知渠道最多只能配置 {MAX_NOTIFICATION_CHANNELS} 个"
            ));
        }
        for channel in &self.channels {
            if channel.url.trim().is_empty() {
                continue;
            }
            match channel.kind {
                NotificationChannelKind::Feishu => {
                    channel.feishu_webhook_url().map_err(ToString::to_string)?;
                }
                NotificationChannelKind::Wecom => {
                    channel.wecom_webhook_url().map_err(ToString::to_string)?;
                }
                NotificationChannelKind::Telegram => {}
                NotificationChannelKind::WechatClaw => {
                    if !channel.url.trim().is_empty() {
                        channel
                            .wechat_claw_base_url()
                            .map_err(ToString::to_string)?;
                    }
                }
                NotificationChannelKind::Ntfy => {
                    channel.ntfy_base_url().map_err(ToString::to_string)?;
                    if !channel.chat_id.trim().is_empty() {
                        channel.ntfy_topic().map_err(ToString::to_string)?;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn merge_redacted_secrets(&mut self, previous: &Self) {
        for channel in &mut self.channels {
            if matches!(
                channel.kind,
                NotificationChannelKind::Feishu
                    | NotificationChannelKind::Wecom
                    | NotificationChannelKind::Ntfy
            ) {
                restore_redacted_url(channel, previous);
            }
            match channel.kind {
                NotificationChannelKind::Telegram | NotificationChannelKind::Ntfy => {
                    let kind = channel.kind;
                    if channel.clear_bot_token {
                        channel.bot_token.clear();
                        channel.bot_token_configured = false;
                        continue;
                    }
                    if !channel.bot_token.trim().is_empty() || !channel.bot_token_configured {
                        continue;
                    }
                    if let Some(existing) = previous
                        .channels
                        .iter()
                        .find(|existing| existing.id == channel.id && existing.kind == kind)
                    {
                        channel.bot_token = existing.bot_token.clone();
                    }
                }
                NotificationChannelKind::WechatClaw => {
                    let existing = previous.channels.iter().find(|existing| {
                        existing.id == channel.id
                            && existing.kind == NotificationChannelKind::WechatClaw
                    });
                    let has_fresh_binding = !channel.bot_token.trim().is_empty()
                        && !channel.context_token.trim().is_empty()
                        && !channel.chat_id.trim().is_empty();
                    let preserve_existing_expired_status = existing.is_some_and(|existing| {
                        existing.session_status == NotificationChannelSessionStatus::Expired
                    }) && !channel.clear_url
                        && !channel.clear_bot_token
                        && !channel.clear_context_token
                        && !has_fresh_binding;

                    if channel.clear_url {
                        channel.url.clear();
                        channel.url_configured = false;
                    } else if channel.url.trim().is_empty()
                        && channel.url_configured
                        && let Some(existing) = existing
                    {
                        channel.url = existing.url.clone();
                    }

                    if channel.clear_context_token {
                        channel.context_token.clear();
                        channel.context_token_configured = false;
                    } else if channel.context_token.trim().is_empty()
                        && channel.context_token_configured
                        && let Some(existing) = existing
                    {
                        channel.context_token = existing.context_token.clone();
                    }

                    if channel.clear_bot_token {
                        channel.bot_token.clear();
                        channel.bot_token_configured = false;
                    } else if channel.bot_token.trim().is_empty()
                        && channel.bot_token_configured
                        && let Some(existing) = existing
                    {
                        channel.bot_token = existing.bot_token.clone();
                    }

                    if channel.clear_bot_token || channel.clear_context_token {
                        channel.get_updates_buf.clear();
                        channel.session_status = NotificationChannelSessionStatus::Active;
                        continue;
                    }
                    if channel.get_updates_buf.trim().is_empty()
                        && let Some(existing) = existing
                        && same_wechat_claw_binding(channel, existing)
                    {
                        channel.get_updates_buf = existing.get_updates_buf.clone();
                    }
                    if preserve_existing_expired_status {
                        channel.session_status = NotificationChannelSessionStatus::Expired;
                    } else if !channel.bot_token.trim().is_empty()
                        && !channel.context_token.trim().is_empty()
                        && !channel.chat_id.trim().is_empty()
                    {
                        channel.session_status = NotificationChannelSessionStatus::Active;
                    }
                }
                NotificationChannelKind::Feishu | NotificationChannelKind::Wecom => {}
            }
        }
    }
}

fn restore_redacted_url(channel: &mut NotificationChannelConfig, previous: &WebhookConfig) {
    if channel.clear_url {
        channel.url.clear();
        channel.url_configured = false;
        return;
    }
    if !channel.url.trim().is_empty() || !channel.url_configured {
        return;
    }
    let kind = channel.kind;
    if let Some(existing) = previous
        .channels
        .iter()
        .find(|existing| existing.id == channel.id && existing.kind == kind)
    {
        channel.url = existing.url.clone();
    }
}

fn same_wechat_claw_binding(
    left: &NotificationChannelConfig,
    right: &NotificationChannelConfig,
) -> bool {
    left.wechat_claw_base_url().ok() == right.wechat_claw_base_url().ok()
        && left.bot_token.trim() == right.bot_token.trim()
        && left.context_token.trim() == right.context_token.trim()
        && left.chat_id.trim() == right.chat_id.trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_multiple_channel_kinds() {
        let mut config = serde_json::from_str::<WebhookConfig>(
            r#"{
                "channels":[
                    {"id":"feishu-1","kind":"feishu","enabled":true,"url":"https://open.feishu.cn/example"},
                    {"id":"wecom-1","kind":"wecom","enabled":true,"url":"https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=preview"},
                    {"id":"telegram-1","kind":"telegram","enabled":true,"botToken":"123:token","chatId":"-100123"}
                ]
            }"#,
        )
        .unwrap();
        config.normalize();

        assert_eq!(config.channels.len(), 3);
        assert_eq!(config.enabled_channel_count(), 3);
        assert!(config.has_enabled_channel());
        assert!(config.channels[0].url_configured);
        assert_eq!(config.channels[1].kind, NotificationChannelKind::Wecom);
        assert!(config.channels[1].url_configured);
        assert_eq!(config.channels[2].kind, NotificationChannelKind::Telegram);
        assert!(config.channels[2].bot_token_configured);
    }

    #[test]
    fn rejects_channel_lists_above_the_resource_limit() {
        let config = WebhookConfig {
            channels: (0..=MAX_NOTIFICATION_CHANNELS)
                .map(|_| NotificationChannelConfig::default())
                .collect(),
        };

        assert_eq!(
            config.validate().unwrap_err(),
            format!("通知渠道最多只能配置 {MAX_NOTIFICATION_CHANNELS} 个")
        );
    }

    #[test]
    fn feishu_webhooks_accept_custom_https_hosts() {
        for accepted in [
            "https://open.feishu.cn/open-apis/bot/v2/hook/secret",
            "https://open.larksuite.com/open-apis/bot/v2/hook/secret",
            "https://open.feishu.cn:443/open-apis/bot/v2/hook/secret",
            "https://feishu.corp.example/open-apis/bot/v2/hook/secret",
        ] {
            let config = NotificationChannelConfig {
                url: accepted.to_string(),
                ..NotificationChannelConfig::default()
            };
            assert!(config.feishu_webhook_url().is_ok(), "{accepted}");
        }

        for rejected in [
            "http://open.feishu.cn/open-apis/bot/v2/hook/secret",
            "https://feishu.corp.example:8443/open-apis/bot/v2/hook/secret",
            "https://open.feishu.cn/other/path",
            "https://open.feishu.cn/open-apis/bot/v2/hook/secret?redirect=1",
            "https://user@open.feishu.cn/open-apis/bot/v2/hook/secret",
        ] {
            let config = NotificationChannelConfig {
                url: rejected.to_string(),
                ..NotificationChannelConfig::default()
            };
            assert!(config.feishu_webhook_url().is_err(), "{rejected}");
        }
    }

    #[test]
    fn wecom_webhooks_require_the_official_robot_endpoint() {
        for accepted in [
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=secret",
            "https://qyapi.weixin.qq.com:443/cgi-bin/webhook/send?key=secret-value",
        ] {
            let config = NotificationChannelConfig {
                kind: NotificationChannelKind::Wecom,
                url: accepted.to_string(),
                ..NotificationChannelConfig::default()
            };
            assert!(config.wecom_webhook_url().is_ok(), "{accepted}");
        }

        for rejected in [
            "http://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=secret",
            "https://qyapi.weixin.qq.com:8443/cgi-bin/webhook/send?key=secret",
            "https://example.com/cgi-bin/webhook/send?key=secret",
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/upload_media?key=secret",
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send",
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=",
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=secret&debug=1",
            "https://user@qyapi.weixin.qq.com/cgi-bin/webhook/send?key=secret",
        ] {
            let config = NotificationChannelConfig {
                kind: NotificationChannelKind::Wecom,
                url: rejected.to_string(),
                ..NotificationChannelConfig::default()
            };
            assert!(config.wecom_webhook_url().is_err(), "{rejected}");
        }
    }

    #[test]
    fn redacted_feishu_url_is_restored_when_other_settings_are_saved() {
        let previous = WebhookConfig {
            channels: vec![NotificationChannelConfig {
                id: "feishu-1".to_string(),
                kind: NotificationChannelKind::Feishu,
                enabled: true,
                url: "https://open.feishu.cn/open-apis/bot/v2/hook/secret".to_string(),
                url_configured: true,
                ..NotificationChannelConfig::default()
            }],
        };
        let mut incoming = previous.clone();
        incoming.channels[0].url.clear();
        incoming.merge_redacted_secrets(&previous);

        assert_eq!(
            incoming.channels[0].url,
            "https://open.feishu.cn/open-apis/bot/v2/hook/secret"
        );
    }

    #[test]
    fn explicit_feishu_url_clear_does_not_restore_the_previous_secret() {
        let previous = WebhookConfig {
            channels: vec![NotificationChannelConfig {
                id: "feishu-1".to_string(),
                kind: NotificationChannelKind::Feishu,
                url: "https://open.feishu.cn/open-apis/bot/v2/hook/secret".to_string(),
                url_configured: true,
                ..NotificationChannelConfig::default()
            }],
        };
        let mut incoming = previous.clone();
        incoming.channels[0].url.clear();
        incoming.channels[0].clear_url = true;
        incoming.merge_redacted_secrets(&previous);

        assert!(incoming.channels[0].url.is_empty());
        assert!(!incoming.channels[0].url_configured);
        assert!(
            serde_json::to_value(&incoming).unwrap()["channels"][0]
                .get("clearUrl")
                .is_none()
        );
    }

    #[test]
    fn redacted_wecom_url_is_restored_unless_explicitly_cleared() {
        let previous = WebhookConfig {
            channels: vec![NotificationChannelConfig {
                id: "wecom-1".to_string(),
                kind: NotificationChannelKind::Wecom,
                url: "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=secret".to_string(),
                url_configured: true,
                ..NotificationChannelConfig::default()
            }],
        };
        let mut incoming = previous.clone();
        incoming.channels[0].url.clear();
        incoming.merge_redacted_secrets(&previous);
        assert_eq!(incoming.channels[0].url, previous.channels[0].url);

        incoming.channels[0].url.clear();
        incoming.channels[0].clear_url = true;
        incoming.merge_redacted_secrets(&previous);
        assert!(incoming.channels[0].url.is_empty());
        assert!(!incoming.channels[0].url_configured);
    }

    #[test]
    fn redacted_telegram_token_is_restored_when_other_settings_are_saved() {
        let previous = WebhookConfig {
            channels: vec![NotificationChannelConfig {
                id: "telegram-1".to_string(),
                kind: NotificationChannelKind::Telegram,
                enabled: true,
                bot_token: "123:secret".to_string(),
                bot_token_configured: true,
                chat_id: "-100123".to_string(),
                ..NotificationChannelConfig::default()
            }],
        };
        let mut incoming = previous.clone();
        incoming.channels[0].bot_token.clear();
        incoming.merge_redacted_secrets(&previous);

        assert_eq!(incoming.channels[0].bot_token, "123:secret");
    }

    #[test]
    fn explicit_telegram_token_clear_does_not_restore_the_previous_secret() {
        let previous = WebhookConfig {
            channels: vec![NotificationChannelConfig {
                id: "telegram-1".to_string(),
                kind: NotificationChannelKind::Telegram,
                bot_token: "123:secret".to_string(),
                bot_token_configured: true,
                chat_id: "-100123".to_string(),
                ..NotificationChannelConfig::default()
            }],
        };
        let mut incoming = previous.clone();
        incoming.channels[0].bot_token.clear();
        incoming.channels[0].clear_bot_token = true;
        incoming.merge_redacted_secrets(&previous);

        assert!(incoming.channels[0].bot_token.is_empty());
        assert!(!incoming.channels[0].bot_token_configured);
        assert!(
            serde_json::to_value(&incoming).unwrap()["channels"][0]
                .get("clearBotToken")
                .is_none()
        );
    }

    #[test]
    fn wechat_claw_requires_an_official_ilink_https_root_url() {
        for accepted in [
            "https://ilinkai.weixin.qq.com",
            "https://region.weixin.qq.com/",
        ] {
            let config = NotificationChannelConfig {
                kind: NotificationChannelKind::WechatClaw,
                url: accepted.to_string(),
                ..NotificationChannelConfig::default()
            };
            assert!(config.wechat_claw_base_url().is_ok(), "{accepted}");
        }

        for rejected in [
            "http://ilinkai.weixin.qq.com",
            "https://ilinkai.weixin.qq.com:8443",
            "https://ilinkai.weixin.qq.com/ilink/bot/sendmessage",
            "https://ilinkai.weixin.qq.com?redirect=1",
            "https://weixin.qq.com.evil.example",
            "https://user@ilinkai.weixin.qq.com",
        ] {
            let config = NotificationChannelConfig {
                kind: NotificationChannelKind::WechatClaw,
                url: rejected.to_string(),
                ..NotificationChannelConfig::default()
            };
            assert!(config.wechat_claw_base_url().is_err(), "{rejected}");
        }
    }

    #[test]
    fn ntfy_requires_an_https_root_url_and_a_valid_topic() {
        for accepted in [
            "https://ntfy.sh",
            "https://ntfy.example.com/",
            "https://ntfy.example.com:8443",
        ] {
            let config = NotificationChannelConfig {
                kind: NotificationChannelKind::Ntfy,
                url: accepted.to_string(),
                chat_id: "codey_topic-1".to_string(),
                ..NotificationChannelConfig::default()
            };
            assert!(config.ntfy_base_url().is_ok(), "{accepted}");
            assert!(config.is_configured(), "{accepted}");
        }

        for rejected in [
            "http://ntfy.sh",
            "https://ntfy.sh/publish",
            "https://ntfy.sh?x=1",
            "https://ntfy.sh#fragment",
            "https://user@ntfy.sh",
        ] {
            let config = NotificationChannelConfig {
                kind: NotificationChannelKind::Ntfy,
                url: rejected.to_string(),
                chat_id: "codey_topic".to_string(),
                ..NotificationChannelConfig::default()
            };
            assert!(config.ntfy_base_url().is_err(), "{rejected}");
        }
    }

    #[test]
    fn ntfy_topic_matches_the_publish_contract() {
        for accepted in ["a", "codey_topic", "Codey-Topic-1", "_"] {
            let config = NotificationChannelConfig {
                kind: NotificationChannelKind::Ntfy,
                url: "https://ntfy.sh".to_string(),
                chat_id: accepted.to_string(),
                ..NotificationChannelConfig::default()
            };
            assert!(config.ntfy_topic().is_ok(), "{accepted}");
        }

        for rejected in ["", "has space", "topic.dot", "主题", &"a".repeat(65)] {
            let config = NotificationChannelConfig {
                kind: NotificationChannelKind::Ntfy,
                url: "https://ntfy.sh".to_string(),
                chat_id: rejected.to_string(),
                ..NotificationChannelConfig::default()
            };
            assert!(config.ntfy_topic().is_err(), "{rejected}");
        }
    }

    #[test]
    fn ntfy_channel_without_a_topic_is_not_configured() {
        let channel = NotificationChannelConfig {
            kind: NotificationChannelKind::Ntfy,
            enabled: true,
            url: "https://ntfy.sh".to_string(),
            ..NotificationChannelConfig::default()
        };

        assert!(!channel.is_configured());
    }

    #[test]
    fn redacted_ntfy_server_url_and_access_token_are_restored_on_save() {
        let previous = WebhookConfig {
            channels: vec![NotificationChannelConfig {
                id: "ntfy-1".to_string(),
                kind: NotificationChannelKind::Ntfy,
                enabled: true,
                url: "https://ntfy.example.com".to_string(),
                url_configured: true,
                bot_token: "tk_secret".to_string(),
                bot_token_configured: true,
                chat_id: "codey-topic".to_string(),
                ..NotificationChannelConfig::default()
            }],
        };
        let mut incoming = previous.clone();
        incoming.channels[0].url.clear();
        incoming.channels[0].bot_token.clear();
        incoming.merge_redacted_secrets(&previous);

        assert_eq!(incoming.channels[0].url, "https://ntfy.example.com");
        assert_eq!(incoming.channels[0].bot_token, "tk_secret");
        assert!(incoming.channels[0].is_configured());
    }

    #[test]
    fn explicit_ntfy_clear_does_not_restore_the_previous_credentials() {
        let previous = WebhookConfig {
            channels: vec![NotificationChannelConfig {
                id: "ntfy-1".to_string(),
                kind: NotificationChannelKind::Ntfy,
                url: "https://ntfy.example.com".to_string(),
                url_configured: true,
                bot_token: "tk_secret".to_string(),
                bot_token_configured: true,
                chat_id: "codey-topic".to_string(),
                ..NotificationChannelConfig::default()
            }],
        };
        let mut incoming = previous.clone();
        incoming.channels[0].bot_token.clear();
        incoming.channels[0].clear_bot_token = true;
        incoming.channels[0].url.clear();
        incoming.channels[0].clear_url = true;
        incoming.merge_redacted_secrets(&previous);

        assert!(incoming.channels[0].url.is_empty());
        assert!(incoming.channels[0].bot_token.is_empty());
        assert!(!incoming.channels[0].bot_token_configured);
        assert!(!incoming.channels[0].is_configured());
        let serialized = serde_json::to_value(&incoming).unwrap();
        assert!(serialized["channels"][0].get("clearUrl").is_none());
        assert!(serialized["channels"][0].get("clearBotToken").is_none());
    }

    #[test]
    fn redacted_wechat_claw_token_is_restored_when_other_settings_are_saved() {
        let previous = WebhookConfig {
            channels: vec![NotificationChannelConfig {
                id: "wechat-claw-1".to_string(),
                kind: NotificationChannelKind::WechatClaw,
                url: "https://ilinkai.weixin.qq.com".to_string(),
                url_configured: true,
                bot_token: "ilink-secret".to_string(),
                bot_token_configured: true,
                context_token: "context-secret".to_string(),
                context_token_configured: true,
                get_updates_buf: "sync-cursor".to_string(),
                chat_id: "user@im.wechat".to_string(),
                ..NotificationChannelConfig::default()
            }],
        };
        let mut incoming = previous.clone();
        incoming.channels[0].url.clear();
        incoming.channels[0].bot_token.clear();
        incoming.channels[0].context_token.clear();
        incoming.merge_redacted_secrets(&previous);

        assert_eq!(incoming.channels[0].bot_token, "ilink-secret");
        assert_eq!(incoming.channels[0].context_token, "context-secret");
        assert_eq!(incoming.channels[0].url, "https://ilinkai.weixin.qq.com");
        assert_eq!(incoming.channels[0].get_updates_buf, "sync-cursor");
    }

    #[test]
    fn wechat_claw_requires_activation_context_and_can_clear_it_explicitly() {
        let previous = WebhookConfig {
            channels: vec![NotificationChannelConfig {
                id: "wechat-claw-1".to_string(),
                kind: NotificationChannelKind::WechatClaw,
                enabled: true,
                url: "https://ilinkai.weixin.qq.com".to_string(),
                url_configured: true,
                bot_token: "ilink-secret".to_string(),
                bot_token_configured: true,
                context_token: "context-secret".to_string(),
                context_token_configured: true,
                chat_id: "user@im.wechat".to_string(),
                ..NotificationChannelConfig::default()
            }],
        };
        assert!(previous.channels[0].is_configured());

        let mut incomplete = previous.channels[0].clone();
        incomplete.context_token.clear();
        assert!(!incomplete.is_configured());

        let mut incoming = previous.clone();
        incoming.channels[0].context_token.clear();
        incoming.channels[0].clear_context_token = true;
        incoming.merge_redacted_secrets(&previous);

        assert!(incoming.channels[0].context_token.is_empty());
        assert!(!incoming.channels[0].context_token_configured);
        assert!(
            serde_json::to_value(&incoming).unwrap()["channels"][0]
                .get("clearContextToken")
                .is_none()
        );
    }

    #[test]
    fn expired_wechat_claw_session_is_not_configured_and_is_serialized() {
        let channel = NotificationChannelConfig {
            kind: NotificationChannelKind::WechatClaw,
            enabled: true,
            url: "https://ilinkai.weixin.qq.com".to_string(),
            bot_token: "ilink-secret".to_string(),
            context_token: "context-secret".to_string(),
            chat_id: "user@im.wechat".to_string(),
            session_status: NotificationChannelSessionStatus::Expired,
            ..NotificationChannelConfig::default()
        };

        assert!(!channel.is_configured());
        let value = serde_json::to_value(&channel).unwrap();
        assert_eq!(value["sessionStatus"], "expired");
    }

    #[test]
    fn expired_wechat_claw_status_survives_redacted_saves_until_rebound_or_cleared() {
        let previous = WebhookConfig {
            channels: vec![NotificationChannelConfig {
                id: "wechat-claw-1".to_string(),
                kind: NotificationChannelKind::WechatClaw,
                enabled: true,
                session_status: NotificationChannelSessionStatus::Expired,
                ..NotificationChannelConfig::default()
            }],
        };
        let mut redacted_save = previous.clone();
        redacted_save.channels[0].session_status = NotificationChannelSessionStatus::Active;
        redacted_save.channels[0].url_configured = true;
        redacted_save.channels[0].bot_token_configured = true;
        redacted_save.channels[0].context_token_configured = true;
        redacted_save.channels[0].chat_id = "stale-user@im.wechat".to_string();
        redacted_save.merge_redacted_secrets(&previous);
        assert_eq!(
            redacted_save.channels[0].session_status,
            NotificationChannelSessionStatus::Expired
        );

        let mut rebound = previous.clone();
        rebound.channels[0].url = "https://ilinkai.weixin.qq.com".to_string();
        rebound.channels[0].bot_token = "new-ilink-secret".to_string();
        rebound.channels[0].context_token = "new-context-secret".to_string();
        rebound.channels[0].chat_id = "user@im.wechat".to_string();
        rebound.merge_redacted_secrets(&previous);
        assert_eq!(
            rebound.channels[0].session_status,
            NotificationChannelSessionStatus::Active
        );
    }

    #[test]
    fn wechat_claw_cursor_is_not_restored_after_unbinding_or_rebinding() {
        let previous = WebhookConfig {
            channels: vec![NotificationChannelConfig {
                id: "wechat-claw-1".to_string(),
                kind: NotificationChannelKind::WechatClaw,
                enabled: true,
                url: "https://ilinkai.weixin.qq.com".to_string(),
                url_configured: true,
                bot_token: "old-bot-token".to_string(),
                bot_token_configured: true,
                context_token: "old-context-token".to_string(),
                context_token_configured: true,
                get_updates_buf: "old-cursor".to_string(),
                chat_id: "old-user@im.wechat".to_string(),
                ..NotificationChannelConfig::default()
            }],
        };

        let mut cleared = previous.clone();
        cleared.channels[0].context_token.clear();
        cleared.channels[0].clear_context_token = true;
        cleared.channels[0].get_updates_buf.clear();
        cleared.merge_redacted_secrets(&previous);
        assert!(cleared.channels[0].get_updates_buf.is_empty());

        let mut cleared_bot = previous.clone();
        cleared_bot.channels[0].bot_token.clear();
        cleared_bot.channels[0].clear_bot_token = true;
        cleared_bot.channels[0].get_updates_buf.clear();
        cleared_bot.merge_redacted_secrets(&previous);
        assert!(cleared_bot.channels[0].get_updates_buf.is_empty());

        let mut rebound = previous.clone();
        rebound.channels[0].bot_token = "new-bot-token".to_string();
        rebound.channels[0].context_token = "new-context-token".to_string();
        rebound.channels[0].chat_id = "new-user@im.wechat".to_string();
        rebound.channels[0].get_updates_buf.clear();
        rebound.merge_redacted_secrets(&previous);
        assert!(rebound.channels[0].get_updates_buf.is_empty());
    }
}
