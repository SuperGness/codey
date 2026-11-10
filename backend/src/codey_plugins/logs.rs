use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

const LOG_NAMES: [&str; 2] = ["host.log", "plugin.log"];

struct LogLocks(Vec<File>);

impl Drop for LogLocks {
    fn drop(&mut self) {
        for lock in &self.0 {
            let _ = fs2::FileExt::unlock(lock);
        }
    }
}

fn directory(plugin_dir: &Path) -> Result<Option<PathBuf>, String> {
    let plugin_dir = super::checked_directory(plugin_dir)?;
    match fs::symlink_metadata(plugin_dir.join("logs")) {
        Ok(_) => super::checked_child_directory(&plugin_dir, "logs", false).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn file_size(path: &Path) -> Result<Option<u64>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            Ok(Some(metadata.len()))
        }
        Ok(_) => Err("日志文件必须是普通文件，不能是符号链接".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn size(plugin_dir: &Path) -> Result<u64, String> {
    let Some(directory) = directory(plugin_dir)? else {
        return Ok(0);
    };
    let mut bytes = 0;
    for name in LOG_NAMES {
        for name in [name.to_owned(), format!("{name}.1")] {
            bytes += file_size(&directory.join(name))?.unwrap_or(0);
        }
    }
    Ok(bytes)
}

pub(super) fn clear(plugin_dir: &Path) -> Result<(), String> {
    let Some(directory) = directory(plugin_dir)? else {
        return Ok(());
    };
    // 与 SDK 共享稳定的锁文件，先取得全部锁，避免写入忙时只清除部分日志。
    let mut locks = LogLocks(Vec::new());
    for name in LOG_NAMES {
        let path = directory.join(format!("{name}.lock"));
        file_size(&path)?;
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(|error| error.to_string())?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .map_err(|_| "日志正在写入或无法锁定，请稍后重试".to_string())?;
        locks.0.push(lock);
    }
    // 先校验并打开全部目标，再清空内容；保留文件和锁，后续写入仍使用原路径。
    let mut files: Vec<File> = Vec::new();
    for name in LOG_NAMES {
        for name in [name.to_owned(), format!("{name}.1")] {
            let path = directory.join(name);
            if file_size(&path)?.is_some() {
                files.push(
                    OpenOptions::new()
                        .write(true)
                        .open(path)
                        .map_err(|error| error.to_string())?,
                );
            }
        }
    }
    for file in files {
        file.set_len(0)
            .map_err(|error| format!("清除日志失败，部分日志可能已清除：{error}"))?;
    }
    Ok(())
}

#[cfg(all(test, feature = "unit-tests"))]
mod tests {
    use super::*;

    #[test]
    fn clears_current_and_rotated_logs_preserving_other_files_and_future_writes() {
        let root = tempfile::tempdir().unwrap();
        let logs = root.path().join("logs");
        fs::create_dir(&logs).unwrap();
        for name in ["host.log", "host.log.1", "plugin.log", "plugin.log.1"] {
            fs::write(logs.join(name), b"history").unwrap();
        }
        fs::write(root.path().join("config.json"), b"{}").unwrap();
        fs::write(logs.join("unrelated"), b"keep").unwrap();
        assert_eq!(size(root.path()).unwrap(), 28);
        clear(root.path()).unwrap();
        assert_eq!(size(root.path()).unwrap(), 0);
        assert_eq!(fs::read(root.path().join("config.json")).unwrap(), b"{}");
        assert_eq!(fs::read(logs.join("unrelated")).unwrap(), b"keep");
        assert!(logs.join("plugin.log.lock").exists());
        codey_plugin_sdk::append_log(&logs, "plugin.log", "new event").unwrap();
        assert!(size(root.path()).unwrap() > 0);
    }

    #[test]
    fn missing_logs_are_empty_and_clearing_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(size(root.path()).unwrap(), 0);
        clear(root.path()).unwrap();
        clear(root.path()).unwrap();
        assert!(!root.path().join("logs").exists());
    }

    #[test]
    fn busy_log_leaves_all_content_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let logs = root.path().join("logs");
        fs::create_dir(&logs).unwrap();
        fs::write(logs.join("host.log"), b"keep").unwrap();
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(logs.join("plugin.log.lock"))
            .unwrap();
        fs2::FileExt::lock_exclusive(&lock).unwrap();
        assert!(clear(root.path()).unwrap_err().contains("日志正在写入"));
        assert_eq!(fs::read(logs.join("host.log")).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_linked_directories_files_and_locks() {
        for name in ["logs", "logs/plugin.log", "logs/plugin.log.lock"] {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            fs::write(outside.path().join("saved"), b"keep").unwrap();
            let target = if name == "logs" {
                outside.path().to_path_buf()
            } else {
                fs::create_dir(root.path().join("logs")).unwrap();
                fs::write(root.path().join("logs/host.log"), b"history").unwrap();
                outside.path().join("saved")
            };
            std::os::unix::fs::symlink(target, root.path().join(name)).unwrap();
            assert!(clear(root.path()).is_err());
            assert_eq!(fs::read(outside.path().join("saved")).unwrap(), b"keep");
            if name != "logs" {
                assert_eq!(
                    fs::read(root.path().join("logs/host.log")).unwrap(),
                    b"history"
                );
            }
        }
    }
}
