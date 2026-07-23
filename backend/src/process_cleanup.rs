use anyhow::{Context, Result};

#[cfg(unix)]
use std::collections::HashSet;
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tokio::process::Command;

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
    let process_name = std::env::current_exe()
        .context("读取当前 Codey 可执行文件路径失败")?
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("codey")
        .to_string();
    let current_pid = std::process::id();
    let roots = other_process_ids(&process_name, current_pid).await?;
    if roots.is_empty() {
        return Ok(0);
    }
    let initial_snapshot = crate::process_tree::unix_process_snapshot().await?;
    let initial_targets =
        crate::process_tree::process_ids_with_descendants(&initial_snapshot, roots, current_pid);
    let mut targets =
        crate::process_tree::identities_for_process_ids(&initial_snapshot, &initial_targets);

    crate::process_tree::signal_processes(&targets.keys().copied().collect(), libc::SIGTERM)?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let remaining = loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let snapshot = crate::process_tree::unix_process_snapshot().await?;
        let roots = other_process_ids(&process_name, current_pid).await?;
        let discovered =
            crate::process_tree::process_ids_with_descendants(&snapshot, roots, current_pid);
        let new_process_ids = discovered
            .into_iter()
            .filter(|process_id| !targets.contains_key(process_id))
            .collect::<HashSet<_>>();
        if !new_process_ids.is_empty() {
            crate::process_tree::signal_processes(&new_process_ids, libc::SIGTERM)?;
            targets.extend(crate::process_tree::identities_for_process_ids(
                &snapshot,
                &new_process_ids,
            ));
        }
        let remaining = crate::process_tree::matching_process_ids(&snapshot, &targets);
        if remaining.is_empty() || tokio::time::Instant::now() >= deadline {
            break remaining;
        }
    };
    if !remaining.is_empty() {
        crate::process_tree::signal_processes(&remaining, libc::SIGKILL)?;
    }
    Ok(targets.len())
}

#[cfg(unix)]
async fn other_process_ids(process_name: &str, current_pid: u32) -> Result<Vec<u32>> {
    let output = Command::new("pgrep")
        .args(["-x", process_name])
        .output()
        .await
        .context("枚举 Codey 进程失败")?;
    if !output.status.success() {
        if output.status.code() == Some(1) {
            return Ok(Vec::new());
        }
        anyhow::bail!("枚举 Codey 进程失败：pgrep 返回 {}", output.status);
    }
    Ok(parse_other_process_ids(&output.stdout, current_pid))
}

#[cfg(unix)]
fn parse_other_process_ids(output: &[u8], current_pid: u32) -> Vec<u32> {
    String::from_utf8_lossy(output)
        .split_whitespace()
        .filter_map(|value| value.parse::<u32>().ok())
        .filter(|process_id| *process_id != current_pid)
        .collect()
}

#[cfg(windows)]
async fn terminate_other_windows_codey_processes() -> Result<usize> {
    let current_pid = std::process::id();
    let executable_name = std::env::current_exe()
        .context("读取当前 Codey 可执行文件路径失败")?
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("codey.exe")
        .to_string();
    let targets = codex_plus_core::windows_enumerate_processes()
        .into_iter()
        .filter(|process| {
            process.process_id != current_pid
                && process.exe_file.eq_ignore_ascii_case(&executable_name)
        })
        .map(|process| process.process_id)
        .collect::<Vec<_>>();

    for process_id in &targets {
        let mut command = tokio::process::Command::new("taskkill");
        command
            .args(["/PID", &process_id.to_string(), "/T", "/F"])
            .creation_flags(codex_plus_core::windows_create_no_window());
        let status = command
            .status()
            .await
            .with_context(|| format!("终止 Codey 进程 {process_id} 失败"))?;
        if !status.success() {
            eprintln!("Codey 进程 {process_id} 可能已经退出：taskkill 返回 {status}");
        }
    }
    Ok(targets.len())
}

#[cfg(all(test, unix))]
mod tests {
    use super::parse_other_process_ids;

    #[test]
    fn process_id_parser_excludes_the_current_process_and_invalid_rows() {
        assert_eq!(
            parse_other_process_ids(b"100\ninvalid\n200\n300\n", 200),
            vec![100, 300]
        );
    }
}
