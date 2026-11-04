//! Persisting settings must never wait for renderer delivery or the lifecycle lock.
use super::*;

#[derive(Default)]
pub(super) struct ApplyQueue {
    revision: u64,
    running: bool,
    reconcile: bool,
    trace_requested: bool,
    status: Value,
}

pub(super) fn status(state: &AppState) -> Value {
    state
        .config_apply
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .status
        .clone()
}

pub(super) fn request_trace_apply(state: &AppState) {
    state
        .config_apply
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .trace_requested = true;
}

/// Called only after a successful launch while holding runtime_operation.
pub(super) async fn confirm_launch(state: &AppState, launched_config: &CodeyConfig) {
    let _commit = state.config_write_lock.lock().await;
    if *state.config.read().await != *launched_config {
        return;
    }
    if state
        .config_apply
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .trace_requested
        && state.trace_log_guard_state.applied_target()
            != Some(launched_config.disable_trace_log_writes)
    {
        return;
    }
    let mut queue = state.config_apply.lock().unwrap_or_else(|e| e.into_inner());
    if !queue.running && !queue.status.is_null() && queue.status["state"] != "applied" {
        // A launch resolves an earlier terminal result. Advance the revision so
        // clients can accept it without allowing stale terminal replies to win.
        queue.revision += 1;
        queue.status = json!({"revision":queue.revision,"state":"applied"});
    }
}

/// Call while holding config_write_lock, so revisions follow persistence order.
pub(super) fn enqueue(state: &Arc<AppState>, reconcile: bool) -> Value {
    let mut queue = state.config_apply.lock().unwrap_or_else(|e| e.into_inner());
    queue.revision += 1;
    queue.reconcile |= reconcile;
    queue.status = json!({"revision":queue.revision,"state":"pending"});
    if !queue.running {
        if let Some(handle) = state.runtime_handle.as_ref() {
            queue.running = true;
            let state = state.clone();
            handle.spawn(async move { run(state).await });
        } else {
            queue.status = json!({"revision":queue.revision,"state":"failed","error":"配置已保存，后台应用服务不可用，请重启 Codey"});
        }
    }
    queue.status.clone()
}

async fn run(state: Arc<AppState>) {
    loop {
        // Take the lifecycle lock before selecting work: a burst of saves during
        // startup becomes one delivery of the latest persisted configuration.
        let restart_settled = state.restart_settled.notified();
        tokio::pin!(restart_settled);
        restart_settled.as_mut().enable();
        let _lifecycle = state.runtime_operation.lock().await;
        if state.restart_in_progress.load(Ordering::Acquire) && !state.is_shutting_down() {
            drop(_lifecycle);
            restart_settled.await;
            continue;
        }
        let (revision, reconcile, config) = {
            let _commit = state.config_write_lock.lock().await;
            let config = state.config.read().await.clone();
            let mut queue = state.config_apply.lock().unwrap_or_else(|e| e.into_inner());
            (queue.revision, std::mem::take(&mut queue.reconcile), config)
        };
        let result = apply(&state, &config, reconcile).await;
        let mut queue = state.config_apply.lock().unwrap_or_else(|e| e.into_inner());
        if queue.revision == revision {
            queue.status = result;
            queue.status["revision"] = json!(revision);
            queue.running = false;
            return;
        }
        // A newer save must still reconcile changes requested by this delivery.
        queue.reconcile |= reconcile;
    }
}

async fn apply(state: &Arc<AppState>, config: &CodeyConfig, reconcile: bool) -> Value {
    if state.is_shutting_down() {
        return json!({"state":"saved"});
    }
    sync_waiting_webhook_watcher(state).await;
    sync_wechat_claw_service(state).await;
    schedule_crashpad_pending_refresh(state, config.protect_crashpad_pending);
    let home = codex_home().to_path_buf();
    #[cfg(test)]
    let home = state.slow_apply_test.trace_home.clone().unwrap_or(home);
    let trace_error = apply_trace_guard(state, config, home).await.err();
    let Some(runtime) = state.runtime.lock().await.clone() else {
        if let Some(error) = trace_error {
            return json!({"state":"failed","error":error});
        }
        return json!({"state":"saved"});
    };
    runtime.set_crashpad_pending_protection(config.protect_crashpad_pending);
    let models = models::hot_reload_runtime_models_locked(state, true).await;
    let logs = hot_reload_runtime_request_log_locked(state, config).await;
    let subagents = if config.subagent_optimization
        && (reconcile || !runtime.applied_subagent_config().await.matches(config))
    {
        hot_reload_runtime_subagent_config_locked(state, config).await
    } else {
        SubagentHotReloadOutcome::default()
    };
    if let Some(error) = trace_error
        .as_deref()
        .or(models.error.as_deref())
        .or(logs.error())
        .or_else(|| {
            (subagents.status != SubagentHotReloadStatus::PendingRestart)
                .then(|| subagents.error())
                .flatten()
        })
    {
        error_log::record_failure(
            "config_apply_failed",
            "apply_saved_config",
            error.to_string(),
            json!({}),
        );
        return json!({"state":"failed","error":error});
    }
    if subagents.requires_restart() || runtime_config_requires_restart(state, config).await {
        return json!({"state":"restart_required"});
    }
    let models = runtime.applied_model_config().await;
    let subagents = runtime.applied_subagent_config().await;
    if !models.matches(config)
        || ((runtime.applied_config.subagent_optimization || config.subagent_optimization)
            && !subagents.matches(config))
    {
        return json!({"state":"failed","error":"配置已保存，但当前 Codex 实例尚未完成热更新；可重新应用配置"});
    }
    json!({"state":"applied"})
}

async fn apply_trace_guard(
    state: &Arc<AppState>,
    config: &CodeyConfig,
    home: PathBuf,
) -> Result<(), String> {
    let desired = config.disable_trace_log_writes;
    let requested = state
        .config_apply
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .trace_requested;
    let applied = state.trace_log_guard_state.applied_target();
    if !requested && applied.is_none_or(|applied| applied == desired) {
        return Ok(());
    }
    let _diagnostic = state.diagnostic_storage_operation.lock().await;
    // A save may have superseded this snapshot while diagnostic cleanup ran.
    if state.config.read().await.disable_trace_log_writes != desired {
        return Ok(());
    }
    let result =
        configure_trace_log_guard(home, desired, state.trace_log_guard_state.clone()).await;
    match result {
        Ok(report) => {
            state
                .trace_log_write_protection_active
                .store(report.protection_active(desired), Ordering::Release);
            let _commit = state.config_write_lock.lock().await;
            if state.config.read().await.disable_trace_log_writes == desired {
                state
                    .config_apply
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .trace_requested = false;
            }
            Ok(())
        }
        Err(error) => {
            state
                .trace_log_write_protection_active
                .store(false, Ordering::Release);
            request_trace_apply(state);
            error_log::record_failure(
                "patch_failed",
                "configure_trace_log_guard",
                error.clone(),
                json!({"disabled":desired,"source":"config_apply"}),
            );
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn settled(state: &AppState) -> Value {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let value = status(state);
                if value["state"] != "pending" {
                    return value;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background configuration application should settle")
    }

    #[tokio::test]
    async fn save_and_route_toggle_return_while_launch_holds_lifecycle_lock() {
        let directory = tempfile::tempdir().unwrap();
        let initial = CodeyConfig::default();
        let state = Arc::new(AppState {
            store: ConfigStore::new(directory.path().join("config.json")),
            config: RwLock::new(initial.clone()),
            ..AppState::default()
        });
        let lifecycle = state.runtime_operation.lock().await;
        let saved =
            tokio::time::timeout(Duration::from_secs(3), save_codey_config(&state, initial))
                .await
                .expect("saving must not wait for startup")
                .unwrap();
        assert_eq!(saved["configApply"]["state"], "pending");
        assert_eq!(saved["modelHotReloaded"], false);
        let config = state.config.read().await.clone();
        let route = config.profiles[0].id.clone();
        let toggled = tokio::time::timeout(
            Duration::from_secs(3),
            models::set_route_enabled(&state, route, false, config.settings_revision),
        )
        .await
        .expect("route toggling must not wait for startup")
        .unwrap();
        assert_eq!(toggled["configApply"]["state"], "pending");
        let runtime_status = tokio::time::timeout(Duration::from_secs(3), runtime_status(&state))
            .await
            .expect("status polling must not wait for startup")
            .unwrap();
        assert_eq!(
            runtime_status["configApply"]["revision"],
            toggled["configApply"]["revision"]
        );
        assert!(
            toggled["configApply"]["revision"].as_u64().unwrap()
                > saved["configApply"]["revision"].as_u64().unwrap()
        );
        assert_eq!(state.store.load().unwrap(), *state.config.read().await);
        drop(lifecycle);
        let result = settled(&state).await;
        assert_eq!(result["state"], "saved");
        assert_eq!(result["revision"], toggled["configApply"]["revision"]);
    }

    #[tokio::test]
    async fn unavailable_worker_reports_failure_and_shutdown_reports_saved() {
        let unavailable = Arc::new(AppState {
            runtime_handle: None,
            ..AppState::default()
        });
        assert_eq!(enqueue(&unavailable, false)["state"], "failed");
        assert!(
            status(&unavailable)["error"]
                .as_str()
                .unwrap()
                .contains("已保存")
        );
        let state = Arc::new(AppState::default());
        state.shutting_down.store(true, Ordering::Release);
        enqueue(&state, true);
        assert_eq!(settled(&state).await["state"], "saved");
    }

    #[tokio::test]
    async fn queued_apply_waits_for_restart_without_holding_the_lifecycle_lock() {
        let state = Arc::new(AppState::default());
        state.restart_in_progress.store(true, Ordering::Release);
        let restart_guard = RestartInProgressGuard {
            state: state.clone(),
        };
        let pending = enqueue(&state, false);
        tokio::task::yield_now().await;
        let lifecycle =
            tokio::time::timeout(Duration::from_secs(1), state.runtime_operation.lock())
                .await
                .expect("scheduled restart must still be able to acquire the lifecycle lock");
        assert_eq!(status(&state), pending);
        drop(lifecycle);
        drop(restart_guard);
        assert_eq!(settled(&state).await["state"], "saved");
    }

    #[tokio::test]
    async fn successful_launch_confirms_only_the_same_config_without_pending_work() {
        let state = Arc::new(AppState::default());
        let config = state.config.read().await.clone();
        for terminal in ["failed", "restart_required", "saved"] {
            {
                let mut queue = state.config_apply.lock().unwrap();
                queue.revision = 7;
                queue.status = json!({"revision":7,"state":terminal,"error":"previous failure"});
            }
            confirm_launch(&state, &config).await;
            assert_eq!(status(&state), json!({"revision":8,"state":"applied"}));
            confirm_launch(&state, &config).await;
            assert_eq!(status(&state), json!({"revision":8,"state":"applied"}));
        }
        let lifecycle = state.runtime_operation.lock().await;
        let pending = enqueue(&state, false);
        confirm_launch(&state, &config).await;
        assert_eq!(status(&state), pending);
        drop(lifecycle);
        settled(&state).await;
        state.config.write().await.user_scripts = vec!["// newer save".into()];
        let previous = status(&state);
        confirm_launch(&state, &config).await;
        assert_eq!(status(&state), previous);
    }
}
