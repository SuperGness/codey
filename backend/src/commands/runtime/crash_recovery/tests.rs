use std::sync::atomic::{AtomicBool, AtomicUsize};

use tokio::sync::{Mutex, Notify, oneshot};

use super::*;

const CRASH: CodexExit = CodexExit {
    platform: "windows",
    exit_code: Some(0xC000_0005),
};

struct FakeRecovery {
    interactive: AtomicBool,
    query_error: AtomicBool,
    hang_query: AtomicBool,
    queries: AtomicUsize,
    queried: Notify,
    stops: AtomicUsize,
    launches: AtomicUsize,
    fail_launch: AtomicBool,
    fail_stop: AtomicBool,
    cancel_after_stop: AtomicBool,
    relock_during_preparation: AtomicBool,
    launch_entered: Notify,
    launch_release: Mutex<Option<oneshot::Receiver<()>>>,
    #[cfg(unix)]
    spawn_before_failure: AtomicBool,
    #[cfg(unix)]
    child: Mutex<Option<tokio::process::Child>>,
    #[cfg(unix)]
    reaped: AtomicBool,
}

impl Default for FakeRecovery {
    fn default() -> Self {
        Self {
            interactive: AtomicBool::new(true),
            query_error: AtomicBool::new(false),
            hang_query: AtomicBool::new(false),
            queries: AtomicUsize::new(0),
            queried: Notify::new(),
            stops: AtomicUsize::new(0),
            launches: AtomicUsize::new(0),
            fail_launch: AtomicBool::new(false),
            fail_stop: AtomicBool::new(false),
            cancel_after_stop: AtomicBool::new(false),
            relock_during_preparation: AtomicBool::new(false),
            launch_entered: Notify::new(),
            launch_release: Mutex::new(None),
            #[cfg(unix)]
            spawn_before_failure: AtomicBool::new(false),
            #[cfg(unix)]
            child: Mutex::new(None),
            #[cfg(unix)]
            reaped: AtomicBool::new(false),
        }
    }
}

#[async_trait::async_trait]
impl RecoveryOperations for FakeRecovery {
    async fn session_is_interactive(&self) -> Result<bool, String> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        self.queried.notify_one();
        if self.hang_query.load(Ordering::SeqCst) {
            return std::future::pending().await;
        }
        if self.query_error.load(Ordering::SeqCst) {
            Err("WTS query failed".into())
        } else {
            Ok(self.interactive.load(Ordering::SeqCst))
        }
    }

    async fn stop(&self, state: &Arc<AppState>) -> Result<(), String> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        if self.fail_stop.load(Ordering::SeqCst) {
            return Err("process cleanup failed".into());
        }
        #[cfg(unix)]
        if let Some(mut child) = self.child.lock().await.take() {
            child.kill().await.map_err(|error| error.to_string())?;
            self.reaped.store(true, Ordering::SeqCst);
        }
        if self.cancel_after_stop.load(Ordering::SeqCst) {
            state.cancel_pending_runtime_recovery();
        }
        Ok(())
    }

    async fn launch(&self, state: &Arc<AppState>, intent: u64) -> Result<RecoveryLaunch, String> {
        if self.relock_during_preparation.swap(false, Ordering::SeqCst) {
            self.interactive.store(false, Ordering::SeqCst);
            return Ok(RecoveryLaunch::SessionUnavailable);
        }
        self.launches.fetch_add(1, Ordering::SeqCst);
        #[cfg(unix)]
        if self.spawn_before_failure.load(Ordering::SeqCst) {
            *self.child.lock().await = Some(
                tokio::process::Command::new("sh")
                    .args(["-c", "exec sleep 60"])
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|error| error.to_string())?,
            );
        }
        self.launch_entered.notify_one();
        if let Some(release) = self.launch_release.lock().await.take() {
            let _ = release.await;
        }
        if self.fail_launch.load(Ordering::SeqCst) || *state.runtime_intent.borrow() != intent {
            return Err("injected launch failure or cancellation".into());
        }
        state.runtime_generation.fetch_add(1, Ordering::SeqCst);
        Ok(RecoveryLaunch::Started)
    }
}

impl FakeRecovery {
    async fn wait_for_query(&self) {
        self.queried.notified().await;
    }
}

fn observe(
    state: &Arc<AppState>,
    operations: &Arc<FakeRecovery>,
    generation: u64,
    exit: CodexExit,
) -> tokio::task::JoinHandle<()> {
    let state = state.clone();
    let operations = operations.clone();
    let (send, receive) = oneshot::channel();
    send.send(exit).unwrap();
    tokio::spawn(async move {
        super::super::forward_codex_exit_with_operations(
            state,
            receive,
            generation,
            Some(operations.as_ref()),
        )
        .await;
    })
}

#[test]
fn only_explicit_windows_exception_codes_are_recoverable() {
    for code in [
        0xC000_0005,
        0xC000_0409,
        0xC000_0374,
        0x8000_0003,
        0xE06D_7363,
    ] {
        assert!(is_confirmed_windows_crash(CodexExit {
            exit_code: Some(code),
            ..CRASH
        }));
    }
    for code in [
        None,
        Some(0),
        Some(1),
        Some(23),
        Some(-1),
        Some(0xC000_0001),
        Some(0xC000_013A),
        Some(0xC000_026B),
        Some(0xC000_02EB),
        Some(0xC000_02FE),
    ] {
        assert!(!is_confirmed_windows_crash(CodexExit {
            exit_code: code,
            ..CRASH
        }));
    }
    for platform in ["macos", "linux"] {
        assert!(!is_confirmed_windows_crash(CodexExit { platform, ..CRASH }));
    }
}

#[tokio::test(start_paused = true)]
async fn confirmed_crash_restarts_once_after_delay() {
    let state = Arc::new(AppState::default());
    let operations = Arc::new(FakeRecovery::default());
    let task = observe(&state, &operations, 0, CRASH);
    tokio::task::yield_now().await;
    assert_eq!(operations.launches.load(Ordering::SeqCst), 0);
    task.await.unwrap();
    assert_eq!(operations.stops.load(Ordering::SeqCst), 1);
    assert_eq!(operations.launches.load(Ordering::SeqCst), 1);
    assert_eq!(state.runtime_generation.load(Ordering::SeqCst), 1);
    assert!(!state.is_shutting_down());
}

#[tokio::test(start_paused = true)]
async fn ordinary_unknown_and_non_windows_exits_keep_shutdown_behavior() {
    for exit in [
        CodexExit {
            exit_code: Some(0),
            ..CRASH
        },
        CodexExit {
            exit_code: None,
            ..CRASH
        },
        CodexExit {
            exit_code: Some(1),
            ..CRASH
        },
        CodexExit {
            exit_code: Some(0xC000_013A),
            ..CRASH
        },
        CodexExit {
            exit_code: Some(0xC000_026B),
            ..CRASH
        },
        CodexExit {
            platform: "macos",
            ..CRASH
        },
        CodexExit {
            platform: "linux",
            ..CRASH
        },
    ] {
        let state = Arc::new(AppState::default());
        let operations = Arc::new(FakeRecovery::default());
        observe(&state, &operations, 0, exit).await.unwrap();
        assert_eq!(
            state.wait_for_shutdown().await,
            super::super::super::AppShutdownReason::CodexExited
        );
        assert_eq!(operations.queries.load(Ordering::SeqCst), 0);
        assert_eq!(operations.launches.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test(start_paused = true)]
async fn stale_crash_does_not_touch_new_generation() {
    let state = Arc::new(AppState::default());
    state.runtime_generation.store(1, Ordering::SeqCst);
    let operations = Arc::new(FakeRecovery::default());
    observe(&state, &operations, 0, CRASH).await.unwrap();
    assert_eq!(operations.queries.load(Ordering::SeqCst), 0);
    assert_eq!(operations.launches.load(Ordering::SeqCst), 0);
    assert!(!state.is_shutting_down());
}

#[tokio::test(start_paused = true)]
async fn exit_during_explicit_restart_never_recovers_the_old_generation() {
    let state = Arc::new(AppState::default());
    let operations = Arc::new(FakeRecovery::default());
    state.restart_in_progress.store(true, Ordering::SeqCst);
    let task = observe(&state, &operations, 0, CRASH);
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    state.runtime_generation.store(1, Ordering::SeqCst);
    state.restart_in_progress.store(false, Ordering::SeqCst);
    state.restart_settled.notify_waiters();
    task.await.unwrap();
    assert_eq!(operations.queries.load(Ordering::SeqCst), 0);
    assert_eq!(operations.launches.load(Ordering::SeqCst), 0);
    assert!(!state.is_shutting_down());
}

#[cfg(not(windows))]
#[tokio::test(start_paused = true)]
async fn non_windows_runtime_observer_never_installs_recovery() {
    let state = Arc::new(AppState::default());
    let (send, receive) = oneshot::channel();
    send.send(CRASH).unwrap();
    super::super::forward_codex_exit_to_codey_shutdown(state.clone(), receive, 0).await;
    assert_eq!(
        state.wait_for_shutdown().await,
        super::super::super::AppShutdownReason::CodexExited
    );
    assert!(state.codex_crash_recovery.lock().await.attempts.is_empty());
}

#[tokio::test(start_paused = true)]
async fn locked_session_waits_without_spending_attempts_or_holding_operation_lock() {
    let state = Arc::new(AppState::default());
    let operations = Arc::new(FakeRecovery::default());
    operations.interactive.store(false, Ordering::SeqCst);
    let task = observe(&state, &operations, 0, CRASH);
    operations.wait_for_query().await;
    tokio::time::advance(RECOVERY_WINDOW * 3).await;
    assert_eq!(operations.launches.load(Ordering::SeqCst), 0);
    assert!(state.codex_crash_recovery.lock().await.attempts.is_empty());
    assert!(state.runtime_operation.try_lock().is_ok());
    operations.interactive.store(true, Ordering::SeqCst);
    task.await.unwrap();
    assert_eq!(operations.launches.load(Ordering::SeqCst), 1);
    assert!(!state.is_shutting_down());
}

#[tokio::test(start_paused = true)]
async fn relock_during_launch_preparation_preserves_budget() {
    let state = Arc::new(AppState::default());
    let operations = Arc::new(FakeRecovery::default());
    operations
        .relock_during_preparation
        .store(true, Ordering::SeqCst);
    let task = observe(&state, &operations, 0, CRASH);
    loop {
        operations.wait_for_query().await;
        if !operations.interactive.load(Ordering::SeqCst) {
            break;
        }
    }
    assert_eq!(operations.launches.load(Ordering::SeqCst), 0);
    assert!(state.codex_crash_recovery.lock().await.attempts.is_empty());
    operations.interactive.store(true, Ordering::SeqCst);
    task.await.unwrap();
    assert_eq!(operations.launches.load(Ordering::SeqCst), 1);
    assert_eq!(state.codex_crash_recovery.lock().await.attempts.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn shutdown_and_update_cancel_locked_wait() {
    for update in [false, true] {
        let state = Arc::new(AppState::default());
        let operations = Arc::new(FakeRecovery::default());
        operations.interactive.store(false, Ordering::SeqCst);
        let task = observe(&state, &operations, 0, CRASH);
        operations.wait_for_query().await;
        if update {
            state.request_update_shutdown();
        } else {
            super::super::begin_shutdown(&state).await;
        }
        task.await.unwrap();
        assert_eq!(operations.stops.load(Ordering::SeqCst), 0);
        assert_eq!(operations.launches.load(Ordering::SeqCst), 0);
        if update {
            assert_eq!(
                state.wait_for_shutdown().await,
                super::super::super::AppShutdownReason::InstallUpdate
            );
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
#[tokio::test(start_paused = true)]
async fn explicit_restart_cancels_recovery_waiting_for_operation_lock() {
    let state = Arc::new(AppState::default());
    let operations = Arc::new(FakeRecovery::default());
    // 先让退出观察任务通过运行代检查，再占用生命周期锁。
    operations.interactive.store(false, Ordering::SeqCst);
    let task = observe(&state, &operations, 0, CRASH);
    operations.wait_for_query().await;
    let operation = state.runtime_operation.lock().await;
    operations.interactive.store(true, Ordering::SeqCst);
    tokio::time::advance(SESSION_POLL_INTERVAL).await;
    super::super::schedule_restart_codey_runtime(&state)
        .await
        .unwrap();
    task.await.unwrap();
    assert_eq!(operations.stops.load(Ordering::SeqCst), 0);
    assert_eq!(operations.launches.load(Ordering::SeqCst), 0);
    assert!(!state.is_shutting_down());
    super::super::begin_shutdown(&state).await;
    drop(operation);
}

#[tokio::test(start_paused = true)]
async fn cancellation_after_cleanup_never_launches() {
    let state = Arc::new(AppState::default());
    let operations = Arc::new(FakeRecovery::default());
    operations.cancel_after_stop.store(true, Ordering::SeqCst);
    observe(&state, &operations, 0, CRASH).await.unwrap();
    assert_eq!(operations.stops.load(Ordering::SeqCst), 1);
    assert_eq!(operations.launches.load(Ordering::SeqCst), 0);
    assert!(!state.is_shutting_down());
}

#[tokio::test(start_paused = true)]
async fn update_during_launch_waits_for_cleanup_and_preserves_update_reason() {
    let state = Arc::new(AppState::default());
    let operations = Arc::new(FakeRecovery::default());
    let (release, waiting) = oneshot::channel();
    *operations.launch_release.lock().await = Some(waiting);
    let task = observe(&state, &operations, 0, CRASH);
    operations.launch_entered.notified().await;
    state.request_update_shutdown();
    assert!(!task.is_finished());
    release.send(()).unwrap();
    task.await.unwrap();
    assert_eq!(operations.stops.load(Ordering::SeqCst), 2);
    assert_eq!(
        state.wait_for_shutdown().await,
        super::super::super::AppShutdownReason::InstallUpdate
    );
}

#[tokio::test(start_paused = true)]
async fn repeated_query_errors_end_recovery_without_attempting_launch() {
    let state = Arc::new(AppState::default());
    let operations = Arc::new(FakeRecovery::default());
    operations.query_error.store(true, Ordering::SeqCst);
    observe(&state, &operations, 0, CRASH).await.unwrap();
    assert_eq!(
        operations.queries.load(Ordering::SeqCst),
        SESSION_QUERY_ERROR_LIMIT
    );
    assert_eq!(operations.launches.load(Ordering::SeqCst), 0);
    assert_eq!(
        state.wait_for_shutdown().await,
        super::super::super::AppShutdownReason::RuntimeRestartFailed
    );
}

#[tokio::test(start_paused = true)]
async fn unresponsive_session_queries_have_a_finite_deadline() {
    let state = Arc::new(AppState::default());
    let operations = Arc::new(FakeRecovery::default());
    operations.hang_query.store(true, Ordering::SeqCst);
    observe(&state, &operations, 0, CRASH).await.unwrap();
    assert_eq!(
        operations.queries.load(Ordering::SeqCst),
        SESSION_QUERY_ERROR_LIMIT
    );
    assert_eq!(operations.launches.load(Ordering::SeqCst), 0);
    assert!(state.is_shutting_down());
}

#[tokio::test(start_paused = true)]
async fn recovery_budget_stops_crash_loop_and_expires_after_stability() {
    for stable in [false, true] {
        let state = Arc::new(AppState::default());
        let operations = Arc::new(FakeRecovery::default());
        for generation in 0..2 {
            observe(&state, &operations, generation, CRASH)
                .await
                .unwrap();
        }
        assert_eq!(operations.launches.load(Ordering::SeqCst), 2);
        if stable {
            tokio::time::advance(RECOVERY_WINDOW).await;
        }
        observe(&state, &operations, 2, CRASH).await.unwrap();
        assert_eq!(
            operations.launches.load(Ordering::SeqCst),
            if stable { 3 } else { 2 }
        );
        assert_eq!(state.is_shutting_down(), !stable);
        if !stable {
            assert_eq!(
                state.wait_for_shutdown().await,
                super::super::super::AppShutdownReason::RuntimeRestartFailed
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn stop_failure_ends_recovery_without_starting_another_process() {
    let state = Arc::new(AppState::default());
    let operations = Arc::new(FakeRecovery::default());
    operations.fail_stop.store(true, Ordering::SeqCst);
    observe(&state, &operations, 0, CRASH).await.unwrap();
    assert_eq!(operations.stops.load(Ordering::SeqCst), 1);
    assert_eq!(operations.launches.load(Ordering::SeqCst), 0);
    assert!(state.is_shutting_down());
}

#[cfg(unix)]
#[tokio::test]
async fn failed_launch_reaps_a_partially_started_child_before_shutdown() {
    let state = Arc::new(AppState::default());
    let operations = Arc::new(FakeRecovery::default());
    operations
        .spawn_before_failure
        .store(true, Ordering::SeqCst);
    operations.fail_launch.store(true, Ordering::SeqCst);
    observe(&state, &operations, 0, CRASH).await.unwrap();
    assert_eq!(operations.stops.load(Ordering::SeqCst), 2);
    assert!(operations.reaped.load(Ordering::SeqCst));
    assert!(operations.child.lock().await.is_none());
    assert_eq!(
        state.wait_for_shutdown().await,
        super::super::super::AppShutdownReason::RuntimeRestartFailed
    );
}
