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

/// 仅探测进程是否存活（`kill(pid, 0)`）。EPERM 说明进程存在但无权限，
/// 同样视为存活。
#[cfg(unix)]
pub fn unix_process_alive(process_id: u32) -> bool {
    let Ok(process_id) = libc::pid_t::try_from(process_id) else {
        return false;
    };
    let result = unsafe { libc::kill(process_id, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
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

pub(crate) struct OwnedChild {
    child: std::process::Child,
    #[cfg(windows)]
    job: windows::Win32::Foundation::HANDLE,
}

impl OwnedChild {
    pub(crate) fn spawn(command: &mut std::process::Command) -> anyhow::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            use windows::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
            command.creation_flags(CREATE_NO_WINDOW.0 | CREATE_SUSPENDED.0);
        }
        let process = Self {
            child: command.spawn()?,
            #[cfg(windows)]
            job: windows::Win32::Foundation::HANDLE::default(),
        };
        #[cfg(windows)]
        let process = {
            let mut process = process;
            process.attach_job()?;
            process.resume()?;
            process
        };
        Ok(process)
    }

    #[cfg(windows)]
    fn attach_job(&mut self) -> anyhow::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::HANDLE,
            System::JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            },
        };
        unsafe {
            self.job = CreateJobObjectW(None, None)?;
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                self.job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            )?;
            AssignProcessToJobObject(self.job, HANDLE(self.child.as_raw_handle()))?;
        }
        Ok(())
    }

    #[cfg(windows)]
    fn resume(&self) -> anyhow::Result<()> {
        use windows::Win32::{
            Foundation::CloseHandle,
            System::{
                Diagnostics::ToolHelp::{
                    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First,
                    Thread32Next,
                },
                Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
            },
        };
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)?;
            let mut entry = THREADENTRY32 {
                dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
                ..Default::default()
            };
            let mut next = Thread32First(snapshot, &mut entry).is_ok();
            let mut thread_id = None;
            while next {
                if entry.th32OwnerProcessID == self.child.id() {
                    thread_id = Some(entry.th32ThreadID);
                    break;
                }
                next = Thread32Next(snapshot, &mut entry).is_ok();
            }
            let _ = CloseHandle(snapshot);
            let thread_id = thread_id.ok_or_else(|| anyhow::anyhow!("无法找到暂停的子进程线程"))?;
            let thread = OpenThread(THREAD_SUSPEND_RESUME, false, thread_id)?;
            let resumed = ResumeThread(thread);
            let _ = CloseHandle(thread);
            anyhow::ensure!(resumed != u32::MAX, "无法恢复子进程");
        }
        Ok(())
    }
}

impl std::ops::Deref for OwnedChild {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.child
    }
}

impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.child
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        // 只在子进程仍运行时回收整个进程组；已退出并被回收的 pid 可能被复用为
        // 无关进程组的组号，此时发信号会误杀其他进程。
        #[cfg(unix)]
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = signal_process_group(Some(self.child.id()), libc::SIGKILL);
        }
        #[cfg(windows)]
        if !self.job.is_invalid() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(self.job);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod owned_tests {
    use super::OwnedChild;
    use std::{
        fs,
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    #[test]
    #[ignore = "仅由进程树回收测试启动"]
    fn owned_process_child() {
        let Some(marker) = std::env::var_os("CODEY_PROCESS_TEST_MARKER") else {
            return;
        };
        if std::env::var_os("CODEY_PROCESS_TEST_DESCENDANT").is_none() {
            let mut child = child_command(&marker)
                .env("CODEY_PROCESS_TEST_DESCENDANT", "1")
                .spawn()
                .unwrap();
            let _ = child.wait();
        } else {
            loop {
                fs::write(&marker, b"running").unwrap();
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn child_command(marker: &std::ffi::OsStr) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "process_tree::owned_tests::owned_process_child",
                "--ignored",
                "--nocapture",
            ])
            .env("CODEY_PROCESS_TEST_MARKER", marker)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    #[test]
    fn dropping_owned_child_terminates_descendants() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("heartbeat");
        let process = OwnedChild::spawn(&mut child_command(marker.as_os_str())).unwrap();
        let start = Instant::now();
        while !marker.exists() && start.elapsed() < Duration::from_secs(10) {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(marker.exists());
        drop(process);
        thread::sleep(Duration::from_millis(50));
        let modified = fs::metadata(&marker).unwrap().modified().unwrap();
        thread::sleep(Duration::from_millis(100));
        assert_eq!(fs::metadata(&marker).unwrap().modified().unwrap(), modified);
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
