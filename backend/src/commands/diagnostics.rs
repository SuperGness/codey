use std::sync::Arc;

use serde_json::{Value, json};

use super::AppState;
use crate::codex_config::codex_home;
use crate::crashpad_pending_guard::{self, CrashpadPendingStatsSnapshot};
use crate::error_log;
use crate::trace_log_guard;
use crate::trace_log_stats::{self, TraceLogStatsSnapshot};

pub(super) async fn clear_diagnostic_storage(
    state: &Arc<AppState>,
    args: &Value,
) -> Result<Value, String> {
    let (clear_trace, clear_crashpad) = match args.get("target") {
        Some(Value::String(target)) if target == "trace" => (true, false),
        Some(Value::String(target)) if target == "crashpad" => (false, true),
        _ => return Err("无效的诊断清理目标".to_string()),
    };
    let _operation = state.diagnostic_storage_operation.lock().await;
    let config = state.config.read().await;
    let disable_trace_writes = config.disable_trace_log_writes;
    let protect_crashpad_pending = config.protect_crashpad_pending;
    drop(config);

    let trace_home = codex_home();
    let previous_protection_active = state
        .trace_log_write_protection_active
        .load(std::sync::atomic::Ordering::Acquire);
    let trace_task = tokio::task::spawn_blocking(move || {
        if !clear_trace {
            return (
                Ok(Default::default()),
                TraceLogStatsSnapshot::idle(),
                previous_protection_active,
                TraceLogStatsSnapshot::idle(),
            );
        }
        let before = trace_log_stats::snapshot(trace_home);
        let guard = trace_log_guard::configure(trace_home, disable_trace_writes);
        let trace_log_write_protection_active = guard
            .as_ref()
            .is_ok_and(|report| report.protection_active(disable_trace_writes));
        let cleanup = guard.and_then(|_| trace_log_guard::clear(trace_home));
        let snapshot = trace_log_stats::snapshot(trace_home);
        (cleanup, snapshot, trace_log_write_protection_active, before)
    });
    let crashpad_task = tokio::task::spawn_blocking(move || {
        if clear_crashpad {
            crashpad_pending_guard::clear_system(protect_crashpad_pending)
        } else {
            crashpad_pending_guard::CrashpadGuardRun {
                cleanup: Default::default(),
                snapshot: CrashpadPendingStatsSnapshot::idle(protect_crashpad_pending),
            }
        }
    });
    let (trace_result, crashpad_result) = tokio::join!(trace_task, crashpad_task);

    let mut errors = Vec::new();
    let trace_before = trace_result
        .as_ref()
        .ok()
        .map(|(_, _, _, before)| before.clone());
    let (trace_cleanup, trace_snapshot, trace_log_write_protection_active) = match trace_result {
        Ok((Ok(cleanup), snapshot, protection_active, _)) => {
            (Some(cleanup), snapshot, protection_active)
        }
        Ok((Err(error), snapshot, protection_active, _)) => {
            let error = format!("{error:#}");
            error_log::record_failure(
                "cleanup_failed",
                "clear_diagnostic_trace_logs",
                error.clone(),
                json!({
                    "protectionEnabled": disable_trace_writes,
                }),
            );
            errors.push(error);
            (None, snapshot, protection_active)
        }
        Err(error) => {
            let error = format!("Trace 日志库清理任务异常退出：{error}");
            error_log::record_failure(
                "cleanup_failed",
                "clear_diagnostic_trace_logs",
                error.clone(),
                json!({
                    "protectionEnabled": disable_trace_writes,
                    "taskJoinFailed": true,
                }),
            );
            errors.push(error.clone());
            let mut snapshot = TraceLogStatsSnapshot::idle();
            snapshot.errors.push(error);
            (None, snapshot, false)
        }
    };
    if clear_trace {
        errors.extend(trace_snapshot.errors.iter().cloned());
    }
    state.trace_log_write_protection_active.store(
        trace_log_write_protection_active,
        std::sync::atomic::Ordering::Release,
    );

    let (crashpad_cleanup, crashpad_snapshot) = match crashpad_result {
        Ok(run) => {
            if !run.cleanup.errors.is_empty() {
                let error = format!(
                    "{} 个 Crashpad 待处理文件未能完成清理",
                    run.cleanup.errors.len()
                );
                error_log::record_failure(
                    "cleanup_failed",
                    "clear_crashpad_pending",
                    error,
                    json!({
                        "protectionEnabled": protect_crashpad_pending,
                        "errorCount": run.cleanup.errors.len(),
                    }),
                );
            }
            errors.extend(run.cleanup.errors.iter().cloned());
            (run.cleanup, run.snapshot)
        }
        Err(error) => {
            let error = format!("Crashpad 待处理报告清理任务异常退出：{error}");
            error_log::record_failure(
                "cleanup_failed",
                "clear_crashpad_pending",
                error.clone(),
                json!({
                    "protectionEnabled": protect_crashpad_pending,
                    "taskJoinFailed": true,
                }),
            );
            errors.push(error.clone());
            let mut cleanup = crashpad_pending_guard::CrashpadCleanupReport::default();
            cleanup.errors.push(error.clone());
            let mut snapshot = CrashpadPendingStatsSnapshot::idle(protect_crashpad_pending);
            snapshot.errors.push(error);
            (cleanup, snapshot)
        }
    };
    if clear_crashpad {
        state
            .crashpad_pending_stats
            .replace(crashpad_snapshot.clone());
    }

    Ok(json!({
        "status": if errors.is_empty() { "ok" } else { "partial" },
        "traceCleanup": trace_cleanup,
        "traceLogStatsBefore": trace_before,
        "crashpadCleanup": crashpad_cleanup,
        "traceProtectionEnabled": disable_trace_writes,
        "traceLogWriteProtectionActive": trace_log_write_protection_active,
        "crashpadProtectionEnabled": protect_crashpad_pending,
        "errors": errors,
        "traceLogStats": trace_snapshot,
        "crashpadPendingStats": crashpad_snapshot,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cleanup_requires_an_explicit_single_target() {
        let state = Arc::new(AppState::default());
        for args in [
            json!({}),
            json!({"target": null}),
            json!({"target": "all"}),
            json!({"target": 1}),
        ] {
            assert_eq!(
                clear_diagnostic_storage(&state, &args).await.unwrap_err(),
                "无效的诊断清理目标"
            );
        }
    }
}
