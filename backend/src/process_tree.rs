#[cfg(unix)]
use std::collections::{HashMap, HashSet};
#[cfg(unix)]
use std::path::Path;

#[cfg(unix)]
use anyhow::{Context, Result};
#[cfg(unix)]
use tokio::process::Command;

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnixProcessInfo {
    pub process_id: u32,
    pub parent_process_id: u32,
    pub process_group_id: u32,
    pub start_time: String,
    pub command: String,
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnixProcessIdentity {
    start_time: String,
    command: String,
}

#[cfg(unix)]
pub async fn unix_process_snapshot() -> Result<Vec<UnixProcessInfo>> {
    let output = Command::new("ps")
        .env("LC_ALL", "C")
        .args(["-axo", "pid=,ppid=,pgid=,lstart=,command="])
        .output()
        .await
        .context("枚举进程树失败")?;
    if !output.status.success() {
        anyhow::bail!("枚举进程树失败：ps 返回 {}", output.status);
    }
    Ok(parse_unix_process_snapshot(&output.stdout))
}

#[cfg(unix)]
pub fn parse_unix_process_snapshot(output: &[u8]) -> Vec<UnixProcessInfo> {
    String::from_utf8_lossy(output)
        .lines()
        .filter_map(parse_unix_process_line)
        .collect()
}

#[cfg(unix)]
fn parse_unix_process_line(line: &str) -> Option<UnixProcessInfo> {
    let (process_id, remainder) = take_process_field(line)?;
    let (parent_process_id, remainder) = take_process_field(remainder)?;
    let (process_group_id, mut remainder) = take_process_field(remainder)?;
    let mut start_time_parts = Vec::with_capacity(5);
    for _ in 0..5 {
        let (part, next) = take_process_field(remainder)?;
        start_time_parts.push(part);
        remainder = next;
    }
    let command = remainder;
    let command = command.trim_start();
    if command.is_empty() {
        return None;
    }
    Some(UnixProcessInfo {
        process_id: process_id.parse().ok()?,
        parent_process_id: parent_process_id.parse().ok()?,
        process_group_id: process_group_id.parse().ok()?,
        start_time: start_time_parts.join(" "),
        command: command.to_string(),
    })
}

#[cfg(unix)]
fn take_process_field(value: &str) -> Option<(&str, &str)> {
    let value = value.trim_start();
    let separator = value.find(char::is_whitespace)?;
    let (field, remainder) = value.split_at(separator);
    (!field.is_empty()).then_some((field, remainder))
}

#[cfg(unix)]
pub fn process_ids_with_descendants(
    processes: &[UnixProcessInfo],
    roots: impl IntoIterator<Item = u32>,
    excluded_process_id: u32,
) -> HashSet<u32> {
    let mut process_ids = roots
        .into_iter()
        .filter(|process_id| *process_id > 1 && *process_id != excluded_process_id)
        .collect::<HashSet<_>>();
    loop {
        let previous_len = process_ids.len();
        for process in processes {
            if process.process_id != excluded_process_id
                && process.process_id > 1
                && process_ids.contains(&process.parent_process_id)
            {
                process_ids.insert(process.process_id);
            }
        }
        if process_ids.len() == previous_len {
            break;
        }
    }
    process_ids
}

#[cfg(unix)]
pub fn identities_for_process_ids(
    processes: &[UnixProcessInfo],
    process_ids: &HashSet<u32>,
) -> HashMap<u32, UnixProcessIdentity> {
    processes
        .iter()
        .filter(|process| process_ids.contains(&process.process_id))
        .map(|process| {
            (
                process.process_id,
                UnixProcessIdentity {
                    start_time: process.start_time.clone(),
                    command: process.command.clone(),
                },
            )
        })
        .collect()
}

#[cfg(unix)]
pub fn matching_process_ids(
    processes: &[UnixProcessInfo],
    identities: &HashMap<u32, UnixProcessIdentity>,
) -> HashSet<u32> {
    processes
        .iter()
        .filter_map(|process| {
            let identity = identities.get(&process.process_id)?;
            (identity.start_time == process.start_time && identity.command == process.command)
                .then_some(process.process_id)
        })
        .collect()
}

#[cfg(unix)]
pub fn command_uses_path(command: &str, path: &Path) -> bool {
    let mut candidates = vec![path.to_path_buf()];
    if let Ok(canonical) = std::fs::canonicalize(path)
        && canonical != path
    {
        candidates.push(canonical);
    }
    candidates.iter().any(|candidate| {
        let candidate = candidate.to_string_lossy();
        command == candidate
            || command
                .strip_prefix(candidate.as_ref())
                .is_some_and(|rest| {
                    rest.starts_with(std::path::MAIN_SEPARATOR)
                        || rest.chars().next().is_some_and(char::is_whitespace)
                })
    })
}

#[cfg(unix)]
pub fn command_has_argument(command: &str, argument: &str) -> bool {
    if argument.is_empty() {
        return false;
    }
    command.match_indices(argument).any(|(index, value)| {
        let before = &command[..index];
        let after = &command[index + value.len()..];
        before.chars().next_back().is_none_or(char::is_whitespace)
            && after.chars().next().is_none_or(char::is_whitespace)
    })
}

#[cfg(unix)]
pub fn signal_processes(process_ids: &HashSet<u32>, signal: libc::c_int) -> Result<()> {
    let mut first_error = None;
    for process_id in process_ids {
        let Ok(process_id) = libc::pid_t::try_from(*process_id) else {
            continue;
        };
        if unsafe { libc::kill(process_id, signal) } == 0 {
            continue;
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) && first_error.is_none() {
            first_error = Some(error);
        }
    }
    match first_error {
        Some(error) => Err(error).context("终止进程树失败"),
        None => Ok(()),
    }
}

#[cfg(unix)]
pub fn signal_process_group(process_group_id: Option<u32>, signal: libc::c_int) -> Result<()> {
    let Some(process_group_id) = process_group_id.filter(|value| *value > 1) else {
        return Ok(());
    };
    let Ok(process_group_id) = libc::pid_t::try_from(process_group_id) else {
        return Ok(());
    };
    if unsafe { libc::kill(-process_group_id, signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error).context("终止进程组失败")
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn snapshot_parser_preserves_commands_with_spaces() {
        assert_eq!(
            parse_unix_process_snapshot(
                b"  100     1   100 Thu Jul 23 19:23:12 2026 /Applications/ChatGPT.app/Contents/MacOS/ChatGPT --flag\n\
                  101   100   101 Thu Jul 23 19:23:13 2026 node ./mcp/server.mjs\n\
                  invalid row\n"
            ),
            vec![
                UnixProcessInfo {
                    process_id: 100,
                    parent_process_id: 1,
                    process_group_id: 100,
                    start_time: "Thu Jul 23 19:23:12 2026".to_string(),
                    command: "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT --flag".to_string(),
                },
                UnixProcessInfo {
                    process_id: 101,
                    parent_process_id: 100,
                    process_group_id: 101,
                    start_time: "Thu Jul 23 19:23:13 2026".to_string(),
                    command: "node ./mcp/server.mjs".to_string(),
                },
            ]
        );
    }

    #[test]
    fn descendants_follow_the_complete_parent_chain() {
        let processes = parse_unix_process_snapshot(
            b"100 1 100 Thu Jul 23 19:23:12 2026 /app/Codex\n\
              101 100 100 Thu Jul 23 19:23:13 2026 /app/Codex Helper\n\
              102 101 102 Thu Jul 23 19:23:14 2026 node ./mcp/server.mjs\n\
              200 1 200 Thu Jul 23 19:23:15 2026 unrelated\n",
        );
        assert_eq!(
            process_ids_with_descendants(&processes, [100], 999),
            HashSet::from([100, 101, 102])
        );
    }

    #[test]
    fn path_match_requires_a_path_or_argument_boundary() {
        let app = Path::new("/Applications/ChatGPT.app");
        assert!(command_uses_path(
            "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT --flag",
            app
        ));
        assert!(!command_uses_path(
            "/Applications/ChatGPT.app-copy/Contents/MacOS/ChatGPT",
            app
        ));
    }

    #[test]
    fn argument_match_requires_token_boundaries() {
        assert!(command_has_argument(
            "/app/Codex --inspect-brk=127.0.0.1:1234 --flag",
            "--inspect-brk=127.0.0.1:1234"
        ));
        assert!(!command_has_argument(
            "/app/Codex --other=--inspect-brk=127.0.0.1:1234",
            "--inspect-brk=127.0.0.1:1234"
        ));
    }

    #[test]
    fn identity_check_rejects_a_reused_process_id() {
        let original =
            parse_unix_process_snapshot(b"100 1 100 Thu Jul 23 19:23:12 2026 /app/Codex\n");
        let identities = identities_for_process_ids(&original, &HashSet::from([100]));
        let reused = parse_unix_process_snapshot(b"100 1 100 Thu Jul 23 19:24:12 2026 unrelated\n");
        assert!(matching_process_ids(&reused, &identities).is_empty());
    }
}
