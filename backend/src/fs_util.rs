use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use sha2::{Digest, Sha256};

pub(crate) fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub(crate) fn timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 删除文件；文件本就不存在视为成功。
pub(crate) fn remove_file_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// `try_lock_*` 失败是否表示锁被其他进程占用（Windows 的 LockFileEx 以
/// ERROR_LOCK_VIOLATION 报告，fs2 已按平台封装）。
pub(crate) fn file_lock_is_contended(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        || error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
}

/// 在超时范围内重试独占锁，最后一次等待不超过剩余时间。
pub(crate) fn acquire_exclusive_lock_with_timeout(
    file: &File,
    _lock_path: &Path,
    timeout: Duration,
    retry: Duration,
    timeout_msg: impl Fn() -> String,
    lock_err_msg: impl Fn() -> String,
) -> anyhow::Result<()> {
    let started = Instant::now();
    loop {
        match fs2::FileExt::try_lock_exclusive(file) {
            Ok(()) => return Ok(()),
            Err(error) if file_lock_is_contended(&error) => {
                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    anyhow::bail!(timeout_msg());
                }
                std::thread::sleep(retry.min(timeout.saturating_sub(elapsed)));
            }
            Err(error) => return Err(error).with_context(&lock_err_msg),
        }
    }
}

/// 加锁后确认路径仍指向被锁的那个文件。空闲状态清理会删除锁文件；若删除发生在
/// 本进程打开之后、拿到锁之前，锁住的是已脱离目录的旧文件，其他进程会在新建的
/// 同名文件上同时持锁。返回 false 时调用方应丢弃句柄并重新打开。
pub(crate) fn locked_file_matches_path(file: &File, path: &Path) -> bool {
    let Ok(current) = fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        file.metadata()
            .is_ok_and(|locked| locked.dev() == current.dev() && locked.ino() == current.ino())
    }
    #[cfg(not(unix))]
    {
        // Windows 上已删除但仍被打开的文件处于删除挂起状态，按路径取元数据会失败，
        // 上面的检查已覆盖该情况。
        let _ = (file, current);
        true
    }
}

/// 以毫秒时间戳命名的唯一子目录（不创建）。同一毫秒内的重复调用追加 `-n` 后缀。
/// 此前 session_index_cleanup 与 codex_config 各自实现了一份备份目录命名。
pub(crate) fn unique_timestamp_dir(root: &Path) -> PathBuf {
    let base = timestamp_millis().to_string();
    let mut path = root.join(&base);
    let mut suffix = 0usize;
    while path.exists() {
        suffix += 1;
        path = root.join(format!("{base}-{suffix}"));
    }
    path
}

/// 保留 `keep` 个最新的受管目录，删除其余。`sort_key` 返回 `None` 的条目不受管，
/// 不参与计数也不会被删除；`Some(key)` 越大表示越新。删除失败静默忽略。
pub(crate) fn prune_dirs(root: &Path, keep: usize, sort_key: impl Fn(&Path) -> Option<u128>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut managed = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if !entry.file_type().ok()?.is_dir() {
                return None;
            }
            let path = entry.path();
            let key = sort_key(&path)?;
            Some((key, path))
        })
        .collect::<Vec<_>>();
    if managed.len() <= keep {
        return;
    }
    managed.sort_by_key(|(key, _)| std::cmp::Reverse(*key));
    for (_, path) in managed.drain(keep..) {
        let _ = fs::remove_dir_all(&path);
    }
}

/// 小写十六进制 SHA-256。此前 subagent_gate、subagent_orchestrator、
/// session_index_cleanup 各有一份逐字节相同的实现。
pub(crate) fn sha256_hex_str(value: &str) -> String {
    sha256_hex(value.as_bytes())
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// 与目标同目录的一次性临时文件名。随机 UUID 保证同一进程、同一毫秒内的
/// 并发写者也不会覆盖对方的临时文件。
pub(crate) fn unique_temp_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "codey".to_string());
    parent.join(format!(".{file_name}.codey-{}.tmp", uuid::Uuid::new_v4()))
}

/// 把写好的同目录临时文件替换到目标位置。Windows 文件占用导致替换失败时，
/// 必须保留原目标；不能先删除目标再重试，否则重试失败会让两份文件同时丢失。
/// 失败路径只清理本次写入的临时文件。
pub(crate) fn persist_temp_file(temp: &Path, destination: &Path) -> std::io::Result<()> {
    match fs::rename(temp, destination) {
        Ok(()) => {
            if let Some(parent) = destination.parent() {
                // Directory syncing is unsupported on some filesystems. The
                // file itself is already durable; this best-effort sync closes
                // the rename durability gap where the platform supports it.
                let _ = File::open(parent).and_then(|directory| directory.sync_all());
            }
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(temp);
            Err(error)
        }
    }
}

fn atomic_write_with(
    destination: &Path,
    write_temp: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let temp = unique_temp_path(destination);
    if let Err(error) = write_temp(&temp) {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    persist_temp_file(&temp, destination)
}

pub(crate) fn atomic_write(destination: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    atomic_write_with(destination, |temp| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(temp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })?;
    Ok(())
}

pub(crate) fn atomic_write_private(destination: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    atomic_write_with(destination, |temp| {
        #[cfg(unix)]
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(temp)?;
            file.write_all(bytes)?;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            let mut file = OpenOptions::new().write(true).create_new(true).open(temp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        Ok(())
    })?;
    Ok(())
}

pub(crate) fn atomic_write_private_with_parent(
    destination: &Path,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", destination.display()))?;
    fs::create_dir_all(parent)?;
    atomic_write_private(destination, bytes)
}

pub(crate) fn atomic_write_preserving_permissions(
    destination: &Path,
    bytes: &[u8],
) -> anyhow::Result<()> {
    atomic_write_with(destination, |temp| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(temp)?;
        file.write_all(bytes)?;
        if let Ok(metadata) = fs::metadata(destination) {
            fs::set_permissions(temp, metadata.permissions())?;
        }
        file.sync_all()
    })?;
    Ok(())
}

pub(crate) fn read_bounded(path: &Path, max_bytes: u64) -> anyhow::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "不是可信普通文件：{}",
        path.display()
    );
    anyhow::ensure!(
        metadata.len() <= max_bytes,
        "文件超过 {} 字节上限：{}",
        max_bytes,
        path.display()
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)?
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= max_bytes,
        "文件读取期间超过 {} 字节上限：{}",
        max_bytes,
        path.display()
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_file_must_still_be_the_file_at_its_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hook-state.lock");
        let opened = File::create(&path).unwrap();
        assert!(locked_file_matches_path(&opened, &path));
        // A sweep removed the lock after it was opened: the handle is orphaned.
        fs::remove_file(&path).unwrap();
        assert!(!locked_file_matches_path(&opened, &path));
        // Another process recreated the name; it is still not our file.
        let recreated = File::create(&path).unwrap();
        #[cfg(unix)]
        assert!(!locked_file_matches_path(&opened, &path));
        assert!(locked_file_matches_path(&recreated, &path));
    }

    #[test]
    fn exclusive_lock_times_out_then_succeeds_after_release() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.lock");
        let first = File::create(&path).unwrap();
        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&first).unwrap();

        let acquire = || {
            acquire_exclusive_lock_with_timeout(
                &second,
                &path,
                Duration::from_millis(5),
                Duration::from_millis(1),
                || "lock timeout".to_string(),
                || "lock failed".to_string(),
            )
        };
        assert_eq!(acquire().unwrap_err().to_string(), "lock timeout");
        fs2::FileExt::unlock(&first).unwrap();
        acquire().unwrap();
        fs2::FileExt::unlock(&second).unwrap();
    }

    #[test]
    fn temp_paths_are_unique_for_the_same_destination() {
        let destination = Path::new("state/config.toml");
        let first = unique_temp_path(destination);
        let second = unique_temp_path(destination);

        assert_ne!(first, second);
        assert_eq!(first.parent(), destination.parent());
        assert_eq!(second.parent(), destination.parent());
    }

    #[test]
    fn persisted_temp_file_replaces_destination_and_removes_temp() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("state.json");
        let temp = unique_temp_path(&destination);
        fs::write(&destination, b"old").unwrap();
        fs::write(&temp, b"new").unwrap();

        persist_temp_file(&temp, &destination).unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"new");
        assert!(!temp.exists());
    }

    #[test]
    fn failed_persist_removes_the_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let temp = directory.path().join("state.tmp");
        let destination = directory.path().join("missing").join("state.json");
        fs::write(&temp, b"temporary").unwrap();

        let error = persist_temp_file(&temp, &destination).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(!temp.exists());
        assert!(!destination.exists());
    }

    #[test]
    fn failed_persist_preserves_existing_destination_when_temp_is_missing() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("session_index.jsonl");
        let temp = unique_temp_path(&destination);
        fs::write(&destination, b"history-one\nhistory-two\n").unwrap();

        assert!(persist_temp_file(&temp, &destination).is_err());

        assert_eq!(
            fs::read(&destination).unwrap(),
            b"history-one\nhistory-two\n"
        );
    }

    #[cfg(windows)]
    #[test]
    fn locked_temp_never_removes_existing_destination() {
        use std::os::windows::fs::OpenOptionsExt;

        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("session_index.jsonl");
        let temp = unique_temp_path(&destination);
        fs::write(&destination, b"original history").unwrap();
        fs::write(&temp, b"updated history").unwrap();
        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&temp)
            .unwrap();

        assert!(persist_temp_file(&temp, &destination).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"original history");
        drop(locked);
    }

    #[test]
    fn atomic_write_replaces_destination() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("state.json");
        fs::write(&destination, b"old").unwrap();

        atomic_write(&destination, b"new").unwrap();

        assert_eq!(fs::read(destination).unwrap(), b"new");
    }

    #[test]
    fn bounded_read_rejects_oversized_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        fs::write(&path, b"12345").unwrap();

        assert_eq!(read_bounded(&path, 5).unwrap(), b"12345");
        assert!(read_bounded(&path, 4).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn private_atomic_write_uses_private_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("secret.json");

        atomic_write_private(&destination, b"secret").unwrap();

        let mode = fs::metadata(destination).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_can_preserve_existing_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("state.json");
        fs::write(&destination, b"old").unwrap();
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o640)).unwrap();

        atomic_write_preserving_permissions(&destination, b"new").unwrap();

        let mode = fs::metadata(destination).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
    }
}
