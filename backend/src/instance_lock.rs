use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fs2::FileExt;

pub struct InstanceLock {
    file: File,
    path: PathBuf,
}

impl InstanceLock {
    pub fn acquire(config_path: &Path) -> Result<Self> {
        let directory = config_path.parent().context("Codey 配置路径缺少父目录")?;
        std::fs::create_dir_all(directory)
            .with_context(|| format!("创建 Codey 配置目录失败：{}", directory.display()))?;
        let path = directory.join("codey.instance.lock");
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .with_context(|| format!("打开 Codey 实例锁失败：{}", path.display()))?;
        if let Err(error) = file.try_lock_exclusive() {
            bail!("已有 Codey 实例正在使用同一配置目录：{error}");
        }
        Ok(Self { file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prevents_two_owners_of_the_same_config_directory() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.json");
        let first = InstanceLock::acquire(&config).unwrap();
        assert!(InstanceLock::acquire(&config).is_err());
        drop(first);
        assert!(InstanceLock::acquire(&config).is_ok());
    }
}
