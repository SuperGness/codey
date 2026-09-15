use super::*;

fn ensure_repairable(windows: bool, mode: Option<&str>) -> Result<(), String> {
    if !windows {
        return Err("主进程注入修复仅支持 Windows".into());
    }
    match mode {
        Some("cli") => Ok(()),
        Some("node_options" | "inspector") => Err("主进程注入正常，无需修复".into()),
        _ => Err("尚未确认主进程注入异常，请先启动 Codex 并刷新状态".into()),
    }
}

async fn repair_app_path(state: &AppState) -> Result<PathBuf, String> {
    let runtime = state.runtime.lock().await;
    ensure_repairable(
        cfg!(windows),
        runtime
            .as_ref()
            .map(|runtime| runtime.maintenance.startup_injection_mode.as_str()),
    )?;
    Ok(runtime
        .as_ref()
        .expect("validated running runtime")
        .codex_app_path
        .clone())
}

pub(in crate::commands) async fn schedule_main_process_injection_repair(
    state: &Arc<AppState>,
) -> Result<Value, String> {
    let mut scheduled = state.restart_task.lock().await;
    ensure_runtime_can_start(state)?;
    repair_app_path(state).await?;
    if state.restart_in_progress.swap(true, Ordering::AcqRel) {
        return Err("Codex 正在重启或修复，请稍候".into());
    }
    let (cancel, cancel_rx) = oneshot::channel();
    let repair_state = Arc::clone(state);
    let task = tokio::spawn(async move {
        let _guard = RestartInProgressGuard {
            state: Arc::clone(&repair_state),
        };
        // Own the complete operation independently of the renderer request and
        // report even a worker panic through the native UI after Codex exits.
        let worker_state = Arc::clone(&repair_state);
        let result = tokio::spawn(run_repair(worker_state, cancel_rx))
            .await
            .unwrap_or_else(|error| Err(format!("主进程注入修复任务异常：{error}")));
        if let Err(error) = result {
            *repair_state.startup_error.write().await = Some(error.clone());
            error_log::record_failure(
                "runtime_repair_failed",
                "repair_main_process_injection",
                error.clone(),
                json!({}),
            );
            #[cfg(windows)]
            let _ = tokio::task::spawn_blocking(move || {
                rfd::MessageDialog::new()
                    .set_title("Codey 主进程注入修复失败")
                    .set_description(error)
                    .set_level(rfd::MessageLevel::Error)
                    .set_buttons(rfd::MessageButtons::Ok)
                    .show();
            })
            .await;
        }
    });
    *scheduled = Some(ScheduledRestart { cancel, task });
    Ok(json!({"status": "repairing"}))
}

async fn run_repair(state: Arc<AppState>, mut cancel: oneshot::Receiver<()>) -> Result<(), String> {
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(250)) => {},
        _ = &mut cancel => return Ok(()),
    }
    let _operation = tokio::select! {
        guard = state.runtime_operation.lock() => guard,
        _ = &mut cancel => return Ok(()),
    };
    ensure_runtime_can_start(&state)?;
    let app_path = repair_app_path(&state).await?;
    let previous_startup_error = state.startup_error.read().await.clone();
    run_repair_steps(
        || async {
            stop_codey_runtime_locked(&state).await?;
            // Invalidate the old exit event even if repair or restart fails.
            state.runtime_generation.fetch_add(1, Ordering::AcqRel);
            Ok(())
        },
        || async {
            ensure_runtime_can_start(&state)?;
            repair_runtime_file(app_path).await
        },
        || async { launch_codey_inner_locked(&state).await.map(|_| ()) },
        || async {
            let runtime = state.runtime.lock().await;
            let mode = runtime.as_ref().map(|runtime| runtime.maintenance.startup_injection_mode.as_str());
            if mode != Some("node_options") && mode != Some("inspector") {
                return Err("Codex 已重启，但未检测到主进程注入执行标记，修复未通过验证；当前仍使用兼容模式".into());
            }
            Ok(())
        },
    ).await?;
    {
        let mut startup_error = state.startup_error.write().await;
        clear_startup_error_if_unchanged(&mut startup_error, previous_startup_error.as_deref());
    }
    Ok(())
}

/// A successful repair only clears the startup error the runtime already had
/// when the repair started. An error another task records while the repair runs
/// stays visible, so success never hides an unrelated failure.
fn clear_startup_error_if_unchanged(startup_error: &mut Option<String>, previous: Option<&str>) {
    if startup_error.as_deref() == previous {
        *startup_error = None;
    }
}

async fn repair_runtime_file(app_path: PathBuf) -> Result<(), String> {
    #[cfg(windows)]
    {
        tokio::task::spawn_blocking(move || crate::electron_fuses::repair_node_options(&app_path))
            .await
            .map_err(|error| format!("运行时文件修复任务异常：{error}"))?
            .map_err(|error| format!("{error:#}"))
    }
    #[cfg(not(windows))]
    {
        let _ = app_path;
        Err("主进程注入修复仅支持 Windows".into())
    }
}

async fn run_repair_steps<S, SF, R, RF, L, LF, V, VF>(
    stop: S,
    repair: R,
    launch: L,
    verify: V,
) -> Result<(), String>
where
    S: FnOnce() -> SF,
    SF: std::future::Future<Output = Result<(), String>>,
    R: FnOnce() -> RF,
    RF: std::future::Future<Output = Result<(), String>>,
    L: FnOnce() -> LF,
    LF: std::future::Future<Output = Result<(), String>>,
    V: FnOnce() -> VF,
    VF: std::future::Future<Output = Result<(), String>>,
{
    stop()
        .await
        .map_err(|error| format!("退出 Codex 失败，未修改运行时：{error}"))?;
    if let Err(error) = repair().await {
        // The normal launcher can restore the existing CLI compatibility mode;
        // injection verification applies only after a successful file repair.
        return Err(match launch().await {
            Ok(()) => format!("修复失败：{error}\nCodex 已恢复启动，主进程注入仍需修复。"),
            Err(launch_error) => {
                format!("修复失败：{error}\n恢复启动 Codex 也失败：{launch_error}")
            }
        });
    }
    launch()
        .await
        .map_err(|error| format!("运行时已修复，但重启失败：{error}"))?;
    verify()
        .await
        .map_err(|error| format!("运行时已修复，但重启验证失败：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn only_confirmed_windows_cli_fallback_can_be_repaired() {
        assert!(ensure_repairable(true, Some("cli")).is_ok());
        for mode in [
            None,
            Some(""),
            Some("unknown"),
            Some("node_options"),
            Some("inspector"),
        ] {
            assert!(ensure_repairable(true, mode).is_err());
        }
        assert!(ensure_repairable(false, Some("cli")).is_err());
    }

    #[tokio::test]
    async fn cancelling_before_stop_does_not_touch_runtime_generation() {
        let state = Arc::new(AppState::default());
        state.runtime_generation.store(7, Ordering::Release);
        let (cancel, cancelled) = oneshot::channel();
        cancel.send(()).unwrap();
        assert!(run_repair(Arc::clone(&state), cancelled).await.is_ok());
        assert_eq!(state.runtime_generation.load(Ordering::Acquire), 7);
        assert!(!state.is_shutting_down());
    }

    #[tokio::test]
    async fn unavailable_runtime_does_not_schedule_repair_or_set_busy() {
        let state = Arc::new(AppState::default());
        assert!(
            schedule_main_process_injection_repair(&state)
                .await
                .is_err()
        );
        assert!(!state.restart_in_progress.load(Ordering::Acquire));
        assert!(state.restart_task.lock().await.is_none());
    }

    #[tokio::test]
    async fn repair_lifecycle_preserves_order_and_only_verifies_successful_repairs() {
        for failure in [
            None,
            Some("stop"),
            Some("repair"),
            Some("launch"),
            Some("verify"),
        ] {
            let calls = Mutex::new(Vec::new());
            let step = |name| {
                calls.lock().unwrap().push(name);
                std::future::ready(if failure == Some(name) {
                    Err(name.to_string())
                } else {
                    Ok(())
                })
            };
            let result = run_repair_steps(
                || step("stop"),
                || step("repair"),
                || step("launch"),
                || step("verify"),
            )
            .await;
            assert_eq!(result.is_ok(), failure.is_none());
            let expected = match failure {
                Some("stop") => vec!["stop"],
                Some("repair" | "launch") => vec!["stop", "repair", "launch"],
                _ => vec!["stop", "repair", "launch", "verify"],
            };
            assert_eq!(*calls.lock().unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn failed_repair_reports_both_original_error_and_recovery_result() {
        for recovery_fails in [false, true] {
            let calls = Mutex::new(Vec::new());
            let step = |name, result| {
                calls.lock().unwrap().push(name);
                std::future::ready(result)
            };
            let error = run_repair_steps(
                || step("stop", Ok(())),
                || step("repair", Err("拒绝访问 (os error 5)".into())),
                || {
                    step(
                        "launch",
                        if recovery_fails {
                            Err("恢复启动错误".into())
                        } else {
                            Ok(())
                        },
                    )
                },
                || step("verify", Err("兼容模式没有注入标记".into())),
            )
            .await
            .unwrap_err();
            assert_eq!(*calls.lock().unwrap(), ["stop", "repair", "launch"]);
            assert!(error.contains("修复失败：拒绝访问 (os error 5)"));
            assert!(!error.contains("运行时已修复"));
            assert!(!error.contains("没有注入标记"));
            if recovery_fails {
                assert!(error.contains("恢复启动 Codex 也失败：恢复启动错误"));
            } else {
                assert!(error.contains("Codex 已恢复启动"));
            }
        }
    }

    #[test]
    fn successful_repair_only_clears_the_error_it_started_with() {
        let mut unchanged_none = None;
        clear_startup_error_if_unchanged(&mut unchanged_none, None);
        assert_eq!(unchanged_none, None);

        let mut unchanged_error = Some("上次启动失败".to_string());
        clear_startup_error_if_unchanged(&mut unchanged_error, Some("上次启动失败"));
        assert_eq!(unchanged_error, None);

        // 修复期间其它任务记录的启动错误必须保留。
        let mut recorded_during_repair = Some("修复期间启动失败".to_string());
        clear_startup_error_if_unchanged(&mut recorded_during_repair, Some("上次启动失败"));
        assert_eq!(recorded_during_repair.as_deref(), Some("修复期间启动失败"));
        clear_startup_error_if_unchanged(&mut recorded_during_repair, None);
        assert_eq!(recorded_during_repair.as_deref(), Some("修复期间启动失败"));
    }
}
