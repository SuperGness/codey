use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

const LOCK_DIRS: [&str; 2] = [
    "tmp/provider-sync.lock",
    "tmp/codey-session-index-cleanup.lock",
];
const UNKNOWN_OWNER_STALE_AFTER: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Deserialize)]
struct LockOwner {
    pid: u32,
}

/// Removes maintenance locks whose recorded owner no longer exists.
///
/// Both maintenance jobs use directory locks so a hard-killed Codey process
/// can leave a lock behind forever. Valid locks owned by a running process are
/// preserved. A lock with unreadable ownership metadata is only removed after
/// a conservative grace period, avoiding the create-dir/write-owner race.
pub fn recover_stale_locks(home: &Path) -> Result<Vec<PathBuf>> {
    let mut recovered = Vec::new();
    for relative in LOCK_DIRS {
        let path = home.join(relative);
        if recover_stale_lock(&path)? {
            recovered.push(path);
        }
    }
    Ok(recovered)
}

fn recover_stale_lock(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }

    let owner = fs::read(path.join("owner.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<LockOwner>(&bytes).ok());
    let stale = match owner {
        Some(owner) => !process_is_running(owner.pid),
        None => lock_age(path).is_some_and(|age| age >= UNKNOWN_OWNER_STALE_AFTER),
    };
    if !stale {
        return Ok(false);
    }

    fs::remove_dir_all(path).with_context(|| format!("清理陈旧维护锁失败：{}", path.display()))?;
    Ok(true)
}

fn lock_age(path: &Path) -> Option<Duration> {
    fs::metadata(path).ok()?.modified().ok()?.elapsed().ok()
}

#[cfg(unix)]
fn process_is_running(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid, 0) };
    if result == 0 {
        return true;
    }
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    )
}

#[cfg(windows)]
fn process_is_running(pid: u32) -> bool {
    pid != 0
        && codex_plus_core::windows_enumerate_processes()
            .into_iter()
            .any(|process| process.process_id == pid)
}

#[cfg(not(any(unix, windows)))]
fn process_is_running(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create_lock(home: &Path, relative: &str, pid: u32) -> PathBuf {
        let path = home.join(relative);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("owner.json"),
            serde_json::to_vec(&json!({"pid": pid, "startedAt": 1})).unwrap(),
        )
        .unwrap();
        path
    }

    #[test]
    fn removes_locks_owned_by_a_dead_process() {
        let temp = tempfile::tempdir().unwrap();
        let provider = create_lock(temp.path(), LOCK_DIRS[0], u32::MAX);
        let index = create_lock(temp.path(), LOCK_DIRS[1], u32::MAX);

        let recovered = recover_stale_locks(temp.path()).unwrap();

        assert_eq!(recovered, vec![provider.clone(), index.clone()]);
        assert!(!provider.exists());
        assert!(!index.exists());
    }

    #[test]
    fn preserves_a_lock_owned_by_the_current_process() {
        let temp = tempfile::tempdir().unwrap();
        let path = create_lock(temp.path(), LOCK_DIRS[0], std::process::id());

        let recovered = recover_stale_locks(temp.path()).unwrap();

        assert!(recovered.is_empty());
        assert!(path.exists());
    }

    #[test]
    fn preserves_a_recent_lock_without_owner_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(LOCK_DIRS[0]);
        fs::create_dir_all(&path).unwrap();

        let recovered = recover_stale_locks(temp.path()).unwrap();

        assert!(recovered.is_empty());
        assert!(path.exists());
    }
}
