use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

#[cfg(windows)]
use anyhow::Context;
use anyhow::Result;
#[cfg(windows)]
use tokio::process::Command;

#[cfg(windows)]
use super::{SpawnedCodex, build_codex_command, reap_child_after_cleanup};
#[cfg(windows)]
use crate::error_log;

#[cfg(windows)]
const WINDOWS_CODEX_STOP_TIMEOUT: Duration = Duration::from_secs(8);
#[cfg(windows)]
const WINDOWS_STARTUP_PATCH_FAILURE_STOP_TIMEOUT: Duration = Duration::from_secs(20);

#[cfg(any(windows, test))]
pub(super) fn normalized_windows_path(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_start_matches(r"\\?\")
        .to_ascii_lowercase()
}

#[cfg(windows)]
pub(super) async fn spawn_windows_codex(
    app_dir: &std::path::Path,
    debug_port: u16,
    extra_args: &[String],
) -> Result<SpawnedCodex> {
    if let Some(activation) =
        codey_runtime_core::launcher::build_packaged_activation(app_dir, debug_port, extra_args)
        && let codey_runtime_core::launcher::CodexLaunch::PackagedActivation {
            app_user_model_id,
            arguments,
            ..
        } = activation
    {
        let existing_process_ids = codey_runtime_core::windows_enumerate_processes()
            .into_iter()
            .map(|process| process.process_id)
            .collect::<HashSet<_>>();
        let mut process_id =
            codey_runtime_core::launcher::activate_packaged_app(&app_user_model_id, &arguments)
                .await?;
        if activation_reused_existing_process(&existing_process_ids, process_id) {
            // ActivateApplication returns the instance that fulfills the launch
            // contract; that can be an already-running single instance. Electron
            // only consumes Chromium/Node command-line switches at process start,
            // so a reused instance cannot be trusted to own either debug port.
            terminate_windows_codex_processes(app_dir, Some(process_id))
                .await
                .context("停止被 Windows Store 激活复用的旧 Codex 实例失败")?;
            let retry_existing_process_ids = codey_runtime_core::windows_enumerate_processes()
                .into_iter()
                .map(|process| process.process_id)
                .collect::<HashSet<_>>();
            process_id =
                codey_runtime_core::launcher::activate_packaged_app(&app_user_model_id, &arguments)
                    .await
                    .context("重新激活 Windows Store Codex 失败")?;
            if activation_reused_existing_process(&retry_existing_process_ids, process_id) {
                anyhow::bail!(
                    "Windows Store Codex 再次复用了已有进程 {process_id}，本次 CDP 启动参数未能可靠生效"
                );
            }
        }
        return Ok(SpawnedCodex {
            child: None,
            process_id: Some(process_id),
            performance_status: String::new(),
            performance_detail: String::new(),
        });
    }

    let command = build_codex_command(app_dir, debug_port, extra_args);
    let executable = command
        .first()
        .ok_or_else(|| anyhow::anyhow!("Codex 启动命令为空"))?;
    let mut child_command = Command::new(executable);
    child_command.args(&command[1..]);
    // A stale WSL_DISTRO_NAME inherited by the native Windows app makes
    // current Codex builds synchronously probe wsl.exe during startup.
    child_command.env_remove("WSL_DISTRO_NAME");
    child_command.creation_flags(codey_runtime_core::windows_create_no_window());
    let child = child_command
        .spawn()
        .with_context(|| format!("启动 Codex 失败：{executable}"))?;
    let process_id = child.id();
    Ok(SpawnedCodex {
        child: Some(child),
        process_id,
        performance_status: String::new(),
        performance_detail: String::new(),
    })
}

#[cfg(any(windows, test))]
pub(super) fn activation_reused_existing_process(
    existing_process_ids: &HashSet<u32>,
    process_id: u32,
) -> bool {
    existing_process_ids.contains(&process_id)
}

#[cfg(any(windows, test))]
pub(super) fn process_creation_identity_matches(
    expected_creation_time: Option<u64>,
    actual_creation_time: Option<u64>,
) -> bool {
    match (expected_creation_time, actual_creation_time) {
        (Some(expected), Some(actual)) => expected == actual,
        _ => true,
    }
}

#[cfg(windows)]
pub(super) async fn stop_windows_spawned_codex(
    spawned: &mut SpawnedCodex,
    app_dir: &std::path::Path,
) -> Result<()> {
    let process_id = spawned.process_id.take();
    let process_stop = terminate_windows_codex_processes_with_timeout(
        app_dir,
        process_id,
        WINDOWS_STARTUP_PATCH_FAILURE_STOP_TIMEOUT,
    )
    .await;
    if let Some(child) = spawned.child.take() {
        reap_child_after_cleanup(child, "reap_child_after_startup_patch_failure").await;
    }
    if let Err(error) = &process_stop {
        error_log::record_failure(
            "cleanup_failed",
            "cleanup_windows_after_startup_patch_failure",
            format!("{error:#}"),
            serde_json::json!({
                "appPath": app_dir,
                "processId": process_id,
            }),
        );
        eprintln!("Codex 启动失败后的进程清理失败：{error:#}");
    }
    process_stop
}

#[cfg(target_os = "macos")]
pub(super) fn build_fresh_macos_open_command(
    app_dir: &std::path::Path,
    debug_port: u16,
    extra_args: &[String],
) -> Vec<String> {
    let mut command =
        codey_runtime_core::launcher::build_macos_open_command(app_dir, debug_port, extra_args);
    if command.first().map(String::as_str) == Some("open")
        && !command.iter().any(|part| part == "-n" || part == "--new")
    {
        command.insert(1, "-n".to_string());
    }
    command
}

#[cfg(target_os = "macos")]
pub(super) async fn stop_macos_codex(
    inspector_argument: &str,
    app_dir: &std::path::Path,
    process_id: Option<u32>,
    process_group_id: Option<u32>,
) -> Result<()> {
    terminate_unix_codex_processes(
        app_dir,
        process_id,
        process_group_id,
        Some(inspector_argument),
    )
    .await
    .map(|_| ())
}

#[cfg(unix)]
pub(super) fn owned_unix_codex_process_ids(
    processes: &[crate::process_tree::UnixProcessInfo],
    app_dir: &Path,
    process_id: Option<u32>,
    process_group_id: Option<u32>,
    launch_marker: Option<&str>,
) -> HashSet<u32> {
    let current_process_id = std::process::id();
    let roots = processes.iter().filter_map(|process| {
        let matches_root = Some(process.process_id) == process_id
            || Some(process.process_group_id) == process_group_id
            || crate::process_tree::command_uses_path(&process.command, app_dir)
            || launch_marker.is_some_and(|marker| {
                crate::process_tree::command_has_argument(&process.command, marker)
            });
        matches_root.then_some(process.process_id)
    });
    crate::process_tree::process_ids_with_descendants(processes, roots, current_process_id)
}

#[cfg(unix)]
fn owned_unix_process_group(
    processes: &[crate::process_tree::UnixProcessInfo],
    app_dir: &Path,
    process_id: Option<u32>,
    process_group_id: Option<u32>,
    launch_marker: Option<&str>,
) -> Option<u32> {
    let process_group_id = process_group_id?;
    processes
        .iter()
        .any(|process| {
            process.process_group_id == process_group_id
                && (Some(process.process_id) == process_id
                    || crate::process_tree::command_uses_path(&process.command, app_dir)
                    || launch_marker.is_some_and(|marker| {
                        crate::process_tree::command_has_argument(&process.command, marker)
                    }))
        })
        .then_some(process_group_id)
}

#[cfg(unix)]
pub(super) async fn terminate_unix_codex_processes(
    app_dir: &Path,
    process_id: Option<u32>,
    process_group_id: Option<u32>,
    launch_marker: Option<&str>,
) -> Result<usize> {
    let mut known_processes = HashMap::new();
    let mut processes = crate::process_tree::unix_process_snapshot().await?;
    let initially_owned = owned_unix_codex_process_ids(
        &processes,
        app_dir,
        process_id,
        process_group_id,
        launch_marker,
    );
    known_processes.extend(crate::process_tree::identities_for_process_ids(
        &processes,
        &initially_owned,
    ));

    let owned_process_group = owned_unix_process_group(
        &processes,
        app_dir,
        process_id,
        process_group_id,
        launch_marker,
    );
    crate::process_tree::signal_process_group(owned_process_group, libc::SIGTERM)?;
    crate::process_tree::signal_processes(
        &known_processes.keys().copied().collect(),
        libc::SIGTERM,
    )?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let poll_delays = [
        Duration::from_millis(100),
        Duration::from_millis(200),
        Duration::from_millis(350),
        Duration::from_millis(550),
        Duration::from_millis(800),
    ];
    let mut poll_index = 0usize;
    let remaining = loop {
        let currently_owned = owned_unix_codex_process_ids(
            &processes,
            app_dir,
            process_id,
            process_group_id,
            launch_marker,
        );
        let newly_discovered = currently_owned
            .into_iter()
            .filter(|process_id| !known_processes.contains_key(process_id))
            .collect::<HashSet<_>>();
        if !newly_discovered.is_empty() {
            crate::process_tree::signal_processes(&newly_discovered, libc::SIGTERM)?;
            known_processes.extend(crate::process_tree::identities_for_process_ids(
                &processes,
                &newly_discovered,
            ));
        }
        let remaining = crate::process_tree::matching_process_ids(&processes, &known_processes);
        if remaining.is_empty() || tokio::time::Instant::now() >= deadline {
            break remaining;
        }
        let remaining_time = deadline.saturating_duration_since(tokio::time::Instant::now());
        let delay = poll_delays
            .get(poll_index)
            .copied()
            .unwrap_or(Duration::from_millis(800))
            .min(remaining_time);
        poll_index = poll_index.saturating_add(1);
        tokio::time::sleep(delay).await;
        processes = crate::process_tree::unix_process_snapshot().await?;
    };

    if !remaining.is_empty() {
        let owned_process_group = process_group_id.filter(|process_group_id| {
            processes.iter().any(|process| {
                process.process_group_id == *process_group_id
                    && remaining.contains(&process.process_id)
            })
        });
        crate::process_tree::signal_process_group(owned_process_group, libc::SIGKILL)?;
        crate::process_tree::signal_processes(&remaining, libc::SIGKILL)?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let final_snapshot = crate::process_tree::unix_process_snapshot().await?;
        let live_process_ids =
            crate::process_tree::matching_process_ids(&final_snapshot, &known_processes);
        let stubborn_processes = remaining
            .intersection(&live_process_ids)
            .copied()
            .collect::<Vec<_>>();
        if !stubborn_processes.is_empty() {
            anyhow::bail!("强制停止 Codex 进程超时：{stubborn_processes:?}");
        }
    }
    Ok(known_processes.len())
}

#[cfg(target_os = "macos")]
pub(super) fn macos_main_executable_is_running(
    processes: &[crate::process_tree::UnixProcessInfo],
    executable: &std::path::Path,
) -> bool {
    processes
        .iter()
        .any(|process| crate::process_tree::command_uses_path(&process.command, executable))
}

#[cfg(target_os = "macos")]
pub(super) async fn macos_codex_is_running(app_dir: &std::path::Path) -> Result<bool> {
    // 启动前只检查 App 的主可执行文件，忽略 app-server 和 Chromium helper。
    let executable = codey_runtime_core::app_paths::build_codex_executable(app_dir);
    let processes = crate::process_tree::unix_process_snapshot().await?;
    Ok(macos_main_executable_is_running(&processes, &executable))
}

#[cfg(any(windows, test))]
fn windows_path_is_within(path: &Path, directory: &Path) -> bool {
    let path = normalized_windows_path(path);
    let directory = normalized_windows_path(directory);
    path == directory
        || path
            .strip_prefix(&directory)
            .is_some_and(|rest| rest.starts_with('\\'))
}

#[cfg(any(windows, test))]
pub(super) fn windows_owned_process_ids_from_snapshot<'a>(
    app_dir: &Path,
    process_id: Option<u32>,
    processes: impl IntoIterator<Item = (u32, u32, Option<&'a Path>)>,
) -> HashSet<u32> {
    let processes = processes.into_iter().collect::<Vec<_>>();
    let mut process_ids = processes
        .iter()
        .filter(|(candidate_process_id, _, executable_path)| {
            Some(*candidate_process_id) == process_id
                || executable_path.is_some_and(|path| windows_path_is_within(path, app_dir))
        })
        .map(|(candidate_process_id, _, _)| *candidate_process_id)
        .collect::<HashSet<_>>();
    windows_extend_tracked_descendants_from_snapshot(&mut process_ids, processes);
    process_ids
}

#[cfg(any(windows, test))]
pub(super) fn windows_extend_tracked_descendants_from_snapshot<'a>(
    process_ids: &mut HashSet<u32>,
    processes: impl IntoIterator<Item = (u32, u32, Option<&'a Path>)>,
) {
    let processes = processes.into_iter().collect::<Vec<_>>();
    loop {
        let previous_len = process_ids.len();
        for (candidate_process_id, parent_process_id, _) in &processes {
            if process_ids.contains(parent_process_id) {
                process_ids.insert(*candidate_process_id);
            }
        }
        if process_ids.len() == previous_len {
            break;
        }
    }
}

#[cfg(windows)]
pub(super) async fn terminate_windows_codex_processes(
    app_dir: &Path,
    process_id: Option<u32>,
) -> Result<()> {
    terminate_windows_codex_processes_with_timeout(app_dir, process_id, WINDOWS_CODEX_STOP_TIMEOUT)
        .await
}

#[cfg(windows)]
async fn terminate_windows_codex_processes_with_timeout(
    app_dir: &Path,
    process_id: Option<u32>,
    stop_timeout: Duration,
) -> Result<()> {
    let processes = codey_runtime_core::windows_enumerate_processes();
    let mut process_ids = windows_owned_process_ids_from_snapshot(
        app_dir,
        process_id,
        processes.iter().map(|process| {
            (
                process.process_id,
                process.parent_process_id,
                process.executable_path.as_deref(),
            )
        }),
    );
    process_ids.remove(&std::process::id());
    let mut ordered_process_ids = process_ids.iter().copied().collect::<Vec<_>>();
    ordered_process_ids.sort_by_key(|candidate| {
        let mut depth = 0_usize;
        let mut current = *candidate;
        while let Some(parent_process_id) = processes
            .iter()
            .find(|process| process.process_id == current)
            .map(|process| process.parent_process_id)
            .filter(|parent_process_id| process_ids.contains(parent_process_id))
        {
            depth = depth.saturating_add(1);
            current = parent_process_id;
        }
        std::cmp::Reverse(depth)
    });
    let mut expected_creation_times = process_ids
        .iter()
        .filter_map(|process_id| {
            processes
                .iter()
                .find(|process| process.process_id == *process_id)
                .map(|process| (*process_id, process.creation_time))
        })
        .collect::<HashMap<_, _>>();
    for process_id in ordered_process_ids {
        let _terminated_natively = processes
            .iter()
            .find(|process| process.process_id == process_id)
            .is_some_and(
                |process| match (&process.executable_path, process.creation_time) {
                    (Some(path), Some(creation_time)) => {
                        codey_runtime_core::windows_terminate_process_if_matches(
                            process.process_id,
                            path,
                            creation_time,
                        )
                    }
                    (_, Some(creation_time)) => {
                        codey_runtime_core::windows_terminate_process_if_creation_matches(
                            process.process_id,
                            creation_time,
                        )
                    }
                    _ => false,
                },
            );
    }
    let deadline = tokio::time::Instant::now() + stop_timeout;
    loop {
        let current_processes = codey_runtime_core::windows_enumerate_processes();
        let previous_process_ids = process_ids.clone();
        windows_extend_tracked_descendants_from_snapshot(
            &mut process_ids,
            current_processes.iter().map(|process| {
                (
                    process.process_id,
                    process.parent_process_id,
                    process.executable_path.as_deref(),
                )
            }),
        );
        for discovered_process_id in process_ids.difference(&previous_process_ids) {
            let Some(process) = current_processes
                .iter()
                .find(|process| process.process_id == *discovered_process_id)
            else {
                continue;
            };
            expected_creation_times.insert(*discovered_process_id, process.creation_time);
            match (&process.executable_path, process.creation_time) {
                (Some(path), Some(creation_time)) => {
                    let _terminated_natively =
                        codey_runtime_core::windows_terminate_process_if_matches(
                            process.process_id,
                            path,
                            creation_time,
                        );
                }
                (_, Some(creation_time)) => {
                    let _terminated_natively =
                        codey_runtime_core::windows_terminate_process_if_creation_matches(
                            process.process_id,
                            creation_time,
                        );
                }
                _ => {}
            }
        }
        let current = current_processes
            .iter()
            .filter(|process| process_ids.contains(&process.process_id))
            .map(|process| {
                (
                    process.process_id,
                    process.exe_file.clone(),
                    process.creation_time,
                )
            })
            .collect::<Vec<_>>();
        let remaining = windows_stop_survivors(&expected_creation_times, &current, &process_ids);
        if remaining.is_empty() {
            return Ok(());
        }
        if tokio::time::Instant::now() < deadline {
            // Windows exits lag behind TerminateProcess (pending I/O,
            // throttled children, antivirus scans) and a first attempt can
            // race with process teardown. Re-issue creation-identity-checked
            // terminations while waiting instead of giving up on survivors.
            for (process_id, _, expected_creation_time) in &remaining {
                if let Some(expected_creation_time) = expected_creation_time {
                    let _terminated_again =
                        codey_runtime_core::windows_terminate_process_if_creation_matches(
                            *process_id,
                            *expected_creation_time,
                        );
                }
            }
        } else {
            anyhow::bail!(
                "无法安全停止 Windows Codex 进程：{}",
                windows_stop_failure_summary(&remaining),
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(any(windows, test))]
pub(super) fn windows_stop_survivors(
    expected_creation_times: &HashMap<u32, Option<u64>>,
    current: &[(u32, String, Option<u64>)],
    targets: &HashSet<u32>,
) -> Vec<(u32, String, Option<u64>)> {
    current
        .iter()
        .filter(|(process_id, _, creation_time)| {
            targets.contains(process_id)
                && expected_creation_times
                    .get(process_id)
                    .is_some_and(|expected| {
                        process_creation_identity_matches(*expected, *creation_time)
                    })
        })
        .map(|(process_id, exe_file, _)| {
            // Retry termination must retain the identity captured before the
            // first attempt. A timestamp learned only after that attempt could
            // belong to a different process that has reused the target pid.
            let expected_creation_time = expected_creation_times.get(process_id).copied().flatten();
            (*process_id, exe_file.clone(), expected_creation_time)
        })
        .collect()
}

#[cfg(any(windows, test))]
pub(super) fn windows_stop_failure_summary(remaining: &[(u32, String, Option<u64>)]) -> String {
    const MAX_LISTED: usize = 5;
    let listed = remaining
        .iter()
        .take(MAX_LISTED)
        .map(|(process_id, exe_file, _)| format!("{exe_file}({process_id})"))
        .collect::<Vec<_>>()
        .join("、");
    if remaining.len() > MAX_LISTED {
        format!(
            "{} 个进程仍在运行：{listed} 等共 {} 个",
            remaining.len(),
            remaining.len()
        )
    } else {
        format!("{} 个进程仍在运行：{listed}", remaining.len())
    }
}
