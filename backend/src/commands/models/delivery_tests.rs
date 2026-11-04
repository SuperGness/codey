use super::*;
use crate::launcher::CodeyRuntime;
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

struct DeliveryAttempt {
    catalog: Value,
    reply: oneshot::Sender<Value>,
}

#[tokio::test]
async fn background_apply_coalesces_queued_saves_and_ignores_old_results() {
    use crate::commands::config_apply;
    let mut renderer = Renderer::start().await;
    let initial = config_with_models(&["old"]);
    let state = state_with_runtime(&initial, &renderer).await;
    let lifecycle = state.runtime_operation.lock().await;
    for name in ["middle", "latest"] {
        let _commit = state.config_write_lock.lock().await;
        *state.config.write().await = config_with_models(&[name]);
        config_apply::enqueue(&state, false);
    }
    drop(lifecycle);
    let first = renderer.next().await;
    assert_models(&first.catalog, &["latest"]);
    let newest = {
        let _commit = state.config_write_lock.lock().await;
        *state.config.write().await = config_with_models(&["newest"]);
        config_apply::enqueue(&state, false)
    };
    first.reply.send(json!({"ok": true})).unwrap();
    let second = renderer.next().await;
    assert_models(&second.catalog, &["newest"]);
    assert_eq!(config_apply::status(&state), newest);
    second
        .reply
        .send(json!({"ok": false, "error": "test renderer rejected update"}))
        .unwrap();
    let rollback = renderer.next().await;
    assert_models(&rollback.catalog, &["latest"]);
    rollback.reply.send(json!({"ok": true})).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let result = config_apply::status(&state);
            if result["state"] != "pending" {
                assert_eq!(result["revision"], newest["revision"]);
                assert_eq!(result["state"], "failed");
                assert!(
                    result["error"]
                        .as_str()
                        .unwrap()
                        .contains("test renderer rejected")
                );
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let latest = state.config.read().await.clone();
    assert!(!runtime_config_requires_restart(&state, &latest).await);
    assert_eq!(
        crate::commands::runtime_status(&state).await.unwrap()["restartRequired"],
        false
    );
}

#[tokio::test]
async fn background_model_delivery_only_waits_for_application_not_restart() {
    use crate::commands::config_apply;
    for delivered in ["active", "deferred"] {
        let mut renderer = Renderer::start().await;
        let initial = config_with_models(&["old"]);
        let latest = config_with_models(&["new"]);
        let state = state_with_runtime(&initial, &renderer).await;
        let lifecycle = state.runtime_operation.lock().await;
        {
            let _commit = state.config_write_lock.lock().await;
            *state.config.write().await = latest.clone();
            config_apply::enqueue(&state, false);
        }
        assert!(!runtime_config_requires_restart(&state, &latest).await);
        assert_eq!(
            crate::commands::runtime_status(&state).await.unwrap()["restartRequired"],
            false
        );
        drop(lifecycle);
        let attempt = renderer.next().await;
        assert_models(&attempt.catalog, &["new"]);
        attempt
            .reply
            .send(json!({"ok": true, "delivered": delivered}))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while config_apply::status(&state)["state"] == "pending" {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(config_apply::status(&state)["state"], "applied");
        assert!(!runtime_config_requires_restart(&state, &latest).await);
    }
}

#[tokio::test]
async fn background_apply_skips_renderer_for_unchanged_catalog() {
    use crate::commands::config_apply;
    let mut renderer = Renderer::start().await;
    let initial = config_with_models(&["old"]);
    let state = state_with_runtime(&initial, &renderer).await;
    {
        let _commit = state.config_write_lock.lock().await;
        state.config.write().await.user_scripts = vec!["// requires restart".into()];
        config_apply::enqueue(&state, false);
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let result = config_apply::status(&state);
            if result["state"] != "pending" {
                assert_eq!(result["state"], "restart_required");
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(renderer.attempts.try_recv().is_err());
    assert_eq!(
        crate::commands::runtime_status(&state).await.unwrap()["restartRequired"],
        true
    );
}

struct Renderer {
    url: String,
    attempts: mpsc::UnboundedReceiver<DeliveryAttempt>,
    task: tokio::task::JoinHandle<()>,
}

impl Renderer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (sender, attempts) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
                let request: Value =
                    serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                        .unwrap();
                assert_eq!(request["method"], "Runtime.evaluate");
                let expression = request["params"]["expression"].as_str().unwrap();
                let catalog = expression
                    .split_once("const expectedCatalog = ")
                    .unwrap()
                    .1
                    .split_once(";\n")
                    .unwrap()
                    .0;
                let (reply, receiver) = oneshot::channel();
                sender
                    .send(DeliveryAttempt {
                        catalog: serde_json::from_str(catalog).unwrap(),
                        reply,
                    })
                    .unwrap();
                let report = receiver.await.unwrap();
                socket
                    .send(Message::Text(
                        json!({
                            "id": request["id"],
                            "result": {"result": {"type": "string", "value": report.to_string()}},
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
        });
        Self {
            url,
            attempts,
            task,
        }
    }

    async fn next(&mut self) -> DeliveryAttempt {
        tokio::time::timeout(Duration::from_secs(5), self.attempts.recv())
            .await
            .expect("模型目录应在限定时间内送达")
            .unwrap()
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn config_with_models(models: &[&str]) -> CodeyConfig {
    let mut profile = ProviderProfile::new("Relay");
    profile.id = "delivery-test".into();
    profile.base_url = "https://relay.example/v1".into();
    profile.api_key = "test-key".into();
    profile.normalize();
    CodeyConfig {
        local_router_enabled: true,
        active_profile_id: profile.id.clone(),
        selected_models_by_provider: BTreeMap::from([(
            profile.provider_id().to_string(),
            models.iter().map(|model| (*model).to_string()).collect(),
        )]),
        profiles: vec![profile],
        ..CodeyConfig::default()
    }
    .normalize()
}

fn assert_models(catalog: &Value, models: &[&str]) {
    assert_eq!(
        catalog["models"],
        json!(
            models
                .iter()
                .map(|model| format!("delivery-test/{model}"))
                .collect::<Vec<_>>()
        )
    );
}

async fn state_with_runtime(config: &CodeyConfig, renderer: &Renderer) -> Arc<AppState> {
    Arc::new(AppState {
        config: tokio::sync::RwLock::new(config.clone()),
        runtime: tokio::sync::Mutex::new(Some(Arc::new(CodeyRuntime::for_model_delivery_test(
            config.clone(),
            &renderer.url,
            None,
        )))),
        ..AppState::default()
    })
}

#[tokio::test]
async fn queued_delivery_uses_latest_config_and_catalog_reads_wait_for_commit() {
    let mut renderer = Renderer::start().await;
    let initial = config_with_models(&["old"]);
    let state = state_with_runtime(&initial, &renderer).await;
    let guard = state.model_delivery_lock.lock().await;
    let reload = hot_reload_runtime_models(&state);
    tokio::pin!(reload);
    assert!(futures_util::poll!(reload.as_mut()).is_pending());
    let latest = config_with_models(&["old", "new"]);
    *state.config.write().await = latest.clone();
    drop(guard);

    let attempt = tokio::select! {
        _ = &mut reload => panic!("热更新必须等待渲染端确认"),
        attempt = renderer.next() => attempt,
    };
    assert_models(&attempt.catalog, &["old", "new"]);
    assert!(state.runtime_operation.try_lock().is_err());
    assert!(state.config_write_lock.try_lock().is_ok());
    assert!(state.config.try_write().is_ok());
    let catalog_read = state.bridge_request("/codex-model-catalog".into(), json!({}));
    tokio::pin!(catalog_read);
    assert!(futures_util::poll!(catalog_read.as_mut()).is_pending());

    attempt
        .reply
        .send(json!({"ok": true, "delivered": "active"}))
        .unwrap();
    assert!(reload.await.reloaded);
    assert_models(&catalog_read.await, &["old", "new"]);
    let runtime = state.runtime.lock().await.clone().unwrap();
    assert_eq!(runtime.applied_model_catalog_config().await, latest);
}

#[tokio::test]
async fn failed_delivery_restores_both_sides_and_retry_uses_saved_models() {
    let mut renderer = Renderer::start().await;
    let initial = config_with_models(&["old"]);
    let router = local_router::LocalRouter::start(&initial).await.unwrap();
    let routes = Arc::clone(&router.snapshot);
    let original_routes = routes.read().unwrap().clone();
    let state = state_with_runtime(&initial, &renderer).await;
    let runtime = Arc::new(CodeyRuntime::for_model_delivery_test(
        initial.clone(),
        &renderer.url,
        Some(router),
    ));
    *state.runtime.lock().await = Some(Arc::clone(&runtime));
    let latest = config_with_models(&["new"]);
    *state.config.write().await = latest.clone();
    let reload = hot_reload_runtime_models(&state);
    tokio::pin!(reload);

    let attempt = tokio::select! {
        _ = &mut reload => panic!("热更新必须等待确认"),
        attempt = renderer.next() => attempt,
    };
    assert_models(&attempt.catalog, &["new"]);
    assert!(!Arc::ptr_eq(&routes.read().unwrap(), &original_routes));
    attempt
        .reply
        .send(json!({"ok": false, "error": "cache write failed"}))
        .unwrap();
    let rollback = tokio::select! {
        _ = &mut reload => panic!("失败后必须恢复渲染端目录"),
        attempt = renderer.next() => attempt,
    };
    assert_models(&rollback.catalog, &["old"]);
    assert!(Arc::ptr_eq(&routes.read().unwrap(), &original_routes));
    rollback.reply.send(json!({"ok": true})).unwrap();
    let outcome = reload.await;
    assert!(!outcome.reloaded);
    assert!(outcome.error.unwrap().contains("cache write failed"));
    assert_eq!(*state.config.read().await, latest);
    assert_eq!(runtime.applied_model_catalog_config().await, initial);
    assert_models(
        &runtime_renderer_model_catalog(&state).await.unwrap(),
        &["old"],
    );

    let retry = hot_reload_runtime_models(&state);
    tokio::pin!(retry);
    let attempt = tokio::select! {
        _ = &mut retry => panic!("重试应重新送达"),
        attempt = renderer.next() => attempt,
    };
    assert_models(&attempt.catalog, &["new"]);
    attempt
        .reply
        .send(json!({"ok": true, "delivered": "deferred"}))
        .unwrap();
    let outcome = retry.await;
    assert!(outcome.reloaded && outcome.deferred);
    assert_eq!(runtime.applied_model_catalog_config().await, latest);
}

#[tokio::test]
async fn reload_waits_for_runtime_replacement_without_holding_config_locks() {
    let mut renderer = Renderer::start().await;
    let initial = config_with_models(&["old"]);
    let state = state_with_runtime(&initial, &renderer).await;
    let old_runtime = state.runtime.lock().await.clone().unwrap();
    let lifecycle = state.runtime_operation.lock().await;
    let reload = hot_reload_runtime_models(&state);
    tokio::pin!(reload);
    assert!(futures_util::poll!(reload.as_mut()).is_pending());
    assert!(state.model_delivery_lock.try_lock().is_ok());
    assert!(state.config_write_lock.try_lock().is_ok());
    let latest = config_with_models(&["new"]);
    *state.config.write().await = latest.clone();
    let replacement = Arc::new(CodeyRuntime::for_model_delivery_test(
        latest.clone(),
        &renderer.url,
        None,
    ));
    *state.runtime.lock().await = Some(Arc::clone(&replacement));
    drop(lifecycle);

    let attempt = tokio::select! {
        _ = &mut reload => panic!("应更新替换后的运行时"),
        attempt = renderer.next() => attempt,
    };
    assert_models(&attempt.catalog, &["new"]);
    attempt.reply.send(json!({"ok": true})).unwrap();
    assert!(reload.await.reloaded);
    assert_eq!(old_runtime.applied_model_catalog_config().await, initial);
    assert_eq!(replacement.applied_model_catalog_config().await, latest);
}

#[tokio::test]
async fn pending_transport_restart_still_delivers_models_with_launch_capabilities() {
    let mut renderer = Renderer::start().await;
    let initial = config_with_models(&["old"]);
    let state = state_with_runtime(&initial, &renderer).await;
    let mut latest = config_with_models(&["old", "new"]);
    latest.profiles[0].supports_websockets = true;
    *state.config.write().await = latest;
    let reload = hot_reload_runtime_models(&state);
    tokio::pin!(reload);
    let attempt = tokio::select! {
        _ = &mut reload => panic!("运输能力待重启不应阻止新增模型显示"),
        attempt = renderer.next() => attempt,
    };
    assert_models(&attempt.catalog, &["old", "new"]);
    attempt.reply.send(json!({"ok": true})).unwrap();
    assert!(reload.await.reloaded);
    let runtime = state.runtime.lock().await.clone().unwrap();
    assert!(!runtime.applied_model_catalog_config().await.profiles[0].supports_websockets);
    assert!(state.config.read().await.profiles[0].supports_websockets);
    assert_models(
        &runtime_renderer_model_catalog(&state).await.unwrap(),
        &["old", "new"],
    );
}

#[tokio::test]
async fn router_mode_change_does_not_publish_an_unapplied_catalog() {
    let renderer = Renderer::start().await;
    let initial = config_with_models(&["old"]);
    let state = state_with_runtime(&initial, &renderer).await;
    state.config.write().await.local_router_enabled = false;
    assert!(!hot_reload_runtime_models(&state).await.reloaded);
    assert_models(
        &runtime_renderer_model_catalog(&state).await.unwrap(),
        &["old"],
    );
}

#[tokio::test]
async fn renderer_replacement_retries_even_after_success_or_with_the_same_url() {
    for (old_success, same_url) in [(true, false), (false, false), (true, true), (false, true)] {
        let mut original = Renderer::start().await;
        let mut replacement = Renderer::start().await;
        let initial = config_with_models(&["old"]);
        let latest = config_with_models(&["new"]);
        let state = state_with_runtime(&initial, &original).await;
        let runtime = state.runtime.lock().await.clone().unwrap();
        *state.config.write().await = latest.clone();
        let reload = hot_reload_runtime_models(&state);
        tokio::pin!(reload);
        let attempt = tokio::select! {
            _ = &mut reload => panic!("热更新必须等待旧页面响应"),
            attempt = original.next() => attempt,
        };
        let current = if same_url {
            &mut original
        } else {
            &mut replacement
        };
        tokio::time::timeout(
            Duration::from_secs(1),
            runtime.replace_model_delivery_target_for_test(&current.url),
        )
        .await
        .expect("CDP 等待期间不能阻止页面目标切换");
        attempt
            .reply
            .send(json!({"ok": old_success, "error": "old renderer", "delivered": "active"}))
            .unwrap();
        let retry = tokio::select! {
            _ = &mut reload => panic!("目标切换后必须向当前页面重新推送"),
            attempt = current.next() => attempt,
        };
        assert_models(&retry.catalog, &["new"]);
        assert_eq!(runtime.applied_model_catalog_config().await, initial);
        retry
            .reply
            .send(json!({"ok": true, "delivered": "deferred"}))
            .unwrap();
        let outcome = reload.await;
        assert!(outcome.reloaded && outcome.deferred);
        assert!(outcome.error.is_none());
        assert_eq!(runtime.applied_model_catalog_config().await, latest);
    }
}

#[tokio::test]
async fn repeated_renderer_replacement_is_bounded_and_reports_rollback_failure() {
    let mut original = Renderer::start().await;
    let mut replacement = Renderer::start().await;
    let mut current = Renderer::start().await;
    let initial = config_with_models(&["old"]);
    let latest = config_with_models(&["new"]);
    let state = state_with_runtime(&initial, &original).await;
    let runtime = state.runtime.lock().await.clone().unwrap();
    *state.config.write().await = latest.clone();
    let reload = hot_reload_runtime_models(&state);
    tokio::pin!(reload);
    let attempt = tokio::select! {
        _ = &mut reload => panic!("应向原页面推送目录"),
        attempt = original.next() => attempt,
    };
    runtime
        .replace_model_delivery_target_for_test(&replacement.url)
        .await;
    attempt.reply.send(json!({"ok": true})).unwrap();
    let retry = tokio::select! {
        _ = &mut reload => panic!("应向替换后的页面重试"),
        attempt = replacement.next() => attempt,
    };
    assert_models(&retry.catalog, &["new"]);
    runtime
        .replace_model_delivery_target_for_test(&current.url)
        .await;
    retry.reply.send(json!({"ok": true})).unwrap();
    let rollback = tokio::select! {
        _ = &mut reload => panic!("连续目标切换后应恢复原目录"),
        attempt = current.next() => attempt,
    };
    assert_models(&rollback.catalog, &["old"]);
    rollback
        .reply
        .send(json!({"ok": false, "error": "rollback rejected"}))
        .unwrap();
    let outcome = reload.await;
    assert!(!outcome.reloaded);
    let error = outcome.error.unwrap();
    assert!(error.contains("页面连续重连"));
    assert!(error.contains("恢复原模型列表失败"));
    assert!(error.contains("rollback rejected"));
    assert_eq!(runtime.applied_model_catalog_config().await, initial);
    assert_eq!(*state.config.read().await, latest);
}

#[tokio::test]
async fn rollback_follows_renderer_replacement() {
    let mut original = Renderer::start().await;
    let mut replacement = Renderer::start().await;
    let initial = config_with_models(&["old"]);
    let state = state_with_runtime(&initial, &original).await;
    let runtime = state.runtime.lock().await.clone().unwrap();
    *state.config.write().await = config_with_models(&["new"]);
    let reload = hot_reload_runtime_models(&state);
    tokio::pin!(reload);
    let attempt = tokio::select! {
        _ = &mut reload => panic!("应推送新目录"),
        attempt = original.next() => attempt,
    };
    attempt
        .reply
        .send(json!({"ok": false, "error": "delivery rejected"}))
        .unwrap();
    let rollback = tokio::select! {
        _ = &mut reload => panic!("应恢复原目录"),
        attempt = original.next() => attempt,
    };
    assert_models(&rollback.catalog, &["old"]);
    runtime
        .replace_model_delivery_target_for_test(&replacement.url)
        .await;
    rollback
        .reply
        .send(json!({"ok": false, "error": "old renderer closed"}))
        .unwrap();
    let retry = tokio::select! {
        _ = &mut reload => panic!("恢复目录也应使用当前页面目标"),
        attempt = replacement.next() => attempt,
    };
    assert_models(&retry.catalog, &["old"]);
    retry.reply.send(json!({"ok": true})).unwrap();
    let outcome = reload.await;
    assert!(!outcome.reloaded);
    let error = outcome.error.unwrap();
    assert!(error.contains("delivery rejected"));
    assert!(!error.contains("恢复原模型列表失败"));
    assert_eq!(runtime.applied_model_catalog_config().await, initial);
}
