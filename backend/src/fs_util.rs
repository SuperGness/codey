use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

pub(crate) fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// 小写十六进制 SHA-256。此前 subagent_gate、subagent_orchestrator、
/// session_index_cleanup 各有一份逐字节相同的实现。
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
