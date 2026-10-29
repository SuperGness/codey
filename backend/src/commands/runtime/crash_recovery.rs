use std::collections::VecDeque;
use std::future::Future;

use tokio::sync::watch;
use tokio::time::Instant;

use super::*;

#[cfg(test)]
mod tests;

const RECOVERY_DELAY: Duration = Duration::from_secs(2);
const SESSION_POLL_INTERVAL: Duration = Duration::from_secs(2);
const SESSION_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const SESSION_QUERY_ERROR_LIMIT: usize = 3;
const RECOVERY_WINDOW: Duration = Duration::from_secs(10 * 60);
const RECOVERY_LIMIT: usize = 2;

#[derive(Default)]
pub(in crate::commands) struct CrashRecoveryBudget {
    attempts: VecDeque<Instant>,
}

impl CrashRecoveryBudget {
    fn reserve(&mut self, now: Instant) -> bool {
        while self
            .attempts
            .front()
            .is_some_and(|attempt| now.duration_since(*attempt) >= RECOVERY_WINDOW)
        {
            self.attempts.pop_front();
        }
        if self.attempts.len() >= RECOVERY_LIMIT {
            return false;
        }
        self.attempts.push_back(now);
        true
    }
}

pub(super) fn is_confirmed_windows_crash(exit: CodexExit) -> bool {
    if exit.platform != "windows" {
        return false;
    }
    // 普通非零返回码和无法取得退出码的情况不能证明崩溃。
    // 控制台关闭、注销和系统关闭也不能触发自动启动。
    let Some(code) = exit.exit_code.and_then(|code| u32::try_from(code).ok()) else {
        return false;
    };
    match code {
        0xC000_013A | 0xC000_026B | 0xC000_02EB | 0xC000_02FE => false,
        // Windows 异常、运行库异常和明确的 fail-fast 终止。
        0x8000_0003
        | 0x8000_0004
        | 0x4000_0015
        | 0xC000_0005
        | 0xC000_0006
        | 0xC000_001D
        | 0xC000_0025
        | 0xC000_0026
        | 0xC000_008C..=0xC000_0096
        | 0xC000_00FD
        | 0xC000_0374
        | 0xC000_0409
        | 0xC000_0417
        | 0xC000_0420
        | 0xC000_0602
        | 0xE06D_7363
        | 0xE043_4352 => true,
        _ => false,
    }
}

#[async_trait::async_trait]
pub(super) trait RecoveryOperations: Send + Sync {
    async fn session_is_interactive(&self) -> Result<bool, String>;
    async fn stop(&self, state: &Arc<AppState>) -> Result<(), String>;
    async fn launch(&self, state: &Arc<AppState>, intent: u64) -> Result<RecoveryLaunch, String>;
}

pub(super) enum RecoveryLaunch {
    Started,
    SessionUnavailable,
}

#[cfg(windows)]
#[derive(Debug)]
pub(super) struct SessionNotInteractive;

#[cfg(windows)]
impl std::fmt::Display for SessionNotInteractive {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Windows 会话已锁定或断开，等待恢复交互后再启动 Codex")
    }
}

#[cfg(windows)]
impl std::error::Error for SessionNotInteractive {}

#[cfg(windows)]
pub(super) struct SystemRecovery;

#[cfg(windows)]
pub(super) async fn query_current_session() -> anyhow::Result<bool> {
    let (send, result) = oneshot::channel();
    // 系统查询即使挂起，也不能阻止 Tokio 运行时退出和释放桌面锁。
    // 查询超时次数受恢复策略限制；独立线程不持有 AppState 或启动子进程。
    std::thread::Builder::new()
        .name("codey-session-query".into())
        .spawn(move || {
            let _ = send.send(crate::windows_session::current_session_is_interactive());
        })
        .context("创建 Windows 会话查询线程失败")?;
    tokio::time::timeout(SESSION_QUERY_TIMEOUT, result)
        .await
        .context("查询 Windows 会话超时")?
        .context("Windows 会话查询线程未返回结果")?
}

#[cfg(windows)]
#[async_trait::async_trait]
impl RecoveryOperations for SystemRecovery {
    async fn session_is_interactive(&self) -> Result<bool, String> {
        query_current_session()
            .await
            .map_err(|error| format!("{error:#}"))
    }

    async fn stop(&self, state: &Arc<AppState>) -> Result<(), String> {
        stop_codey_runtime_locked(state).await.map(|_| ())
    }

    async fn launch(&self, state: &Arc<AppState>, intent: u64) -> Result<RecoveryLaunch, String> {
        // 不取消正在执行的启动 future；启动完成后由 intent 检查负责清理，
        // 避免在创建子进程之后丢弃 future，留下无法回收的进程。
        match launch_codey_inner_locked_for_intent(state, Some(intent)).await {
            Ok(_) => Ok(RecoveryLaunch::Started),
            Err(error) if error.is::<SessionNotInteractive>() => {
                Ok(RecoveryLaunch::SessionUnavailable)
            }
            Err(error) => Err(format!("{error:#}")),
        }
    }
}

fn recovery_is_current(state: &AppState, generation: u64, intent: u64) -> bool {
    !state.is_shutting_down()
        && !state.restart_in_progress.load(Ordering::Acquire)
        && state.runtime_generation.load(Ordering::Acquire) == generation
        && *state.runtime_intent.borrow() == intent
}

async fn unless_cancelled<T>(
    future: impl Future<Output = T>,
    intent_changes: &mut watch::Receiver<u64>,
    intent: u64,
) -> Option<T> {
    if *intent_changes.borrow_and_update() != intent {
        return None;
    }
    tokio::select! {
        biased;
        _ = intent_changes.changed() => None,
        result = future => Some(result),
    }
}

fn record_recovery(event: &str, generation: u64, exit: CodexExit, detail: Value) {
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
        event,
        json!({
            "codeyProcessId": std::process::id(),
            "runtimeGeneration": generation,
            "exit": exit,
            "detail": detail,
        }),
    );
}

struct RecoveryContext<'a> {
    state: &'a Arc<AppState>,
    generation: u64,
    intent: u64,
    exit: CodexExit,
    changes: watch::Receiver<u64>,
    query_errors: usize,
    waiting_logged: bool,
}

impl RecoveryContext<'_> {
    fn active(&self) -> bool {
        recovery_is_current(self.state, self.generation, self.intent)
    }

    async fn pause(&mut self, duration: Duration) -> bool {
        unless_cancelled(tokio::time::sleep(duration), &mut self.changes, self.intent)
            .await
            .is_some()
            && self.active()
    }

    async fn fail(&self, error: String) {
        let mut startup_error = self.state.startup_error.write().await;
        if !self.active() {
            return;
        }
        record_recovery(
            "launcher.codex_crash_recovery_failed",
            self.generation,
            self.exit,
            json!({"error": error, "shutdownRequested": true}),
        );
        error_log::record_failure(
            "runtime_restart_failed",
            "recover_windows_codex_crash",
            error.clone(),
            json!({"runtimeGeneration": self.generation, "exit": self.exit}),
        );
        *startup_error = Some(error);
        // 主进程沿用完整退出清理，释放桌面单实例锁。
        self.state
            .request_shutdown_with_reason(super::super::AppShutdownReason::RuntimeRestartFailed);
    }

    async fn query(&mut self, operations: &dyn RecoveryOperations) -> Option<bool> {
        if !self.active() {
            return None;
        }
        let interactive = unless_cancelled(
            tokio::time::timeout(SESSION_QUERY_TIMEOUT, operations.session_is_interactive()),
            &mut self.changes,
            self.intent,
        )
        .await?
        .unwrap_or_else(|_| Err("查询 Windows 会话超时".into()));
        match interactive {
            Ok(true) => return Some(true),
            Ok(false) => {}
            Err(error) => {
                self.query_errors += 1;
                record_recovery(
                    "launcher.codex_crash_recovery_session_query_failed",
                    self.generation,
                    self.exit,
                    json!({"error": error, "attempt": self.query_errors}),
                );
                if self.query_errors >= SESSION_QUERY_ERROR_LIMIT {
                    self.fail("无法确认 Windows 会话是否可交互，已停止 Codex 自动恢复".into())
                        .await;
                    return None;
                }
            }
        }
        if !self.waiting_logged {
            record_recovery(
                "launcher.codex_crash_recovery_waiting_for_session",
                self.generation,
                self.exit,
                json!({}),
            );
            self.waiting_logged = true;
        }
        Some(false)
    }

    async fn wait_for_session(&mut self, operations: &dyn RecoveryOperations) -> bool {
        loop {
            match self.query(operations).await {
                Some(true) => return true,
                Some(false) if self.pause(SESSION_POLL_INTERVAL).await => {}
                _ => return false,
            }
        }
    }
}

pub(super) async fn recover_codex_exit(
    state: &Arc<AppState>,
    generation: u64,
    intent: u64,
    exit: CodexExit,
    operations: &dyn RecoveryOperations,
) {
    if !is_confirmed_windows_crash(exit) || !recovery_is_current(state, generation, intent) {
        return;
    }
    let mut context = RecoveryContext {
        state,
        generation,
        intent,
        exit,
        changes: state.runtime_intent.subscribe(),
        query_errors: 0,
        waiting_logged: false,
    };
    record_recovery(
        "launcher.codex_crash_recovery_pending",
        generation,
        exit,
        json!({"delayMs": RECOVERY_DELAY.as_millis()}),
    );
    if !context.pause(RECOVERY_DELAY).await {
        return;
    }
    loop {
        if !context.wait_for_session(operations).await {
            return;
        }
        let Some(operation) =
            unless_cancelled(state.runtime_operation.lock(), &mut context.changes, intent).await
        else {
            return;
        };
        // 等待生命周期互斥锁时桌面可能再次锁定；清理和启动前均再次检查。
        match context.query(operations).await {
            Some(true) => {}
            Some(false) => {
                drop(operation);
                if !context.pause(SESSION_POLL_INTERVAL).await {
                    return;
                }
                continue;
            }
            None => return,
        }
        if let Err(error) = operations.stop(state).await {
            context
                .fail(format!("恢复前清理 Codex 失败：{error}"))
                .await;
            return;
        }
        match context.query(operations).await {
            Some(true) => {}
            Some(false) => {
                drop(operation);
                if !context.pause(SESSION_POLL_INTERVAL).await {
                    return;
                }
                continue;
            }
            None => return,
        }
        if !context.active() {
            return;
        }
        if !state
            .codex_crash_recovery
            .lock()
            .await
            .reserve(Instant::now())
        {
            context
                .fail("Codex 在 10 分钟内已自动恢复 2 次，已停止反复启动".into())
                .await;
            return;
        }
        // 不重放任何请求，只复用现有配置恢复及启动流程。
        match operations.launch(state, intent).await {
            Ok(RecoveryLaunch::Started) => {
                record_recovery(
                    "launcher.codex_crash_recovered",
                    generation,
                    exit,
                    json!({}),
                );
                return;
            }
            Ok(RecoveryLaunch::SessionUnavailable) => {
                // 配置准备期间再次锁屏，尚未创建进程，不消耗启动次数。
                state.codex_crash_recovery.lock().await.attempts.pop_back();
                drop(operation);
                if !context.pause(SESSION_POLL_INTERVAL).await {
                    return;
                }
            }
            Err(error) => {
                // 已创建进程的失败启动会保留清理对象；此处和主退出路径继续重试。
                let cleanup = operations.stop(state).await;
                let error = match cleanup {
                    Ok(()) => format!("Codex 自动恢复失败：{error}"),
                    Err(cleanup) => format!("Codex 自动恢复失败：{error}；清理失败：{cleanup}"),
                };
                context.fail(error).await;
                return;
            }
        }
    }
}
