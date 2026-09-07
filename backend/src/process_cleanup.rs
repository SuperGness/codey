use anyhow::{Context, Result};

#[cfg(any(windows, test))]
use std::collections::HashMap;
#[cfg(any(unix, windows, test))]
use std::collections::HashSet;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::time::Duration;

/// Stops every other Codey process and its descendants during final shutdown.
/// The caller remains alive long enough to stop its owned Codex tree and
/// restore temporary configuration before invoking this function.
pub async fn terminate_other_codey_processes() -> Result<usize> {
    #[cfg(unix)]
    {
        terminate_other_unix_codey_processes().await
    }

    #[cfg(windows)]
    {
        terminate_other_windows_codey_processes().await
    }
}

#[cfg(unix)]
async fn terminate_other_unix_codey_processes() -> Result<usize> {
    let executable_path = std::env::current_exe().context("读取当前 Codey 可执行文件路径失败")?;
    let current_pid = std::process::id();
    let initial_snapshot = crate::process_tree::unix_process_snapshot().await?;
    let roots = unix_codey_root_process_ids(&initial_snapshot, &executable_path, current_pid);
    if roots.is_empty() {
        return Ok(0);
    }
    let initial_targets =
        crate::process_tree::process_ids_with_descendants(&initial_snapshot, roots, current_pid);
    let targets =
        crate::process_tree::identities_for_process_ids(&initial_snapshot, &initial_targets);

    // 身份集合来自同一份初始快照，直接对其匹配结果发 SIGTERM；已退出的
    // 进程只会得到无害的 ESRCH。
    let initial_matches = crate::process_tree::matching_process_ids(&initial_snapshot, &targets);
    crate::process_tree::signal_processes(&initial_matches, libc::SIGTERM)?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    // 等待阶段只用 kill(0) 探测存活，不再每 50ms fork 一次 ps；PID 复用只会
    // 让等待更保守。真正强杀前再用一次完整快照复核启动身份。
    let mut alive = initial_matches;
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        alive.retain(|process_id| crate::process_tree::unix_process_alive(*process_id));
        if alive.is_empty() || tokio::time::Instant::now() >= deadline {
            break;
        }
    }
    if !alive.is_empty() {
        let snapshot = crate::process_tree::unix_process_snapshot().await?;
        let remaining = crate::process_tree::matching_process_ids(&snapshot, &targets);
        crate::process_tree::signal_processes(&remaining, libc::SIGKILL)?;
    }
    Ok(targets.len())
}

#[cfg(unix)]
fn unix_codey_root_process_ids(
    processes: &[crate::process_tree::UnixProcessInfo],
    executable_path: &Path,
    current_pid: u32,
) -> HashSet<u32> {
    processes
        .iter()
        .filter(|process| {
            process.process_id != current_pid
                && crate::process_tree::command_uses_path(&process.command, executable_path)
        })
        .map(|process| process.process_id)
        .collect()
}

#[cfg(windows)]
async fn terminate_other_windows_codey_processes() -> Result<usize> {
    let current_pid = std::process::id();
    let executable_path = std::env::current_exe().context("读取当前 Codey 可执行文件路径失败")?;
    let processes = codey_runtime_core::windows_enumerate_processes()
        .context("检测待清理的 Windows Codey 进程失败")?;
    let roots = processes
        .iter()
        .filter(|process| {
            process.process_id != current_pid
                && process.executable_path.as_deref().is_some_and(|path| {
                    codey_runtime_core::windows_process_paths_equal(path, &executable_path)
                })
                && process.creation_time.is_some()
        })
        .map(|process| process.process_id)
        .collect::<HashSet<_>>();
    let process_identities = processes
        .iter()
        .filter_map(|process| {
            Some((
                process.process_id,
                process.parent_process_id,
                process.creation_time?,
            ))
        })
        .collect::<Vec<_>>();
    let target_ids =
        process_ids_with_descendants_from_identities(&process_identities, roots, current_pid);
    let targets = processes
        .into_iter()
        .filter(|process| target_ids.contains(&process.process_id))
        .filter_map(|process| {
            Some((
                process.process_id,
                process.executable_path?,
                process.creation_time?,
            ))
        })
        .collect::<Vec<_>>();

    let mut terminated = 0;
    for (process_id, executable_path, creation_time) in targets {
        if codey_runtime_core::windows_terminate_process_if_matches(
            process_id,
            &executable_path,
            creation_time,
        ) {
            terminated += 1;
        }
    }
    Ok(terminated)
}

#[cfg(any(windows, test))]
fn process_ids_with_descendants_from_identities(
    processes: &[(u32, u32, u64)],
    roots: HashSet<u32>,
    excluded_process_id: u32,
) -> HashSet<u32> {
    let creation_times = processes
        .iter()
        .map(|(process_id, _, creation_time)| (*process_id, *creation_time))
        .collect::<HashMap<_, _>>();
    let mut process_ids = roots
        .into_iter()
        .filter(|process_id| {
            *process_id > 1
                && *process_id != excluded_process_id
                && creation_times.contains_key(process_id)
        })
        .collect::<HashSet<_>>();
    loop {
        let previous_len = process_ids.len();
        for (process_id, parent_process_id, creation_time) in processes {
            if *process_id > 1
                && *process_id != excluded_process_id
                && process_ids.contains(parent_process_id)
                && creation_times
                    .get(parent_process_id)
                    .is_some_and(|parent_creation_time| parent_creation_time <= creation_time)
            {
                process_ids.insert(*process_id);
            }
        }
        if process_ids.len() == previous_len {
            return process_ids;
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::process_tree::{
        identities_for_process_ids, matching_process_ids, parse_unix_process_snapshot,
    };

    #[test]
    fn unix_root_filter_requires_the_current_executable_path() {
        let processes = parse_unix_process_snapshot(
            b"100 1 100 Thu Jul 23 19:23:12 2026 /Applications/Codey.app/Contents/MacOS/codey\n\
              200 1 200 Thu Jul 23 19:23:13 2026 /tmp/other/codey\n\
              300 1 300 Thu Jul 23 19:23:14 2026 /Applications/Codey.app/Contents/MacOS/codey --watch\n",
        );

        assert_eq!(
            unix_codey_root_process_ids(
                &processes,
                Path::new("/Applications/Codey.app/Contents/MacOS/codey"),
                100,
            ),
            HashSet::from([300])
        );
    }

    #[test]
    fn fixed_cleanup_identities_never_adopt_a_new_same_path_process() {
        let initial = parse_unix_process_snapshot(
            b"100 1 100 Thu Jul 23 19:23:12 2026 /Applications/Codey.app/Contents/MacOS/codey\n",
        );
        let identities = identities_for_process_ids(&initial, &HashSet::from([100]));
        let later = parse_unix_process_snapshot(
            b"100 1 100 Thu Jul 23 19:23:12 2026 /Applications/Codey.app/Contents/MacOS/codey\n\
              200 1 200 Thu Jul 23 19:23:13 2026 /Applications/Codey.app/Contents/MacOS/codey\n",
        );

        assert_eq!(
            matching_process_ids(&later, &identities),
            HashSet::from([100])
        );
    }

    #[test]
    fn descendant_filter_freezes_the_initial_tree_and_rejects_stale_parent_ids() {
        let initial = [
            (100, 1, 200),
            (101, 100, 201),
            (102, 101, 202),
            (200, 100, 199),
            (300, 1, 203),
        ];

        assert_eq!(
            process_ids_with_descendants_from_identities(&initial, HashSet::from([100]), 999),
            HashSet::from([100, 101, 102])
        );
    }
}
