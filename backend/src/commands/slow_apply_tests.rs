use super::*;

const TEST_WATCHDOG: Duration = Duration::from_secs(15);

type Reconcile = dyn Fn(&CodeyConfig) -> anyhow::Result<crate::codex_config::RuntimeSubagentReconcileReport>
    + Send
    + Sync;

#[derive(Default)]
pub(super) struct TestHooks {
    pub request_log: Option<Arc<Gate>>,
    pub subagents: Option<Arc<Reconcile>>,
    pub trace_home: Option<PathBuf>,
}

#[derive(Default)]
pub(super) struct Gate {
    entered: Notify,
    release: Notify,
}

impl Gate {
    pub async fn wait(&self) {
        self.entered.notify_one();
        self.release.notified().await;
    }
}

async fn settled(state: &AppState) -> Value {
    tokio::time::timeout(TEST_WATCHDOG, async {
        loop {
            let result = config_apply::status(state);
            if result["state"] != "pending" {
                return result;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

async fn save_without_waiting(state: &Arc<AppState>, config: CodeyConfig) -> Value {
    let result = tokio::time::timeout(TEST_WATCHDOG, save_codey_config(state, config))
        .await
        .expect("slow application must not block a subsequent save")
        .unwrap();
    assert_eq!(state.store.load().unwrap(), *state.config.read().await);
    result
}

#[tokio::test]
async fn request_log_wait_releases_config_lock_and_rejects_old_result() {
    let directory = tempfile::tempdir().unwrap();
    let config = CodeyConfig {
        local_router_enabled: true,
        ..Default::default()
    };
    let gate = Arc::new(Gate::default());
    let state = Arc::new(AppState {
        store: ConfigStore::new(directory.path().join("config.json")),
        config: RwLock::new(config.clone()),
        runtime: Mutex::new(Some(Arc::new(CodeyRuntime::for_model_delivery_test(
            config.clone(),
            "ws://unused",
            None,
        )))),
        slow_apply_test: TestHooks {
            request_log: Some(gate.clone()),
            ..Default::default()
        },
        ..Default::default()
    });
    let lifecycle = state.runtime_operation.lock().await;
    let delivery = tokio::spawn({
        let state = state.clone();
        async move { hot_reload_runtime_request_log_locked(&state, &config).await }
    });
    gate.entered.notified().await;
    let mut newer = state.config.read().await.clone();
    newer.route_request_log.enabled = !newer.route_request_log.enabled;
    let saved = save_without_waiting(&state, newer).await;
    assert_eq!(saved["configApply"]["state"], "pending");
    gate.release.notify_one();
    assert_eq!(delivery.await.unwrap().health(), "superseded");
    // No renderer is needed for this test; release the real queued delivery.
    *state.runtime.lock().await = None;
    drop(lifecycle);
    assert_eq!(
        settled(&state).await["revision"],
        saved["configApply"]["revision"]
    );
}

#[tokio::test]
async fn role_worker_wait_releases_config_lock_and_does_not_mark_old_roles_applied() {
    let directory = tempfile::tempdir().unwrap();
    let config = CodeyConfig {
        subagent_optimization: true,
        local_router_enabled: false,
        ..Default::default()
    };
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered_tx = BlockingMutex::new(Some(entered_tx));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = BlockingMutex::new(release_rx);
    let runtime = Arc::new(CodeyRuntime::for_model_delivery_test(
        config.clone(),
        "ws://unused",
        None,
    ));
    let state = Arc::new(AppState {
        store: ConfigStore::new(directory.path().join("config.json")),
        config: RwLock::new(config.clone()),
        runtime: Mutex::new(Some(runtime.clone())),
        slow_apply_test: TestHooks {
            subagents: Some(Arc::new(move |_| {
                if let Some(entered) = entered_tx.lock().unwrap().take() {
                    entered.send(()).unwrap();
                    release_rx.lock().unwrap().recv().unwrap();
                }
                Ok(Default::default())
            })),
            ..Default::default()
        },
        ..Default::default()
    });
    let delivery = tokio::spawn({
        let state = state.clone();
        async move { hot_reload_runtime_subagent_config(&state, &config).await }
    });
    entered_rx.await.unwrap();
    let mut newer = state.config.read().await.clone();
    let role = newer.subagent_roles.get_mut("codey_quick_scan").unwrap();
    role.reasoning_effort = if role.reasoning_effort == "high" {
        "low"
    } else {
        "high"
    }
    .into();
    save_without_waiting(&state, newer).await;
    release_tx.send(()).unwrap();
    assert_eq!(delivery.await.unwrap().health(), "superseded");
    let result = settled(&state).await;
    assert_ne!(result["state"], "failed", "{result}");
    assert_eq!(
        runtime.applied_subagent_config().await.as_ref(),
        &RuntimeSubagentConfig::from_config(&*state.config.read().await)
    );
}

#[tokio::test]
async fn trace_wait_does_not_block_saves_and_applies_latest_without_runtime() {
    let directory = tempfile::tempdir().unwrap();
    let config = CodeyConfig {
        disable_trace_log_writes: false,
        ..Default::default()
    };
    let state = Arc::new(AppState {
        store: ConfigStore::new(directory.path().join("config.json")),
        config: RwLock::new(config.clone()),
        slow_apply_test: TestHooks {
            trace_home: Some(directory.path().into()),
            ..Default::default()
        },
        ..Default::default()
    });
    let diagnostic = state.diagnostic_storage_operation.lock().await;
    let mut enabled = config;
    enabled.disable_trace_log_writes = true;
    save_without_waiting(&state, enabled).await;
    // Wait until the queue owns the lifecycle lock and is blocked on diagnostics.
    loop {
        if state.runtime_operation.try_lock().is_err() {
            break;
        }
        tokio::task::yield_now().await;
    }
    let mut latest = state.config.read().await.clone();
    latest.disable_trace_log_writes = false;
    let saved = save_without_waiting(&state, latest).await;
    drop(diagnostic);
    let result = settled(&state).await;
    assert_eq!(result["revision"], saved["configApply"]["revision"]);
    assert_eq!(result["state"], "saved");
    assert_eq!(state.trace_log_guard_state.applied_target(), Some(false));
    assert!(
        !state
            .trace_log_write_protection_active
            .load(Ordering::Acquire)
    );
}

#[tokio::test]
async fn trace_failure_keeps_saved_target_and_retries_without_rescanning_success() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("trace-home");
    std::fs::write(&home, "not a directory").unwrap();
    let state = Arc::new(AppState {
        store: ConfigStore::new(directory.path().join("config.json")),
        config: RwLock::new(CodeyConfig {
            disable_trace_log_writes: false,
            ..Default::default()
        }),
        slow_apply_test: TestHooks {
            trace_home: Some(home.clone()),
            ..Default::default()
        },
        ..Default::default()
    });
    let mut enabled = state.config.read().await.clone();
    enabled.disable_trace_log_writes = true;
    save_without_waiting(&state, enabled).await;
    assert_eq!(settled(&state).await["state"], "failed");
    assert!(state.store.load().unwrap().disable_trace_log_writes);
    assert!(
        !state
            .trace_log_write_protection_active
            .load(Ordering::Acquire)
    );
    std::fs::remove_file(&home).unwrap();
    std::fs::create_dir(&home).unwrap();
    let current = state.config.read().await.clone();
    save_without_waiting(&state, current).await;
    assert_eq!(settled(&state).await["state"], "saved");
    assert_eq!(state.trace_log_guard_state.applied_target(), Some(true));
    assert!(
        !state
            .trace_log_write_protection_active
            .load(Ordering::Acquire)
    );
    // Removing the directory after success must not make an unrelated save scan it.
    std::fs::remove_dir(&home).unwrap();
    std::fs::write(&home, "not a directory").unwrap();
    let current = state.config.read().await.clone();
    save_without_waiting(&state, current).await;
    assert_eq!(settled(&state).await["state"], "saved");
}

#[tokio::test]
async fn sqlite_lock_wait_does_not_block_another_config_save() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = rusqlite::Connection::open(directory.path().join("logs_2.sqlite")).unwrap();
    db.execute_batch("CREATE TABLE logs(level TEXT);").unwrap();
    // 断言失败时也会自动回滚，避免阻塞任务等到较长的锁等待超时。
    let transaction = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Exclusive)
        .unwrap();
    let state = Arc::new(AppState {
        store: ConfigStore::new(directory.path().join("config.json")),
        config: RwLock::new(CodeyConfig {
            disable_trace_log_writes: false,
            ..Default::default()
        }),
        slow_apply_test: TestHooks {
            trace_home: Some(directory.path().into()),
            ..Default::default()
        },
        // SQLite 的等待时间要明显长于保存超时，才能确认保存结束时应用仍被锁阻塞。
        trace_log_guard_state: Arc::new(
            crate::trace_log_guard::TraceLogGuardState::with_busy_timeout_for_test(
                Duration::from_secs(60),
            ),
        ),
        ..Default::default()
    });
    let mut enabled = state.config.read().await.clone();
    enabled.disable_trace_log_writes = true;
    save_without_waiting(&state, enabled).await;
    tokio::time::timeout(TEST_WATCHDOG, async {
        while !state.trace_log_guard_state.configuring_for_test() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut latest = state.config.read().await.clone();
    latest.user_scripts.push("// unrelated change".into());
    let saved = save_without_waiting(&state, latest).await;
    assert_eq!(saved["configApply"]["state"], "pending");
    assert!(state.trace_log_guard_state.configuring_for_test());
    assert_eq!(state.trace_log_guard_state.applied_target(), None);
    transaction.rollback().unwrap();
    assert_eq!(
        settled(&state).await["revision"],
        saved["configApply"]["revision"]
    );
    assert!(
        state
            .trace_log_write_protection_active
            .load(Ordering::Acquire)
    );
    assert_eq!(
        db.execute("INSERT INTO logs(level) VALUES ('TRACE')", [])
            .unwrap(),
        0
    );
}
