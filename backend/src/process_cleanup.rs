use anyhow::{Context, Result};

#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tokio::process::Command;

/// Stops every other Codey instance after the Codex instance owned by this
/// process exits. The caller remains alive long enough to restore Codex's
/// temporary configuration before invoking this function.
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
    let targets = other_process_ids(&process_name, current_pid).await?;
    if targets.is_empty() {
        return Ok(0);
    }

    signal_processes("-TERM", &targets).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let remaining = other_process_ids(&process_name, current_pid).await?;
    if !remaining.is_empty() {
        signal_processes("-KILL", &remaining).await?;
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

#[cfg(unix)]
async fn signal_processes(signal: &str, process_ids: &[u32]) -> Result<()> {
    let mut command = Command::new("kill");
    command.arg(signal);
    for process_id in process_ids {
        command.arg(process_id.to_string());
    }
    let status = command.status().await.context("终止 Codey 进程失败")?;
    // A target can finish between pgrep and kill. Re-enumeration below decides
    // whether a forceful second pass is necessary.
    if !status.success() {
        eprintln!("部分 Codey 进程在收到 {signal} 前已经退出：{status}");
    }
    Ok(())
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
        let status = tokio::process::Command::new("taskkill")
            .args(["/PID", &process_id.to_string(), "/T", "/F"])
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
