use tokio::sync::watch;
use tokio::task::JoinSet;

use super::*;
use crate::notifications::outbox::{DeliveryOutcome, DeliveryTarget, Outbox, OutboxDelivery};

pub(super) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub(super) async fn outbox(state: &AppState) -> Result<Outbox, String> {
    state
        .notification_outbox
        .get_or_try_init(|| async {
            Outbox::open(
                state
                    .store
                    .path()
                    .with_file_name("notification-outbox.sqlite3"),
            )
            .await
        })
        .await
        .cloned()
}

pub(super) fn binding_hash(channel: &NotificationChannelConfig) -> String {
    // ClawBot 的 context_token 与同步游标自动更新，不代表接收账号改变。
    // bot_token、接收人和服务地址仍纳入摘要，重新绑定后旧队列不能发送给新对象。
    crate::fs_util::sha256_hex_str(
        &serde_json::to_string(&(
            channel.kind,
            channel.url.trim(),
            channel.bot_token.trim(),
            channel.chat_id.trim(),
            if channel.kind == NotificationChannelKind::WechatClaw {
                ""
            } else {
                channel.context_token.trim()
            },
        ))
        .expect("notification binding serializes"),
    )
}

pub(super) async fn enqueue(
    state: &Arc<AppState>,
    key: String,
    event: NotificationEvent,
    channels: Vec<NotificationChannelConfig>,
) -> Result<Value, String> {
    let targets = channels
        .into_iter()
        .filter(|channel| channel.enabled && channel.is_configured())
        .map(|channel| DeliveryTarget {
            channel_id: channel.id.clone(),
            binding_hash: binding_hash(&channel),
        })
        .collect();
    let event_id = event.event_id.clone();
    let inserted = outbox(state)
        .await?
        .enqueue(key, event, targets, now_ms())
        .await?;
    state.notification_delivery_wake.notify_one();
    Ok(json!({"status": if inserted { "queued" } else { "duplicate" }, "eventId":event_id}))
}

pub(super) async fn start(state: Arc<AppState>) -> Result<tokio::task::JoinHandle<()>, String> {
    let queue = outbox(&state).await?;
    let stop = state.notification_delivery_stop.subscribe();
    Ok(tokio::spawn(run(state, queue, stop)))
}

async fn stopped(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow() || stop.changed().await.is_err() {
            return;
        }
    }
}

async fn wait_until(due: Option<i64>) {
    match due {
        Some(due) => {
            tokio::time::sleep(Duration::from_millis(
                due.saturating_sub(now_ms()).max(0) as u64
            ))
            .await
        }
        None => std::future::pending::<()>().await,
    }
}

async fn run(state: Arc<AppState>, queue: Outbox, mut stop: watch::Receiver<bool>) {
    let mut active = JoinSet::new();
    let mut busy = HashSet::new();
    loop {
        if *stop.borrow() {
            break;
        }
        let capacity = MAX_CONCURRENT_WEBHOOK_DELIVERIES.saturating_sub(active.len());
        if capacity > 0 {
            // 不在 select 中取消存储操作，避免后台事务在关闭后继续认领。
            let deliveries = match queue
                .claim_due(now_ms(), capacity, busy.iter().cloned().collect())
                .await
            {
                Ok(deliveries) => deliveries,
                Err(_) => {
                    record_storage_failure();
                    break;
                }
            };
            for delivery in deliveries {
                busy.insert(delivery.target.channel_id.clone());
                // 落盘等待期间，其他请求仍需及时读取响应并结束 HTTP 超时计时。
                active.spawn(deliver(Arc::clone(&state), delivery));
            }
        }
        if *stop.borrow() {
            break;
        }
        let due = if active.len() < MAX_CONCURRENT_WEBHOOK_DELIVERIES {
            match queue.next_due_ms(busy.iter().cloned().collect()).await {
                Ok(due) => due,
                Err(_) => {
                    record_storage_failure();
                    break;
                }
            }
        } else {
            None
        };
        tokio::select! {
            biased;
            _ = stopped(&mut stop) => break,
            Some(result) = active.join_next(), if !active.is_empty() => {
                let Ok((delivery, outcome, elapsed_ms)) = result else {
                    // 任务异常退出时停止投递，由下方统一处理未确认的认领。
                    break;
                };
                record_delivery(&delivery, outcome, elapsed_ms);
                if queue.finish(&delivery, outcome, now_ms()).await.is_err() {
                    record_storage_failure();
                    break;
                }
                busy.remove(&delivery.target.channel_id);
            }
            _ = state.notification_delivery_wake.notified() => {},
            _ = wait_until(due) => {},
        }
    }
    // 等网络任务全部退出后再提交不确定状态，避免取消期间继续发送。
    active.abort_all();
    while active.join_next().await.is_some() {}
    if queue.mark_inflight_uncertain(now_ms()).await.is_err() {
        record_storage_failure();
    }
}

async fn deliver(
    state: Arc<AppState>,
    delivery: OutboxDelivery,
) -> (OutboxDelivery, DeliveryOutcome, u64) {
    let started = std::time::Instant::now();
    let outcome = send_current(&state, &delivery).await;
    (
        delivery,
        outcome,
        started.elapsed().as_millis().min(u64::MAX as u128) as u64,
    )
}

pub(super) async fn send_current(
    state: &Arc<AppState>,
    delivery: &OutboxDelivery,
) -> DeliveryOutcome {
    if !matches!(
        delivery.event.event.as_str(),
        "session.completed" | "session.failed" | "session.waiting"
    ) {
        return DeliveryOutcome::Cancelled;
    }
    if delivery.event.event == "session.waiting"
        && state
            .notification_waiting_snapshot
            .read()
            .await
            .is_obsolete(&delivery.event.session_id, &delivery.event_key)
    {
        return DeliveryOutcome::Cancelled;
    }
    let channel = {
        let config = state.config.read().await;
        config
            .webhook
            .channels
            .iter()
            .find(|channel| channel.id == delivery.target.channel_id)
            .cloned()
    };
    let Some(channel) = channel.filter(|channel| {
        channel.enabled
            && channel.is_configured()
            && binding_hash(channel) == delivery.target.binding_hash
    }) else {
        return DeliveryOutcome::Cancelled;
    };
    if channel.kind == NotificationChannelKind::WechatClaw
        && let Some(remaining) =
            super::super::wechat_claw_notification_cooldown_remaining(state, &channel).await
    {
        return DeliveryOutcome::Retryable {
            retry_after_ms: Some(remaining.as_millis() as u64),
        };
    }
    let Ok(client) = current_notification_http_client(state).await else {
        return DeliveryOutcome::Retryable {
            retry_after_ms: None,
        };
    };
    if *state.notification_delivery_stop.borrow() {
        return DeliveryOutcome::Uncertain;
    }
    record_dispatch(delivery, channel.kind);
    let dispatcher = NotificationDispatcher::with_client(client, channel.clone());
    match dispatcher.send_once(&delivery.event).await {
        Ok(()) => DeliveryOutcome::Delivered,
        Err(error)
            if channel.kind == NotificationChannelKind::WechatClaw
                && error.indicates_stale_token() =>
        {
            let remaining = super::super::pause_wechat_claw_notification_channel(state, &channel)
                .await
                .unwrap_or_else(|| Duration::from_secs(60 * 60));
            DeliveryOutcome::Retryable {
                retry_after_ms: Some(remaining.as_millis() as u64),
            }
        }
        Err(error)
            if channel.kind == NotificationChannelKind::WechatClaw
                && error.should_retry_with_fresh_context() =>
        {
            // 明确拒绝后刷新上下文，下一次持久认领使用最新值，避免内部重试叠加。
            let token_already_refreshed =
                state
                    .config
                    .read()
                    .await
                    .webhook
                    .channels
                    .iter()
                    .any(|saved| {
                        saved.id == channel.id
                            && binding_hash(saved) == delivery.target.binding_hash
                            && saved.context_token.trim() != channel.context_token.trim()
                    });
            if !token_already_refreshed {
                let _ = super::super::refresh_wechat_claw_channel_context(state, &channel).await;
            }
            DeliveryOutcome::Retryable {
                retry_after_ms: None,
            }
        }
        Err(error) => error.outcome(),
    }
}

fn record_dispatch(delivery: &OutboxDelivery, kind: NotificationChannelKind) {
    let detail = json!({"deliveryKeyHash":delivery.delivery_key, "eventId":delivery.event.event_id,
        "channelType":kind, "attempt":delivery.attempt, "queueWaitMs":now_ms().saturating_sub(delivery.created_at_ms).max(0)});
    #[cfg(not(test))]
    let _ =
        codey_runtime_core::diagnostic_log::append_diagnostic_log("notification.dispatch", detail);
    #[cfg(test)]
    let _ = detail;
}

fn record_delivery(delivery: &OutboxDelivery, outcome: DeliveryOutcome, elapsed_ms: u64) {
    let outcome = match outcome {
        DeliveryOutcome::Delivered => "delivered",
        DeliveryOutcome::Retryable { .. } => "retry_scheduled",
        DeliveryOutcome::Uncertain => "uncertain",
        DeliveryOutcome::PermanentFailure => "permanent_failure",
        DeliveryOutcome::Cancelled => "cancelled",
    };
    let detail = json!({"deliveryKeyHash":delivery.delivery_key,"attempt":delivery.attempt,"outcome":outcome,"deliveryMs":elapsed_ms});
    #[cfg(not(test))]
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
        "notification.delivery_settled",
        detail,
    );
    #[cfg(test)]
    let _ = detail;
}

fn record_storage_failure() {
    #[cfg(not(test))]
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
        "notification.storage_error",
        json!({"sendingPaused":true}),
    );
}
