//! Background sweep for session state left behind by finished sessions.
//!
//! Every Hook event creates its session directory and lock files, and those
//! lock files keep the directory from ever becoming empty, so finished
//! sessions accumulate. Directories idle past the threshold are removed only
//! while no Hook holds any of their locks. Lock files that older layouts kept
//! at the state root are retired the same way.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use fs2::FileExt;

use super::STATE_DIRECTORY;

const STALE_SESSION_STATE_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub(crate) fn spawn_stale_session_state_sweep(home: &Path) {
    let state_root = home.join(STATE_DIRECTORY);
    let spawned = std::thread::Builder::new()
        .name("codey-subagent-state-sweep".into())
        .spawn(move || {
            if let Err(error) =
                sweep_stale_session_state(&state_root, SystemTime::now(), STALE_SESSION_STATE_AGE)
            {
                eprintln!("清理过期的 Codey 子代理门禁状态失败：{error:#}");
            }
        });
    if let Err(error) = spawned {
        eprintln!("启动 Codey 子代理门禁状态清理失败：{error}");
    }
}

fn sweep_stale_session_state(
    state_root: &Path,
    now: SystemTime,
    max_idle: Duration,
) -> Result<usize> {
    let Some(cutoff) = now.checked_sub(max_idle) else {
        return Ok(0);
    };
    let entries = match fs::read_dir(state_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let path = entry.path();
        let swept = if kind.is_dir() && is_session_hash(name) {
            idle_since(&path, cutoff) && remove_unlocked_session_dir(&path)
        } else if kind.is_file() && is_retired_root_lock(name) {
            modified_before(&path, cutoff) && remove_unlocked_file(&path)
        } else {
            false
        };
        if swept {
            removed += 1;
        }
    }
    Ok(removed)
}

// Earlier layouts locked at the state root; the current gate only locks inside
// session directories, so nothing reopens these names.
fn is_retired_root_lock(name: &str) -> bool {
    matches!(
        name,
        "hook-state.lock" | "orchestrator-admission.lock" | "orchestrator-ledger-v1.lock"
    ) || name
        .strip_prefix("orchestrator-ledger-v1.lock.")
        .is_some_and(is_session_hash)
}

fn modified_before(path: &Path, cutoff: SystemTime) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| modified <= cutoff)
}

fn remove_unlocked_file(path: &Path) -> bool {
    let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else {
        return false;
    };
    if file.try_lock_exclusive().is_err() {
        return false;
    }
    let _ = FileExt::unlock(&file);
    drop(file);
    fs::remove_file(path).is_ok()
}

fn is_session_hash(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

// Lock files are reopened without being modified, so idleness is judged by the
// newest timestamp of the directory and every entry. Unreadable metadata keeps
// the directory.
fn idle_since(dir: &Path, cutoff: SystemTime) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    std::iter::once(fs::metadata(dir))
        .chain(entries.map(|entry| entry.and_then(|entry| entry.metadata())))
        .all(|metadata| {
            metadata
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| modified <= cutoff)
        })
}

fn remove_unlocked_session_dir(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    let mut locks = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            return false;
        }
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("lock") {
            let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
                return false;
            };
            if file.try_lock_exclusive().is_err() {
                return false;
            }
            locks.push((path, file));
        } else {
            files.push(path);
        }
    }
    if files.iter().any(|path| fs::remove_file(path).is_err()) {
        return false;
    }
    for (path, file) in locks {
        let _ = FileExt::unlock(&file);
        drop(file);
        let _ = fs::remove_file(path);
    }
    fs::remove_dir(dir).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagent_gate::state::{HookStateLock, session_state_dir};

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    #[test]
    fn sweeps_only_idle_unlocked_session_directories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let lock_only = session_state_dir(root, "lock-only");
        drop(HookStateLock::acquire(&lock_only).unwrap());
        let with_state = session_state_dir(root, "with-state");
        drop(HookStateLock::acquire(&with_state).unwrap());
        fs::write(with_state.join("orchestrator-ledger-v1.json"), b"{}").unwrap();
        fs::write(with_state.join("orchestrator-ledger-v1.lock"), b"").unwrap();
        let busy = session_state_dir(root, "busy");
        let held = HookStateLock::acquire(&busy).unwrap();
        fs::write(root.join("runtime-subagent-policy.json"), b"{}").unwrap();
        fs::write(root.join("subagent-traces-v1.lock"), b"").unwrap();
        let retired_lock = root.join(format!("orchestrator-ledger-v1.lock.{}", "a".repeat(64)));
        fs::write(&retired_lock, b"").unwrap();
        fs::create_dir(root.join("not-a-session")).unwrap();

        let now = SystemTime::now();
        assert_eq!(sweep_stale_session_state(root, now, 7 * DAY).unwrap(), 0);
        assert!(lock_only.exists() && with_state.exists());

        let later = now + 8 * DAY;
        assert_eq!(sweep_stale_session_state(root, later, 7 * DAY).unwrap(), 3);
        assert!(!lock_only.exists() && !with_state.exists() && !retired_lock.exists());
        assert!(root.join("subagent-traces-v1.lock").exists());
        assert!(busy.exists(), "a held Hook lock keeps its session");
        assert!(root.join("runtime-subagent-policy.json").exists());
        assert!(root.join("not-a-session").exists());

        drop(held);
        assert_eq!(sweep_stale_session_state(root, later, 7 * DAY).unwrap(), 1);
        assert!(!busy.exists());
        assert_eq!(
            sweep_stale_session_state(&root.join("missing"), later, 7 * DAY).unwrap(),
            0
        );
    }
}
