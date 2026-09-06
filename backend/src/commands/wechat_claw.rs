use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::stream::{self, StreamExt};
use qrcode::{QrCode, render::svg};
use reqwest::{Client, RequestBuilder, StatusCode, Url, redirect};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;
use uuid::Uuid;

use super::{AppState, save_config_to_store};
use crate::config::CodeyConfig;
use crate::notifications::ilink;
use crate::notifications::{
    NotificationChannelConfig, NotificationChannelKind, NotificationChannelSessionStatus,
};

const ILINK_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SYNC_IDLE_DELAY: Duration = Duration::from_millis(750);
const SYNC_INITIAL_BACKOFF: Duration = Duration::from_secs(2);
const SYNC_MAX_BACKOFF: Duration = Duration::from_secs(60);
const SYNC_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const SYNC_RECOVERY_PREPARE_TIMEOUT: Duration = Duration::from_secs(8);
const SYNC_RECOVERY_POLL_TIMEOUT: Duration = Duration::from_secs(2);
const STALE_TOKEN_COOLDOWN: Duration = Duration::from_secs(60 * 60);
const MAX_ILINK_RESPONSE_BYTES: usize = 1024 * 1024;

/// QR status is an iLink long-poll endpoint. Keep its client separate from
/// notification delivery so a temporary scan can wait efficiently without
/// widening the short timeout used for one-way notifications.
pub(super) fn wechat_claw_login_http_client() -> Client {
    Client::builder()
        .user_agent(format!("Codey/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(40))
        .redirect(redirect::Policy::none())
        .build()
        .expect("create WeChat ClawBot login HTTP client")
}

#[derive(Debug, Default)]
pub(super) struct WechatClawLoginState {
    sessions: HashMap<String, PendingWechatClawLogin>,
}

#[derive(Debug, Default)]
pub(super) struct WechatClawSessionGuard {
    stale_bindings: HashMap<String, WechatClawStaleBinding>,
}

#[derive(Debug)]
struct WechatClawStaleBinding {
    fingerprint: [u8; 32],
    retry_at: Instant,
}

#[derive(Debug)]
pub(super) struct WechatClawSyncHandle {
    fingerprint: [u8; 32],
    shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Debug, Clone)]
struct WechatClawSyncChannel {
    id: String,
    base_url: Url,
    bot_token: String,
    chat_id: String,
    context_token: String,
    get_updates_buf: String,
}

#[derive(Debug, Clone)]
struct WechatClawPreparedBinding {
    id: String,
    fingerprint: [u8; 32],
}

impl WechatClawPreparedBinding {
    fn new(channel: &WechatClawSyncChannel) -> Self {
        Self {
            id: channel.id.clone(),
            fingerprint: wechat_claw_sync_binding_fingerprint(channel),
        }
    }

    fn matches(&self, channel: &WechatClawSyncChannel) -> bool {
        self.id == channel.id && self.fingerprint == wechat_claw_sync_binding_fingerprint(channel)
    }
}

#[derive(Debug)]
struct WechatClawSyncChannelState {
    channel: WechatClawSyncChannel,
    notify_started: bool,
    next_attempt: Instant,
    failure_count: u32,
}

#[derive(Debug)]
struct WechatClawSyncSuccess {
    get_updates_buf: String,
    context_token: Option<String>,
}

#[derive(Debug)]
struct WechatClawSyncFailure {
    message: String,
    notify_started: bool,
    token_stale: bool,
}

pub(super) async fn sync_wechat_claw_service(state: &Arc<AppState>) {
    let _sync_guard = state.wechat_claw_sync_update.lock().await;
    start_wechat_claw_service_locked(state, None).await;
}

async fn start_wechat_claw_service_locked(
    state: &Arc<AppState>,
    prepared_binding: Option<&WechatClawPreparedBinding>,
) {
    let channels = {
        let config = state.config.read().await;
        configured_wechat_claw_sync_channels(&config)
    };
    let fingerprint = wechat_claw_sync_fingerprint(&channels);
    if !state.is_shutting_down()
        && !channels.is_empty()
        && state
            .wechat_claw_sync
            .lock()
            .await
            .as_ref()
            .is_some_and(|handle| handle.fingerprint == fingerprint && !handle.task.is_finished())
    {
        return;
    }

    stop_wechat_claw_service_locked(state).await;
    if channels.is_empty() || state.is_shutting_down() {
        return;
    }

    let (shutdown, shutdown_rx) = oneshot::channel();
    let sync_state = Arc::clone(state);
    let prepared_binding = prepared_binding.cloned();
    let task = tokio::spawn(async move {
        run_wechat_claw_sync_service(sync_state, channels, prepared_binding, shutdown_rx).await;
    });
    *state.wechat_claw_sync.lock().await = Some(WechatClawSyncHandle {
        fingerprint,
        shutdown,
        task,
    });
}

pub(super) async fn stop_wechat_claw_service(state: &Arc<AppState>) {
    let _sync_guard = state.wechat_claw_sync_update.lock().await;
    stop_wechat_claw_service_locked(state).await;
}

async fn stop_wechat_claw_service_locked(state: &Arc<AppState>) {
    let handle = state.wechat_claw_sync.lock().await.take();
    if let Some(WechatClawSyncHandle { shutdown, task, .. }) = handle {
        let _ = shutdown.send(());
        if let Err(error) = task.await {
            eprintln!("Codey 微信 ClawBot 同步服务异常退出：{error}");
        }
    }
}

fn configured_wechat_claw_sync_channels(config: &CodeyConfig) -> Vec<WechatClawSyncChannel> {
    let mut channels = config
        .webhook
        .channels
        .iter()
        .filter(|channel| {
            channel.enabled
                && channel.kind == NotificationChannelKind::WechatClaw
                && channel.is_configured()
        })
        .filter_map(|channel| {
            Some(WechatClawSyncChannel {
                id: channel.id.clone(),
                base_url: channel.wechat_claw_base_url().ok()?,
                bot_token: channel.bot_token.trim().to_string(),
                chat_id: channel.chat_id.trim().to_string(),
                context_token: channel.context_token.trim().to_string(),
                get_updates_buf: channel.get_updates_buf.trim().to_string(),
            })
        })
        .collect::<Vec<_>>();
    channels.sort_by(|left, right| left.id.cmp(&right.id));
    channels
}

fn wechat_claw_sync_fingerprint(channels: &[WechatClawSyncChannel]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for channel in channels {
        for value in [
            channel.id.as_str(),
            channel.base_url.as_str(),
            channel.bot_token.as_str(),
            channel.chat_id.as_str(),
            channel.context_token.as_str(),
        ] {
            hasher.update(value.len().to_be_bytes());
            hasher.update(value.as_bytes());
        }
    }
    hasher.finalize().into()
}

fn wechat_claw_sync_binding_fingerprint(channel: &WechatClawSyncChannel) -> [u8; 32] {
    wechat_claw_binding_fingerprint(
        &channel.id,
        &channel.base_url,
        &channel.bot_token,
        &channel.chat_id,
    )
}

fn wechat_claw_binding_fingerprint(
    id: &str,
    base_url: &Url,
    bot_token: &str,
    chat_id: &str,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for value in [id, base_url.as_str(), bot_token, chat_id] {
        hasher.update(value.len().to_be_bytes());
        hasher.update(value.as_bytes());
    }
    hasher.finalize().into()
}

fn notification_channel_binding_fingerprint(
    channel: &NotificationChannelConfig,
) -> Option<[u8; 32]> {
    let base_url = channel.wechat_claw_base_url().ok()?;
    Some(wechat_claw_binding_fingerprint(
        &channel.id,
        &base_url,
        channel.bot_token.trim(),
        channel.chat_id.trim(),
    ))
}

impl WechatClawSessionGuard {
    fn pause(&mut self, channel: &WechatClawSyncChannel) -> Duration {
        self.stale_bindings.insert(
            channel.id.clone(),
            WechatClawStaleBinding {
                fingerprint: wechat_claw_sync_binding_fingerprint(channel),
                retry_at: Instant::now() + STALE_TOKEN_COOLDOWN,
            },
        );
        STALE_TOKEN_COOLDOWN
    }

    fn pause_notification_channel(
        &mut self,
        channel: &NotificationChannelConfig,
    ) -> Option<Duration> {
        let fingerprint = notification_channel_binding_fingerprint(channel)?;
        self.stale_bindings.insert(
            channel.id.clone(),
            WechatClawStaleBinding {
                fingerprint,
                retry_at: Instant::now() + STALE_TOKEN_COOLDOWN,
            },
        );
        Some(STALE_TOKEN_COOLDOWN)
    }

    fn remaining_for_fingerprint(
        &mut self,
        channel_id: &str,
        fingerprint: [u8; 32],
    ) -> Option<Duration> {
        let stale = self.stale_bindings.get(channel_id)?;
        let now = Instant::now();
        if stale.fingerprint != fingerprint || stale.retry_at <= now {
            self.stale_bindings.remove(channel_id);
            return None;
        }
        Some(stale.retry_at.saturating_duration_since(now))
    }

    fn remaining(&mut self, channel: &WechatClawSyncChannel) -> Option<Duration> {
        self.remaining_for_fingerprint(&channel.id, wechat_claw_sync_binding_fingerprint(channel))
    }

    fn remaining_for_notification_channel(
        &mut self,
        channel: &NotificationChannelConfig,
    ) -> Option<Duration> {
        let fingerprint = notification_channel_binding_fingerprint(channel)?;
        self.remaining_for_fingerprint(&channel.id, fingerprint)
    }
}

async fn pause_wechat_claw_sync_channel(
    state: &Arc<AppState>,
    channel: &WechatClawSyncChannel,
) -> Duration {
    state.wechat_claw_session_guard.lock().await.pause(channel)
}

async fn wechat_claw_sync_cooldown_remaining(
    state: &Arc<AppState>,
    channel: &WechatClawSyncChannel,
) -> Option<Duration> {
    state
        .wechat_claw_session_guard
        .lock()
        .await
        .remaining(channel)
}

pub(super) async fn pause_wechat_claw_notification_channel(
    state: &Arc<AppState>,
    channel: &NotificationChannelConfig,
) -> Option<Duration> {
    state
        .wechat_claw_session_guard
        .lock()
        .await
        .pause_notification_channel(channel)
}

pub(super) async fn wechat_claw_notification_cooldown_remaining(
    state: &Arc<AppState>,
    channel: &NotificationChannelConfig,
) -> Option<Duration> {
    state
        .wechat_claw_session_guard
        .lock()
        .await
        .remaining_for_notification_channel(channel)
}

async fn run_wechat_claw_sync_service(
    state: Arc<AppState>,
    channels: Vec<WechatClawSyncChannel>,
    prepared_binding: Option<WechatClawPreparedBinding>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let stop_channels = channels.clone();
    let mut workers = tokio::task::JoinSet::new();
    for channel in channels {
        let state = Arc::clone(&state);
        let notify_started = prepared_binding
            .as_ref()
            .is_some_and(|prepared| prepared.matches(&channel));
        workers.spawn(
            async move { run_wechat_claw_sync_channel(state, channel, notify_started).await },
        );
    }

    let stopped = loop {
        if workers.is_empty() {
            break false;
        }
        tokio::select! {
            _ = &mut shutdown => {
                workers.abort_all();
                while workers.join_next().await.is_some() {}
                break true;
            }
            result = workers.join_next() => {
                if let Some(Err(error)) = result
                    && !error.is_cancelled()
                {
                    eprintln!("Codey 微信 ClawBot 同步 worker 异常退出：{error}");
                }
            }
        }
    };

    if stopped {
        notify_stop_wechat_claw_channels(&state.wechat_claw_login_http_client, stop_channels).await;
    }
}

async fn run_wechat_claw_sync_channel(
    state: Arc<AppState>,
    channel: WechatClawSyncChannel,
    notify_started: bool,
) {
    let mut channel_state = WechatClawSyncChannelState {
        channel,
        notify_started,
        next_attempt: Instant::now(),
        failure_count: 0,
    };
    loop {
        tokio::time::sleep_until(channel_state.next_attempt.into()).await;
        if let Some(remaining) =
            wechat_claw_sync_cooldown_remaining(&state, &channel_state.channel).await
        {
            channel_state.next_attempt = Instant::now() + remaining;
            continue;
        }
        let result = sync_wechat_claw_channel(
            &state.wechat_claw_login_http_client,
            channel_state.channel.clone(),
            channel_state.notify_started,
        )
        .await;
        match result {
            Ok(update) => {
                channel_state.notify_started = true;
                channel_state.failure_count = 0;
                channel_state.next_attempt = Instant::now() + SYNC_IDLE_DELAY;
                let persisted_channel = channel_state.channel.clone();
                channel_state.channel.get_updates_buf = update.get_updates_buf.clone();
                if let Some(context_token) = update.context_token.clone() {
                    channel_state.channel.context_token = context_token;
                }
                if let Err(error) =
                    persist_wechat_claw_sync_update(&state, &persisted_channel, update).await
                {
                    eprintln!("Codey 微信 ClawBot 同步状态保存失败：{error}");
                }
            }
            Err(error) => {
                if error.token_stale {
                    let cooldown =
                        pause_wechat_claw_sync_channel(&state, &channel_state.channel).await;
                    channel_state.notify_started = error.notify_started;
                    channel_state.failure_count = 0;
                    channel_state.next_attempt = Instant::now() + cooldown;
                    eprintln!(
                        "Codey 微信 ClawBot 凭据暂时不可用，将在一小时后自动重试：{}",
                        error.message
                    );
                    continue;
                }
                channel_state.notify_started = error.notify_started;
                channel_state.failure_count = channel_state.failure_count.saturating_add(1);
                channel_state.next_attempt =
                    Instant::now() + sync_backoff(channel_state.failure_count);
                eprintln!("Codey 微信 ClawBot 同步失败：{}", error.message);
            }
        }
    }
}

async fn notify_stop_wechat_claw_channels(client: &Client, channels: Vec<WechatClawSyncChannel>) {
    stream::iter(channels)
        .for_each_concurrent(None, |channel| async move {
            let result = async {
                let request = sync_ilink_post_request(
                    client,
                    &channel.base_url,
                    &channel.bot_token,
                    "ilink/bot/msg/notifystop",
                    &json!({"base_info": ilink::base_info()}),
                )?
                .timeout(SYNC_STOP_TIMEOUT);
                activation_response_json(
                    request,
                    "后台同步停止",
                    ActivationResponseContract::GetUpdates,
                )
                .await
                .map_err(WechatClawSyncFailure::from)?;
                Ok::<(), WechatClawSyncFailure>(())
            }
            .await;
            if let Err(error) = result {
                eprintln!("Codey 微信 ClawBot 停止同步握手失败：{}", error.message);
            }
        })
        .await;
}

async fn sync_wechat_claw_channel(
    client: &Client,
    channel: WechatClawSyncChannel,
    notify_started: bool,
) -> Result<WechatClawSyncSuccess, WechatClawSyncFailure> {
    let notify_started = if !notify_started {
        let request = sync_ilink_post_request(
            client,
            &channel.base_url,
            &channel.bot_token,
            "ilink/bot/msg/notifystart",
            &json!({"base_info": ilink::base_info()}),
        )?;
        activation_response_json(
            request,
            "后台同步启动",
            ActivationResponseContract::GetUpdates,
        )
        .await
        .map_err(|error| WechatClawSyncFailure::from_activation(error, false))?;
        true
    } else {
        true
    };

    let request = sync_ilink_post_request(
        client,
        &channel.base_url,
        &channel.bot_token,
        "ilink/bot/getupdates",
        &json!({
            "get_updates_buf": channel.get_updates_buf,
            "base_info": ilink::base_info(),
        }),
    )
    .map_err(|error| error.with_notify_started(notify_started))?;
    let payload = match activation_response_json(
        request,
        "后台消息同步",
        ActivationResponseContract::GetUpdates,
    )
    .await
    {
        Ok(payload) => payload,
        Err(error) => {
            return Err(WechatClawSyncFailure::from_activation(
                error,
                notify_started,
            ));
        }
    };
    let get_updates_buf = response_updates_buffer(&payload)
        .unwrap_or(channel.get_updates_buf.as_str())
        .to_string();
    let context_token = activation_context(&payload, &channel.chat_id)
        .map(|(_, context_token)| context_token)
        .filter(|context_token| !context_token.trim().is_empty());
    Ok(WechatClawSyncSuccess {
        get_updates_buf,
        context_token,
    })
}

impl From<String> for WechatClawSyncFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            notify_started: false,
            token_stale: false,
        }
    }
}

impl From<ActivationRequestError> for WechatClawSyncFailure {
    fn from(error: ActivationRequestError) -> Self {
        Self::from_activation(error, false)
    }
}

impl WechatClawSyncFailure {
    fn from_activation(error: ActivationRequestError, notify_started: bool) -> Self {
        let token_stale = matches!(&error, ActivationRequestError::StaleToken(_));
        Self {
            message: activation_request_error_message(error),
            notify_started,
            token_stale,
        }
    }

    fn with_notify_started(mut self, notify_started: bool) -> Self {
        self.notify_started = notify_started;
        self
    }
}

fn activation_request_error_message(error: ActivationRequestError) -> String {
    match error {
        ActivationRequestError::Retryable => "微信 ClawBot 同步服务暂时无响应".to_string(),
        ActivationRequestError::Fatal(message) | ActivationRequestError::StaleToken(message) => {
            message
        }
    }
}

fn sync_ilink_post_request(
    client: &Client,
    base_url: &Url,
    bot_token: &str,
    endpoint: &str,
    body: &Value,
) -> Result<RequestBuilder, WechatClawSyncFailure> {
    let endpoint = base_url
        .join(endpoint)
        .map_err(|_| "微信 ClawBot 服务地址无效".to_string())?;
    Ok(client
        .post(endpoint)
        .headers(ilink::headers(Some(bot_token)))
        .json(&body))
}

fn sync_backoff(failure_count: u32) -> Duration {
    let exponent = failure_count.saturating_sub(1).min(5);
    (SYNC_INITIAL_BACKOFF * 2u32.pow(exponent)).min(SYNC_MAX_BACKOFF)
}

fn cooldown_minutes(remaining: Duration) -> u64 {
    remaining.as_secs().saturating_add(59) / 60
}

fn stale_token_cooldown_message(remaining: Duration) -> String {
    format!(
        "微信 ClawBot 凭据暂时不可用，正在冷却，约 {} 分钟后自动重试",
        cooldown_minutes(remaining).max(1)
    )
}

async fn persist_wechat_claw_sync_update(
    state: &Arc<AppState>,
    channel: &WechatClawSyncChannel,
    update: WechatClawSyncSuccess,
) -> Result<(), String> {
    let _config_write_guard = state.config_write_lock.lock().await;
    let mut config = state.config.read().await.clone();
    let Some(saved_channel) = config.webhook.channels.iter_mut().find(|saved| {
        saved.id == channel.id
            && saved.kind == NotificationChannelKind::WechatClaw
            && saved.bot_token.trim() == channel.bot_token
            && saved.chat_id.trim() == channel.chat_id
            && saved.context_token.trim() == channel.context_token
            && saved
                .wechat_claw_base_url()
                .is_ok_and(|base_url| base_url == channel.base_url)
    }) else {
        return Ok(());
    };

    let mut changed = false;
    let next_buf = update.get_updates_buf.trim();
    if saved_channel.get_updates_buf.trim() != next_buf {
        saved_channel.get_updates_buf = next_buf.to_string();
        changed = true;
    }
    if let Some(context_token) = update.context_token.map(|value| value.trim().to_string())
        && !context_token.is_empty()
        && saved_channel.context_token.trim() != context_token
    {
        saved_channel.context_token = context_token;
        saved_channel.context_token_configured = true;
        saved_channel.session_status = NotificationChannelSessionStatus::Active;
        changed = true;
    }
    if !changed {
        return Ok(());
    }
    save_config_to_store(state, &config).await?;
    *state.config.write().await = config;
    Ok(())
}

pub(super) async fn refresh_wechat_claw_channel_context(
    state: &Arc<AppState>,
    channel: &NotificationChannelConfig,
) -> Result<bool, String> {
    if channel.kind != NotificationChannelKind::WechatClaw {
        return Ok(false);
    }
    let _sync_guard = state.wechat_claw_sync_update.lock().await;
    stop_wechat_claw_service_locked(state).await;
    let result = refresh_wechat_claw_channel_context_locked(state, channel).await;
    let prepared_binding = result
        .as_ref()
        .ok()
        .map(|(_, sync_channel)| WechatClawPreparedBinding::new(sync_channel));
    start_wechat_claw_service_locked(state, prepared_binding.as_ref()).await;
    result.map(|(context_changed, _)| context_changed)
}

async fn refresh_wechat_claw_channel_context_locked(
    state: &Arc<AppState>,
    channel: &NotificationChannelConfig,
) -> Result<(bool, WechatClawSyncChannel), String> {
    let sync_channel = {
        let config = state.config.read().await;
        let Some(saved) = config.webhook.channels.iter().find(|saved| {
            saved.id == channel.id && saved.kind == NotificationChannelKind::WechatClaw
        }) else {
            return Err("微信 ClawBot 渠道已不存在".to_string());
        };
        if !saved.enabled || !saved.is_configured() {
            return Err("微信 ClawBot 渠道当前不可用".to_string());
        }
        if saved.bot_token.trim() != channel.bot_token.trim()
            || saved.chat_id.trim() != channel.chat_id.trim()
            || saved.wechat_claw_base_url().ok() != channel.wechat_claw_base_url().ok()
        {
            return Err("微信 ClawBot 渠道绑定已变更".to_string());
        }
        WechatClawSyncChannel {
            id: saved.id.clone(),
            base_url: saved.wechat_claw_base_url().map_err(ToString::to_string)?,
            bot_token: saved.bot_token.trim().to_string(),
            chat_id: saved.chat_id.trim().to_string(),
            context_token: saved.context_token.trim().to_string(),
            get_updates_buf: saved.get_updates_buf.trim().to_string(),
        }
    };
    if let Some(remaining) = wechat_claw_sync_cooldown_remaining(state, &sync_channel).await {
        return Err(stale_token_cooldown_message(remaining));
    }
    let notify_request = sync_ilink_post_request(
        &state.wechat_claw_login_http_client,
        &sync_channel.base_url,
        &sync_channel.bot_token,
        "ilink/bot/msg/notifystart",
        &json!({"base_info": ilink::base_info()}),
    )
    .map_err(|error| error.message)?
    .timeout(SYNC_RECOVERY_PREPARE_TIMEOUT);
    match activation_response_json(
        notify_request,
        "同步重建",
        ActivationResponseContract::GetUpdates,
    )
    .await
    {
        Ok(_) => {}
        Err(ActivationRequestError::StaleToken(_)) => {
            let cooldown = pause_wechat_claw_sync_channel(state, &sync_channel).await;
            return Err(stale_token_cooldown_message(cooldown));
        }
        Err(ActivationRequestError::Retryable) => {
            return Err("微信 ClawBot 同步重建暂时无响应".to_string());
        }
        Err(error) => return Err(activation_request_error_message(error)),
    }

    let previous_context = sync_channel.context_token.trim().to_string();
    let update = match tokio::time::timeout(
        SYNC_RECOVERY_POLL_TIMEOUT,
        sync_wechat_claw_channel(
            &state.wechat_claw_login_http_client,
            sync_channel.clone(),
            true,
        ),
    )
    .await
    {
        Ok(Ok(update)) => Some(update),
        Ok(Err(error)) if error.token_stale => {
            let cooldown = pause_wechat_claw_sync_channel(state, &sync_channel).await;
            return Err(stale_token_cooldown_message(cooldown));
        }
        Ok(Err(error)) => {
            eprintln!(
                "Codey 微信 ClawBot 同步重建后的即时轮询失败，将直接验证消息投递：{}",
                error.message
            );
            None
        }
        Err(_) => None,
    };
    let context_changed = update
        .as_ref()
        .and_then(|update| update.context_token.as_deref())
        .map(str::trim)
        .filter(|context_token| !context_token.is_empty())
        .is_some_and(|context_token| context_token != previous_context);
    if let Some(update) = update {
        persist_wechat_claw_sync_update(state, &sync_channel, update).await?;
    }
    Ok((context_changed, sync_channel))
}

#[derive(Debug)]
struct PendingWechatClawLogin {
    base_url: String,
    created_at: Instant,
    poll_in_flight: bool,
    phase: WechatClawLoginPhase,
}

#[derive(Debug, Clone)]
enum WechatClawLoginPhase {
    Qr {
        qr_code: String,
    },
    Activating {
        bot_token: String,
        recipient_id: String,
        get_updates_buf: String,
        notify_started: bool,
    },
}

pub(super) async fn start_wechat_claw_login(state: &AppState) -> Result<Value, String> {
    let response = get_bot_qrcode_request(&state.wechat_claw_login_http_client)?
        .send()
        .await
        .map_err(|_| "无法连接微信 ClawBot 登录服务，请检查网络后重试".to_string())?;
    let payload = login_response_json(response).await?;
    let qr_code = payload
        .get("qrcode")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "微信 ClawBot 登录服务没有返回二维码，请重新开始扫码".to_string())?
        .to_string();
    // `qrcode` is the opaque status-poll token. `qrcode_img_content` is the URL
    // that must be encoded into the scannable image; it is not an image URL.
    // Generate it locally to avoid another request and webview policy differences.
    let qr_code_image_src = qr_code_image_data_uri(qr_code_scan_payload(&payload)?)?;
    let login_id = Uuid::new_v4().to_string();
    let mut logins = state.wechat_claw_logins.lock().await;
    logins.remove_expired();
    logins.sessions.insert(
        login_id.clone(),
        PendingWechatClawLogin {
            base_url: ILINK_BASE_URL.to_string(),
            created_at: Instant::now(),
            poll_in_flight: false,
            phase: WechatClawLoginPhase::Qr {
                qr_code: qr_code.clone(),
            },
        },
    );
    Ok(json!({
        "loginId": login_id,
        "status": "wait",
        "qrCode": qr_code,
        "qrCodeImageUrl": qr_code_image_src,
    }))
}

pub(super) async fn poll_wechat_claw_login(
    state: &AppState,
    login_id: String,
) -> Result<Value, String> {
    let (base_url, phase) = {
        let mut logins = state.wechat_claw_logins.lock().await;
        logins.remove_expired();
        let Some(session) = logins.sessions.get_mut(&login_id) else {
            return Ok(json!({
                "status": "expired",
                "message": "二维码已过期，请重新开始扫码",
            }));
        };
        if session.poll_in_flight {
            return Ok(pending_login_response(&session.phase));
        }
        session.poll_in_flight = true;
        (session.base_url.clone(), session.phase.clone())
    };

    let result = match phase {
        WechatClawLoginPhase::Qr { qr_code } => {
            poll_wechat_claw_qr(state, &login_id, qr_code, base_url).await
        }
        WechatClawLoginPhase::Activating {
            bot_token,
            recipient_id,
            get_updates_buf,
            notify_started,
        } => {
            poll_wechat_claw_activation(
                state,
                &login_id,
                base_url,
                bot_token,
                recipient_id,
                get_updates_buf,
                notify_started,
            )
            .await
        }
    };
    if let Some(session) = state
        .wechat_claw_logins
        .lock()
        .await
        .sessions
        .get_mut(&login_id)
    {
        session.poll_in_flight = false;
    }
    result
}

async fn poll_wechat_claw_qr(
    state: &AppState,
    login_id: &str,
    qr_code: String,
    base_url: String,
) -> Result<Value, String> {
    let url = endpoint_url(&base_url, "ilink/bot/get_qrcode_status")?;
    let response = state
        .wechat_claw_login_http_client
        .get(url)
        .query(&[("qrcode", qr_code)])
        .headers(ilink::headers(None))
        .send()
        .await
        .map_err(|_| "无法查询微信 ClawBot 扫码状态，请检查网络后重试".to_string())?;
    let payload = login_response_json(response).await?;
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("failed");

    match status {
        "wait" => Ok(json!({"status":"wait"})),
        "scaned" => Ok(json!({"status":"scanned", "message":"已扫码，请在微信中确认授权"})),
        "scaned_but_redirect" => {
            let Some(next_base_url) = redirect_base_url(&payload)? else {
                return Ok(
                    json!({"status":"failed", "message":"微信 ClawBot 返回了无效的登录地址，请重新开始扫码"}),
                );
            };
            let mut logins = state.wechat_claw_logins.lock().await;
            if let Some(session) = logins.sessions.get_mut(login_id) {
                session.base_url = next_base_url;
            }
            Ok(json!({"status":"scanned", "message":"已扫码，请在微信中确认授权"}))
        }
        "confirmed" => {
            let token = payload
                .get("bot_token")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "扫码完成但没有获得有效凭据，请重新开始扫码".to_string())?
                .to_string();
            let confirmed_base_url = payload
                .get("baseurl")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(validate_base_url)
                .transpose()?
                .unwrap_or(base_url);
            let recipient_id = payload
                .get("ilink_user_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or_default()
                .to_string();
            let mut logins = state.wechat_claw_logins.lock().await;
            let Some(session) = logins.sessions.get_mut(login_id) else {
                return Ok(json!({
                    "status": "expired",
                    "message": "激活已过期，请重新开始扫码",
                }));
            };
            session.base_url = confirmed_base_url;
            session.created_at = Instant::now();
            session.phase = WechatClawLoginPhase::Activating {
                bot_token: token,
                recipient_id,
                get_updates_buf: String::new(),
                notify_started: false,
            };
            Ok(json!({
                "status": "activating",
                "message": "扫码已确认。请在微信中打开 ClawBot，并发送一条消息完成激活。",
            }))
        }
        "expired" => {
            state
                .wechat_claw_logins
                .lock()
                .await
                .sessions
                .remove(login_id);
            Ok(json!({"status":"expired", "message":"二维码已过期，请重新开始扫码"}))
        }
        _ => {
            state
                .wechat_claw_logins
                .lock()
                .await
                .sessions
                .remove(login_id);
            Ok(json!({"status":"failed", "message":"微信 ClawBot 登录未完成，请重新开始扫码"}))
        }
    }
}

fn pending_login_response(phase: &WechatClawLoginPhase) -> Value {
    match phase {
        WechatClawLoginPhase::Qr { .. } => json!({"status":"wait"}),
        WechatClawLoginPhase::Activating { .. } => json!({
            "status": "activating",
            "message": "正在等待微信消息完成 ClawBot 激活。",
        }),
    }
}

#[derive(Debug)]
enum ActivationRequestError {
    Retryable,
    Fatal(String),
    StaleToken(String),
}

#[derive(Debug, Clone, Copy)]
enum ActivationResponseContract {
    Strict,
    GetUpdates,
}

async fn poll_wechat_claw_activation(
    state: &AppState,
    login_id: &str,
    base_url: String,
    bot_token: String,
    recipient_id: String,
    get_updates_buf: String,
    notify_started: bool,
) -> Result<Value, String> {
    if !notify_started {
        let request =
            notify_start_request(&state.wechat_claw_login_http_client, &base_url, &bot_token)?;
        match activation_response_json(request, "激活", ActivationResponseContract::Strict).await
        {
            Ok(_) => {
                let mut logins = state.wechat_claw_logins.lock().await;
                if let Some(PendingWechatClawLogin {
                    phase: WechatClawLoginPhase::Activating { notify_started, .. },
                    ..
                }) = logins.sessions.get_mut(login_id)
                {
                    *notify_started = true;
                }
            }
            Err(ActivationRequestError::Retryable) => {
                return Ok(activation_retry_response());
            }
            Err(
                ActivationRequestError::Fatal(message)
                | ActivationRequestError::StaleToken(message),
            ) => {
                return Ok(fail_activation(state, login_id, message).await);
            }
        }
    }

    let request = get_updates_request(
        &state.wechat_claw_login_http_client,
        &base_url,
        &bot_token,
        &get_updates_buf,
    )?;
    let payload =
        match activation_response_json(request, "消息同步", ActivationResponseContract::GetUpdates)
            .await
        {
            Ok(payload) => payload,
            Err(ActivationRequestError::Retryable) => return Ok(activation_retry_response()),
            Err(
                ActivationRequestError::Fatal(message)
                | ActivationRequestError::StaleToken(message),
            ) => {
                return Ok(fail_activation(state, login_id, message).await);
            }
        };
    let next_updates_buf = response_updates_buffer(&payload)
        .unwrap_or(get_updates_buf.as_str())
        .to_string();

    if let Some((from_user_id, context_token)) = activation_context(&payload, &recipient_id) {
        state
            .wechat_claw_logins
            .lock()
            .await
            .sessions
            .remove(login_id);
        return Ok(json!({
            "status": "confirmed",
            "baseUrl": base_url,
            "botToken": bot_token,
            "recipientId": from_user_id,
            "contextToken": context_token,
            "getUpdatesBuf": next_updates_buf,
        }));
    }

    let mut logins = state.wechat_claw_logins.lock().await;
    if let Some(PendingWechatClawLogin {
        phase: WechatClawLoginPhase::Activating {
            get_updates_buf, ..
        },
        ..
    }) = logins.sessions.get_mut(login_id)
    {
        *get_updates_buf = next_updates_buf;
    }
    Ok(json!({
        "status": "activating",
        "message": "请在微信中打开 ClawBot，并发送一条消息完成激活。",
    }))
}

fn activation_retry_response() -> Value {
    json!({
        "status": "activating",
        "message": "微信 ClawBot 激活服务暂时无响应，正在自动重试；请保持当前页面打开。",
    })
}

async fn fail_activation(state: &AppState, login_id: &str, message: String) -> Value {
    state
        .wechat_claw_logins
        .lock()
        .await
        .sessions
        .remove(login_id);
    json!({"status":"failed", "message":message})
}

fn notify_start_request(
    client: &Client,
    base_url: &str,
    bot_token: &str,
) -> Result<RequestBuilder, String> {
    ilink_post_request(
        client,
        base_url,
        bot_token,
        "ilink/bot/msg/notifystart",
        &json!({"base_info": ilink::base_info()}),
    )
}

fn get_updates_request(
    client: &Client,
    base_url: &str,
    bot_token: &str,
    get_updates_buf: &str,
) -> Result<RequestBuilder, String> {
    ilink_post_request(
        client,
        base_url,
        bot_token,
        "ilink/bot/getupdates",
        &json!({
            "get_updates_buf": get_updates_buf,
            "base_info": ilink::base_info(),
        }),
    )
}

fn ilink_post_request(
    client: &Client,
    base_url: &str,
    bot_token: &str,
    endpoint: &str,
    body: &Value,
) -> Result<RequestBuilder, String> {
    Ok(client
        .post(endpoint_url(base_url, endpoint)?)
        .headers(ilink::headers(Some(bot_token)))
        .json(&body))
}

async fn activation_response_json(
    request: RequestBuilder,
    action: &str,
    contract: ActivationResponseContract,
) -> Result<Value, ActivationRequestError> {
    let response = request
        .send()
        .await
        .map_err(|_| ActivationRequestError::Retryable)?;
    let status = response.status();
    if !status.is_success() {
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return Err(ActivationRequestError::Retryable);
        }
        return Err(ActivationRequestError::Fatal(format!(
            "微信 ClawBot {action}服务返回 HTTP {status}，请重新扫码"
        )));
    }
    let body = crate::http_response::read_bounded_body(
        response,
        MAX_ILINK_RESPONSE_BYTES,
        "微信 ClawBot 服务响应",
    )
    .await
    .map_err(|_| ActivationRequestError::Retryable)?;
    let body = String::from_utf8_lossy(&body);
    let payload = parse_activation_response_body(&body, action, contract)?;
    if wechat_claw_token_stale(&payload) {
        let message = validate_activation_response(&payload, action, contract)
            .err()
            .unwrap_or_else(|| "微信 ClawBot 凭据暂时不可用".to_string());
        return Err(ActivationRequestError::StaleToken(message));
    }
    validate_activation_response(&payload, action, contract)
        .map_err(ActivationRequestError::Fatal)?;
    Ok(payload)
}

fn parse_activation_response_body(
    body: &str,
    action: &str,
    contract: ActivationResponseContract,
) -> Result<Value, ActivationRequestError> {
    if body.trim().is_empty() && matches!(contract, ActivationResponseContract::GetUpdates) {
        return Ok(json!({}));
    }
    serde_json::from_str::<Value>(body).map_err(|_| {
        ActivationRequestError::Fatal(format!(
            "微信 ClawBot {action}服务返回了无法解析的响应，请重新扫码"
        ))
    })
}

fn validate_activation_response(
    payload: &Value,
    action: &str,
    contract: ActivationResponseContract,
) -> Result<(), String> {
    let message = remote_error_message(payload);
    if let Some(result) = response_code(payload, "ret") {
        if result != 0 {
            return Err(format!(
                "微信 ClawBot {action}失败（{result}）：{}",
                bounded_remote_message(message)
            ));
        }
    } else if matches!(contract, ActivationResponseContract::Strict) {
        return Err(format!(
            "微信 ClawBot {action}服务没有返回明确结果，请重新扫码"
        ));
    }

    for key in ["errcode", "err_code"] {
        if let Some(errcode) = response_code(payload, key)
            && errcode != 0
        {
            return Err(format!(
                "微信 ClawBot {action}失败（{errcode}）：{}",
                bounded_remote_message(message)
            ));
        }
    }
    Ok(())
}

fn wechat_claw_token_stale(payload: &Value) -> bool {
    ["ret", "errcode", "err_code"]
        .into_iter()
        .any(|key| response_code(payload, key) == Some(-14))
}

fn activation_context(payload: &Value, expected_recipient_id: &str) -> Option<(String, String)> {
    let messages = response_messages(payload)?;
    let expected = expected_recipient_id.trim();
    messages.iter().find_map(|message| {
        let from_user_id = message_string(message, "from_user_id")?;
        if !expected.is_empty() && from_user_id != expected {
            return None;
        }
        let context_token = message_string(message, "context_token")?;
        Some((from_user_id.to_string(), context_token.to_string()))
    })
}

fn message_string<'a>(message: &'a Value, field: &str) -> Option<&'a str> {
    message
        .get(field)
        .and_then(Value::as_str)
        .or_else(|| {
            message
                .get("msg")
                .and_then(|nested| nested.get(field))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn response_updates_buffer(payload: &Value) -> Option<&str> {
    response_string(payload, &["get_updates_buf", "sync_buf"])
}

fn response_messages(payload: &Value) -> Option<&Vec<Value>> {
    for key in ["msgs", "messages", "message_list", "updates"] {
        if let Some(messages) = payload.get(key).and_then(Value::as_array) {
            return Some(messages);
        }
    }
    for key in ["data", "result", "body", "payload"] {
        if let Some(messages) = payload.get(key).and_then(response_messages) {
            return Some(messages);
        }
    }
    None
}

fn response_string<'a>(payload: &'a Value, keys: &[&str]) -> Option<&'a str> {
    for key in keys {
        if let Some(value) = payload
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value);
        }
    }
    for key in ["data", "result", "body", "payload"] {
        if let Some(value) = payload
            .get(key)
            .and_then(|nested| response_string(nested, keys))
        {
            return Some(value);
        }
    }
    None
}

fn response_code(payload: &Value, key: &str) -> Option<i64> {
    payload.get(key).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_str()?.trim().parse::<i64>().ok())
    })
}

fn remote_error_message(payload: &Value) -> &str {
    response_string(payload, &["errmsg", "error_message", "err_msg", "message"])
        .unwrap_or("未知错误")
}

fn bounded_remote_message(message: &str) -> String {
    let normalized = message.split_whitespace().collect::<Vec<_>>().join(" ");
    let value = normalized.chars().take(160).collect::<String>();
    if value.is_empty() {
        "未知错误".to_string()
    } else {
        value
    }
}

impl WechatClawLoginState {
    fn remove_expired(&mut self) {
        self.sessions
            .retain(|_, session| session.created_at.elapsed() < LOGIN_TIMEOUT);
    }
}

fn endpoint_url(base_url: &str, endpoint: &str) -> Result<Url, String> {
    let base_url = validate_base_url(base_url)?;
    Url::parse(&base_url)
        .map_err(|_| "微信 ClawBot 服务地址无效".to_string())?
        .join(endpoint)
        .map_err(|_| "微信 ClawBot 服务地址无效".to_string())
}

fn get_bot_qrcode_request(client: &Client) -> Result<reqwest::RequestBuilder, String> {
    let url = endpoint_url(ILINK_BASE_URL, "ilink/bot/get_bot_qrcode")?;
    Ok(client
        .post(url)
        .query(&[("bot_type", "3")])
        .headers(ilink::headers(None))
        // The official client accepts a list of known local bot tokens here.
        // Codey intentionally keeps this isolated notification binding stateless.
        .json(&json!({"local_token_list": []})))
}

fn redirect_base_url(payload: &Value) -> Result<Option<String>, String> {
    let Some(host) = payload
        .get("redirect_host")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    validate_base_url(&format!("https://{host}"))
        .map(Some)
        .map_err(|_| "微信 ClawBot 返回了不受信任的登录地址".to_string())
}

fn validate_base_url(value: &str) -> Result<String, String> {
    let config = NotificationChannelConfig {
        url: value.trim().to_string(),
        ..NotificationChannelConfig::default()
    };
    config
        .wechat_claw_base_url()
        .map(|url| url.as_str().trim_end_matches('/').to_string())
        .map_err(ToString::to_string)
}

fn qr_code_scan_payload(payload: &Value) -> Result<&str, String> {
    let value = payload
        .get("qrcode_img_content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "微信 ClawBot 登录服务没有返回二维码内容，请重新开始扫码".to_string())?;
    let url = Url::parse(value)
        .map_err(|_| "微信 ClawBot 返回了无效的二维码内容，请重新开始扫码".to_string())?;
    let host = url.host_str().unwrap_or_default();
    let official_host = host == "weixin.qq.com" || host.ends_with(".weixin.qq.com");
    if url.scheme() != "https"
        || !official_host
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("微信 ClawBot 返回了不受信任的二维码内容，请重新开始扫码".to_string());
    }
    Ok(value)
}

fn qr_code_image_data_uri(value: &str) -> Result<String, String> {
    let code = QrCode::new(value.as_bytes())
        .map_err(|_| "无法生成微信 ClawBot 登录二维码，请重新开始扫码".to_string())?;
    let image = code.render::<svg::Color>().module_dimensions(1, 1).build();
    Ok(format!(
        "data:image/svg+xml;base64,{}",
        STANDARD.encode(image)
    ))
}

async fn login_response_json(response: reqwest::Response) -> Result<Value, String> {
    if !response.status().is_success() {
        return Err(format!(
            "微信 ClawBot 登录服务返回 HTTP {}",
            response.status()
        ));
    }
    response
        .json::<Value>()
        .await
        .map_err(|_| "微信 ClawBot 登录服务返回了无法解析的响应".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_test_request_path(stream: &mut TcpStream) -> String {
        let mut request = Vec::new();
        loop {
            let mut chunk = [0_u8; 1_024];
            let read = stream.read(&mut chunk).await.unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(request)
            .unwrap()
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap()
            .to_string()
    }

    async fn write_test_json_response(stream: &mut TcpStream, body: &str) {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
    }

    fn configured_wechat_claw_channel(id: &str) -> NotificationChannelConfig {
        NotificationChannelConfig {
            id: id.to_string(),
            kind: NotificationChannelKind::WechatClaw,
            enabled: true,
            url: "https://ilinkai.weixin.qq.com".to_string(),
            bot_token: "ilink-secret-token".to_string(),
            context_token: "context-secret-token".to_string(),
            chat_id: "recipient@im.wechat".to_string(),
            ..NotificationChannelConfig::default()
        }
    }

    #[test]
    fn saved_enabled_clawbot_channels_are_sync_targets() {
        let mut config = CodeyConfig::default();
        let mut disabled = configured_wechat_claw_channel("disabled-claw");
        disabled.enabled = false;
        let mut missing_context = configured_wechat_claw_channel("missing-context");
        missing_context.context_token.clear();
        config.webhook.channels = vec![
            disabled,
            missing_context,
            configured_wechat_claw_channel("active-claw"),
        ];

        let channels = configured_wechat_claw_sync_channels(&config);

        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].id, "active-claw");
        assert_eq!(channels[0].get_updates_buf, "");
    }

    #[test]
    fn clawbot_sync_fingerprint_changes_with_binding_credentials() {
        let channel = WechatClawSyncChannel {
            id: "claw".to_string(),
            base_url: Url::parse("https://ilinkai.weixin.qq.com").unwrap(),
            bot_token: "token-1".to_string(),
            chat_id: "recipient@im.wechat".to_string(),
            context_token: "context-1".to_string(),
            get_updates_buf: "cursor-1".to_string(),
        };
        let mut changed_token = channel.clone();
        changed_token.bot_token = "token-2".to_string();
        let mut changed_cursor = channel.clone();
        changed_cursor.get_updates_buf = "cursor-2".to_string();

        assert_ne!(
            wechat_claw_sync_fingerprint(std::slice::from_ref(&channel)),
            wechat_claw_sync_fingerprint(&[changed_token])
        );
        assert_eq!(
            wechat_claw_sync_fingerprint(std::slice::from_ref(&channel)),
            wechat_claw_sync_fingerprint(&[changed_cursor])
        );
    }

    #[test]
    fn clawbot_stale_token_guard_only_pauses_the_matching_binding() {
        let channel = WechatClawSyncChannel {
            id: "claw".to_string(),
            base_url: Url::parse("https://ilinkai.weixin.qq.com").unwrap(),
            bot_token: "token-1".to_string(),
            chat_id: "recipient@im.wechat".to_string(),
            context_token: "context-1".to_string(),
            get_updates_buf: "cursor-1".to_string(),
        };
        let mut replacement = channel.clone();
        replacement.bot_token = "token-2".to_string();
        let mut guard = WechatClawSessionGuard::default();

        guard.pause(&channel);

        assert!(guard.remaining(&channel).is_some());
        assert!(guard.remaining(&replacement).is_none());
    }

    #[tokio::test]
    async fn clawbot_sync_service_replaces_a_finished_matching_worker() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = CodeyConfig::default();
        let mut channel = configured_wechat_claw_channel("recover-claw");
        channel.url = format!("http://{}/", listener.local_addr().unwrap());
        channel.allow_insecure_test_url = true;
        config.webhook.channels = vec![channel];
        let state = Arc::new(AppState {
            config: tokio::sync::RwLock::new(config),
            ..AppState::default()
        });
        let fingerprint = {
            let config = state.config.read().await;
            wechat_claw_sync_fingerprint(&configured_wechat_claw_sync_channels(&config))
        };
        let (shutdown, _shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async {});
        tokio::task::yield_now().await;
        assert!(task.is_finished());
        *state.wechat_claw_sync.lock().await = Some(WechatClawSyncHandle {
            fingerprint,
            shutdown,
            task,
        });

        sync_wechat_claw_service(&state).await;

        assert!(
            state
                .wechat_claw_sync
                .lock()
                .await
                .as_ref()
                .is_some_and(|handle| !handle.task.is_finished())
        );
        stop_wechat_claw_service(&state).await;
        drop(listener);
    }

    #[test]
    fn clawbot_sync_backoff_is_capped() {
        assert_eq!(sync_backoff(1), SYNC_INITIAL_BACKOFF);
        assert_eq!(sync_backoff(100), SYNC_MAX_BACKOFF);
    }

    #[tokio::test]
    async fn clawbot_sync_progress_is_persisted_without_bumping_revision() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = CodeyConfig::default();
        let mut channel = configured_wechat_claw_channel("persisted-claw");
        channel.get_updates_buf = "old-cursor".to_string();
        config.webhook.channels = vec![channel.clone()];
        config.settings_revision = 9;
        let state = Arc::new(AppState {
            store: crate::config::ConfigStore::new(directory.path().join("config.json")),
            config: tokio::sync::RwLock::new(config),
            ..AppState::default()
        });
        let sync_channel = WechatClawSyncChannel {
            id: channel.id,
            base_url: Url::parse("https://ilinkai.weixin.qq.com").unwrap(),
            bot_token: channel.bot_token,
            chat_id: channel.chat_id,
            context_token: channel.context_token,
            get_updates_buf: "old-cursor".to_string(),
        };

        persist_wechat_claw_sync_update(
            &state,
            &sync_channel,
            WechatClawSyncSuccess {
                get_updates_buf: "new-cursor".to_string(),
                context_token: Some("new-context".to_string()),
            },
        )
        .await
        .unwrap();

        let memory = state.config.read().await.clone();
        let disk = state.store.load().unwrap();
        assert_eq!(memory.settings_revision, 9);
        assert_eq!(disk.settings_revision, 9);
        assert_eq!(memory.webhook.channels[0].get_updates_buf, "new-cursor");
        assert_eq!(memory.webhook.channels[0].context_token, "new-context");
        assert_eq!(disk.webhook.channels[0].get_updates_buf, "new-cursor");
        assert_eq!(disk.webhook.channels[0].context_token, "new-context");
    }

    #[tokio::test]
    async fn clawbot_sync_service_starts_long_polling_and_persists_progress() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let next_update = json!({
            "get_updates_buf": "cursor-after-restart",
            "msgs": [{
                "from_user_id": "recipient@im.wechat",
                "context_token": "context-after-restart",
            }],
        })
        .to_string();
        let server = tokio::spawn(async move {
            let mut paths = Vec::new();
            for body in ["{}".to_string(), next_update, "{}".to_string()] {
                let (mut stream, _) = listener.accept().await.unwrap();
                paths.push(read_test_request_path(&mut stream).await);
                write_test_json_response(&mut stream, &body).await;
            }
            paths
        });

        let directory = tempfile::tempdir().unwrap();
        let mut config = CodeyConfig {
            settings_revision: 12,
            ..CodeyConfig::default()
        };
        let mut channel = configured_wechat_claw_channel("restart-claw");
        channel.url = format!("http://{address}/");
        channel.allow_insecure_test_url = true;
        channel.get_updates_buf = "cursor-before-restart".to_string();
        config.webhook.channels = vec![channel];
        let state = Arc::new(AppState {
            store: crate::config::ConfigStore::new(directory.path().join("config.json")),
            config: tokio::sync::RwLock::new(config),
            ..AppState::default()
        });

        sync_wechat_claw_service(&state).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if state.config.read().await.webhook.channels[0].get_updates_buf
                    == "cursor-after-restart"
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("sync progress should be persisted");
        stop_wechat_claw_service(&state).await;
        let paths = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("sync start, poll, and stop requests should complete")
            .unwrap();

        assert_eq!(
            paths,
            [
                "/ilink/bot/msg/notifystart",
                "/ilink/bot/getupdates",
                "/ilink/bot/msg/notifystop"
            ]
        );
        let memory = state.config.read().await.clone();
        let disk = state.store.load().unwrap();
        assert_eq!(memory.settings_revision, 12);
        assert_eq!(disk.settings_revision, 12);
        assert_eq!(
            memory.webhook.channels[0].context_token,
            "context-after-restart"
        );
        assert_eq!(
            disk.webhook.channels[0].get_updates_buf,
            "cursor-after-restart"
        );
    }

    #[tokio::test]
    async fn clawbot_sync_keeps_the_started_session_after_a_regular_poll_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut paths = Vec::new();
            for body in ["{}", r#"{"ret":-2,"errmsg":"prepare failed"}"#] {
                let (mut stream, _) = listener.accept().await.unwrap();
                paths.push(read_test_request_path(&mut stream).await);
                write_test_json_response(&mut stream, body).await;
            }
            paths
        });
        let channel = WechatClawSyncChannel {
            id: "retry-claw".to_string(),
            base_url: Url::parse(&format!("http://{address}/")).unwrap(),
            bot_token: "bot-token".to_string(),
            chat_id: "recipient@im.wechat".to_string(),
            context_token: "context-token".to_string(),
            get_updates_buf: "cursor".to_string(),
        };

        let error = sync_wechat_claw_channel(&Client::new(), channel, false)
            .await
            .unwrap_err();

        assert!(error.notify_started);
        assert!(!error.token_stale);
        assert_eq!(
            server.await.unwrap(),
            ["/ilink/bot/msg/notifystart", "/ilink/bot/getupdates"]
        );
    }

    #[tokio::test]
    async fn clawbot_stale_token_enters_cooldown_without_clearing_the_binding() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut paths = Vec::new();
            for body in ["{}", r#"{"errcode":-14,"errmsg":"token expired"}"#, "{}"] {
                let (mut stream, _) = listener.accept().await.unwrap();
                paths.push(read_test_request_path(&mut stream).await);
                write_test_json_response(&mut stream, body).await;
            }
            paths
        });

        let directory = tempfile::tempdir().unwrap();
        let mut config = CodeyConfig {
            settings_revision: 13,
            ..CodeyConfig::default()
        };
        let mut channel = configured_wechat_claw_channel("expired-claw");
        channel.url = format!("http://{address}/");
        channel.allow_insecure_test_url = true;
        channel.bot_token_configured = true;
        channel.context_token_configured = true;
        channel.get_updates_buf = "expired-cursor".to_string();
        config.webhook.channels = vec![channel.clone()];
        let store = crate::config::ConfigStore::new(directory.path().join("config.json"));
        store.save(&config).unwrap();
        let state = Arc::new(AppState {
            store,
            config: tokio::sync::RwLock::new(config),
            ..AppState::default()
        });

        sync_wechat_claw_service(&state).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if wechat_claw_notification_cooldown_remaining(&state, &channel)
                    .await
                    .is_some()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("stale token should enter cooldown");
        stop_wechat_claw_service(&state).await;
        let paths = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("sync start, stale response, and stop requests should complete")
            .unwrap();

        assert_eq!(
            paths,
            [
                "/ilink/bot/msg/notifystart",
                "/ilink/bot/getupdates",
                "/ilink/bot/msg/notifystop",
            ]
        );
        let memory = state.config.read().await.clone();
        let disk = state.store.load().unwrap();
        for saved in [&memory.webhook.channels[0], &disk.webhook.channels[0]] {
            assert_eq!(saved.bot_token, channel.bot_token);
            assert!(saved.bot_token_configured);
            assert_eq!(saved.context_token, channel.context_token);
            assert!(saved.context_token_configured);
            assert_eq!(saved.chat_id, channel.chat_id);
            assert_eq!(saved.get_updates_buf, channel.get_updates_buf);
            assert_eq!(
                saved.session_status,
                NotificationChannelSessionStatus::Active
            );
        }
        assert_eq!(memory.settings_revision, 13);
        assert_eq!(disk.settings_revision, 13);
    }

    #[test]
    fn login_urls_are_pinned_to_official_https_hosts() {
        assert_eq!(
            validate_base_url("https://ilinkai.weixin.qq.com/").unwrap(),
            "https://ilinkai.weixin.qq.com"
        );
        assert!(validate_base_url("https://region.weixin.qq.com").is_ok());
        assert!(validate_base_url("http://ilinkai.weixin.qq.com").is_err());
        assert!(validate_base_url("https://weixin.qq.com.evil.example").is_err());
        assert!(validate_base_url("https://ilinkai.weixin.qq.com/path").is_err());
    }

    #[test]
    fn redirect_hosts_cannot_escape_the_official_domain() {
        assert_eq!(
            redirect_base_url(&json!({"redirect_host":"region.weixin.qq.com"})).unwrap(),
            Some("https://region.weixin.qq.com".to_string())
        );
        assert!(redirect_base_url(&json!({"redirect_host":"evil.example"})).is_err());
        assert!(
            redirect_base_url(&json!({"redirect_host":"region.weixin.qq.com/escape"})).is_err()
        );
    }

    #[test]
    fn qr_code_image_is_generated_locally_as_svg_data_uri() {
        let payload = json!({
            "qrcode": "opaque-status-poll-token",
            "qrcode_img_content": "https://login.weixin.qq.com/l/login-fixture"
        });
        let scan_payload = qr_code_scan_payload(&payload).unwrap();
        assert_eq!(scan_payload, "https://login.weixin.qq.com/l/login-fixture");

        let image = qr_code_image_data_uri(scan_payload).unwrap();
        let encoded = image
            .strip_prefix("data:image/svg+xml;base64,")
            .expect("SVG data URI");
        let svg = String::from_utf8(STANDARD.decode(encoded).unwrap()).unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("<path"));
        assert!(svg.contains("shape-rendering=\"crispEdges\""));
    }

    #[test]
    fn qr_code_scan_payload_requires_an_official_https_url() {
        for rejected in [
            json!({"qrcode_img_content":"http://login.weixin.qq.com/l/code"}),
            json!({"qrcode_img_content":"https://weixin.qq.com.evil.example/l/code"}),
            json!({"qrcode_img_content":"https://user@login.weixin.qq.com/l/code"}),
            json!({"qrcode_img_content":"opaque-status-poll-token"}),
        ] {
            assert!(qr_code_scan_payload(&rejected).is_err());
        }
    }

    #[test]
    fn login_state_expires_old_qr_codes() {
        let mut state = WechatClawLoginState::default();
        state.sessions.insert(
            "old".to_string(),
            PendingWechatClawLogin {
                base_url: ILINK_BASE_URL.to_string(),
                created_at: Instant::now() - LOGIN_TIMEOUT,
                poll_in_flight: false,
                phase: WechatClawLoginPhase::Qr {
                    qr_code: "qr".to_string(),
                },
            },
        );
        state.remove_expired();
        assert!(state.sessions.is_empty());
    }

    #[test]
    fn login_headers_include_the_required_ilink_identifiers() {
        let headers = ilink::headers(None);
        assert_eq!(headers["authorizationtype"], "ilink_bot_token");
        assert_eq!(headers["ilink-app-id"], "bot");
        assert!(headers.contains_key("x-wechat-uin"));
        assert!(!headers.contains_key("authorization"));
    }

    #[test]
    fn qr_code_request_uses_the_current_official_post_contract() {
        let request = get_bot_qrcode_request(&Client::new())
            .unwrap()
            .build()
            .unwrap();

        assert_eq!(request.method(), reqwest::Method::POST);
        assert_eq!(
            request.url().as_str(),
            "https://ilinkai.weixin.qq.com/ilink/bot/get_bot_qrcode?bot_type=3"
        );
        assert_eq!(request.headers()["authorizationtype"], "ilink_bot_token");
        let body = request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
            .unwrap();
        assert_eq!(body["local_token_list"], json!([]));
    }

    #[test]
    fn activation_requests_use_notify_start_then_buffered_get_updates() {
        let client = Client::new();
        let notify = notify_start_request(&client, ILINK_BASE_URL, "secret")
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            notify.url().as_str(),
            "https://ilinkai.weixin.qq.com/ilink/bot/msg/notifystart"
        );
        assert_eq!(notify.headers()["authorization"], "Bearer secret");

        let updates = get_updates_request(&client, ILINK_BASE_URL, "secret", "next-buffer")
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            updates.url().as_str(),
            "https://ilinkai.weixin.qq.com/ilink/bot/getupdates"
        );
        let body = updates
            .body()
            .and_then(reqwest::Body::as_bytes)
            .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
            .unwrap();
        assert_eq!(body["get_updates_buf"], "next-buffer");
        assert!(
            body["base_info"]["bot_agent"]
                .as_str()
                .is_some_and(|value| value.starts_with("Codey/"))
        );
    }

    #[test]
    fn activation_only_accepts_an_inbound_context_for_the_bound_user() {
        let payload = json!({
            "ret": 0,
            "get_updates_buf": "next",
            "msgs": [
                {"from_user_id":"other@im.wechat", "context_token":"other-context"},
                {"from_user_id":"user@im.wechat", "context_token":"user-context"}
            ]
        });

        assert_eq!(
            activation_context(&payload, "user@im.wechat"),
            Some(("user@im.wechat".to_string(), "user-context".to_string()))
        );
        assert_eq!(response_updates_buffer(&payload), Some("next"));
        assert_eq!(
            activation_context(&payload, ""),
            Some(("other@im.wechat".to_string(), "other-context".to_string()))
        );
    }

    #[test]
    fn activation_parses_nested_messages_and_legacy_sync_buffers() {
        let payload = json!({
            "ret": 0,
            "data": {
                "sync_buf": "legacy-next",
                "updates": [{
                    "msg": {
                        "from_user_id": "user@im.wechat",
                        "context_token": "nested-context"
                    }
                }]
            }
        });

        assert_eq!(
            activation_context(&payload, "user@im.wechat"),
            Some(("user@im.wechat".to_string(), "nested-context".to_string()))
        );
        assert_eq!(response_updates_buffer(&payload), Some("legacy-next"));
    }

    #[test]
    fn notify_start_responses_require_explicit_success_fields() {
        assert!(
            validate_activation_response(
                &json!({"ret":0}),
                "激活",
                ActivationResponseContract::Strict
            )
            .is_ok()
        );
        assert!(
            validate_activation_response(
                &json!({"ret":0,"errcode":0}),
                "激活",
                ActivationResponseContract::Strict,
            )
            .is_ok()
        );
        assert!(
            validate_activation_response(&json!({}), "激活", ActivationResponseContract::Strict)
                .is_err()
        );
        assert!(
            validate_activation_response(
                &json!({"ret":-2,"errmsg":"prepare failed"}),
                "激活",
                ActivationResponseContract::Strict,
            )
            .is_err()
        );
    }

    #[test]
    fn get_updates_responses_allow_empty_long_poll_results() {
        for payload in [
            json!({}),
            json!({"get_updates_buf":"next"}),
            json!({"ret":0}),
            json!({"err_code":0,"messages":[]}),
            json!({"data":{"sync_buf":"nested-next","updates":[]}}),
        ] {
            assert!(
                validate_activation_response(
                    &payload,
                    "消息同步",
                    ActivationResponseContract::GetUpdates,
                )
                .is_ok()
            );
        }
    }

    #[test]
    fn get_updates_accepts_an_empty_http_success_body_but_strict_calls_do_not() {
        assert_eq!(
            parse_activation_response_body(
                "  \n",
                "后台消息同步",
                ActivationResponseContract::GetUpdates,
            )
            .unwrap(),
            json!({})
        );
        assert!(matches!(
            parse_activation_response_body("", "激活", ActivationResponseContract::Strict),
            Err(ActivationRequestError::Fatal(_))
        ));
    }

    #[test]
    fn get_updates_responses_reject_explicit_remote_errors() {
        assert!(
            validate_activation_response(
                &json!({"ret":-14,"errmsg":"token expired"}),
                "消息同步",
                ActivationResponseContract::GetUpdates,
            )
            .is_err()
        );
        assert!(
            validate_activation_response(
                &json!({"err_code":"-2","err_msg":"prepare failed"}),
                "消息同步",
                ActivationResponseContract::GetUpdates,
            )
            .is_err()
        );
    }
}
