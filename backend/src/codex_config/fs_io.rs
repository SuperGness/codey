use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use anyhow::{Context, Result};

pub(super) use crate::fs_util::atomic_write_private_with_parent as atomic_write;

pub(super) fn create_private_dir_all(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

pub(super) fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    // Atomic replace: a crash between truncate and write previously left a
    // half-written constraint template behind.
    crate::fs_util::atomic_write_private(path, bytes)
}

pub(super) fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("读取文件失败：{}", path.display())),
    }
}

pub(super) fn remove_optional(path: &Path) -> Result<()> {
    crate::fs_util::remove_file_if_exists(path)
        .with_context(|| format!("删除文件失败：{}", path.display()))
}
