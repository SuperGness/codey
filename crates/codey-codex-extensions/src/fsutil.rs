use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

pub const MAX_FILE: u64 = 4 * 1024 * 1024;
pub const MAX_TOTAL: u64 = 32 * 1024 * 1024;
pub const MAX_FILES: usize = 2048;

pub const MAX_INVENTORY_BYTES: u64 = 64 * 1024 * 1024;

/// 仅在一次清单读取中复用文件内容；后续事务重新建立缓存。
#[derive(Default)]
pub struct ReadCache {
    files: BTreeMap<PathBuf, Option<Vec<u8>>>,
    bytes_read: u64,
    budget_exceeded: bool,
}
impl ReadCache {
    pub fn read(&mut self, path: &Path) -> Result<Option<&[u8]>> {
        self.ensure_complete()?;
        if !self.files.contains_key(path) {
            let remaining = MAX_INVENTORY_BYTES.saturating_sub(self.bytes_read);
            let bytes = read_limited(path, MAX_FILE.min(remaining))?;
            let size = bytes.as_ref().map_or(0, |bytes| bytes.len() as u64);
            if size > remaining {
                self.budget_exceeded = true;
                self.ensure_complete()?;
            }
            self.bytes_read += size;
            ensure!(size <= MAX_FILE, "文件超过大小限制");
            self.files.insert(path.to_path_buf(), bytes);
        }
        Ok(self.files.get(path).and_then(|value| value.as_deref()))
    }

    // 部分条目允许降级展示，但预算耗尽不得被这类错误恢复掩盖。
    pub fn ensure_complete(&self) -> Result<()> {
        ensure!(
            !self.budget_exceeded,
            "扩展清单读取总量超过 64 MiB，未生成完整清单；请减少当前范围或缓存中的资源后重试"
        );
        Ok(())
    }
}

pub fn updated_at(path: &Path) -> Option<String> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()
        .map(|time| chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339())
}

pub fn writable(path: &Path) -> bool {
    for parent in path.ancestors() {
        if let Ok(meta) = fs::metadata(parent) {
            if meta.permissions().readonly() {
                return false;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o222 == 0 {
                    return false;
                }
            }
            if parent != path || meta.is_dir() {
                return true;
            }
        }
    }
    false
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn safe_path(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "必须使用绝对路径");
    ensure!(
        !path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir)),
        "路径不能包含相对跳转"
    );
    // 只拒绝目标自身的符号链接：它才是跳转到预期范围之外的载体，而祖先目录
    // 带链接是系统常态（macOS 的 /tmp、/var、/etc 都是链接）。逐级拒绝会让
    // CODEX_HOME 落在这些路径下时整个模块不可用，而它并不增加任何防护。
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure!(
                !meta.file_type().is_symlink(),
                "拒绝符号链接目标: {}",
                path.display()
            );
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                ensure!(meta.file_attributes() & 0x400 == 0, "拒绝重解析点");
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

pub fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    let bytes = read_limited(path, MAX_FILE)?;
    ensure!(
        bytes
            .as_ref()
            .is_none_or(|bytes| bytes.len() as u64 <= MAX_FILE),
        "文件超过大小限制"
    );
    Ok(bytes)
}

// 多读一个字节以检测文件增长及预算超限，避免依赖读取前的元数据。
fn read_limited(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    safe_path(path)?;
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("读取失败: {}", path.display())),
    };
    ensure!(f.metadata()?.is_file(), "目标不是普通文件");
    let mut bytes = Vec::new();
    (&mut f).take(limit + 1).read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

pub fn private_dir(path: &Path) -> Result<()> {
    safe_path(path)?;
    if !path.exists() {
        fs::create_dir_all(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
    }
    ensure!(path.is_dir(), "目标不是目录");
    Ok(())
}

pub fn file_mode(path: &Path) -> Result<Option<u32>> {
    safe_path(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match fs::metadata(path) {
            Ok(meta) => Ok(Some(meta.permissions().mode() & 0o777)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    #[cfg(not(unix))]
    {
        Ok(None)
    }
}

fn atomic_write_mode(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<()> {
    safe_path(path)?;
    let parent = path.parent().context("目标没有父目录")?;
    private_dir(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(mode.unwrap_or(0o600) & 0o777))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

pub fn apply(path: &Path, data: &Option<Vec<u8>>, mode: Option<u32>) -> Result<()> {
    if let Some(bytes) = data {
        ensure!(
            bytes.len() as u64 <= MAX_FILE,
            "文件超过大小限制，未执行写入"
        );
        atomic_write_mode(path, bytes, mode)
    } else {
        safe_path(path)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

pub fn walk(root: &Path) -> Result<Vec<PathBuf>> {
    safe_path(root)?;
    if !root.exists() {
        return Ok(vec![]);
    }
    ensure!(root.is_dir(), "目标不是目录");
    let mut files = Vec::new();
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let mut total = 0u64;
    let mut entries = 0usize;
    while let Some((dir, depth)) = pending.pop() {
        ensure!(depth <= 20, "目录层级超过限制");
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            entries += 1;
            ensure!(entries <= MAX_FILES, "目录条目超过限制");
            let path = entry.path();
            safe_path(&path)?;
            let m = fs::symlink_metadata(&path)?;
            if m.is_dir() {
                pending.push((path, depth + 1));
            } else if m.is_file() {
                ensure!(m.len() <= MAX_FILE, "文件超过大小限制");
                total += m.len();
                ensure!(total <= MAX_TOTAL, "目录内容超过总大小限制");
                files.push(path);
            } else {
                bail!("目录包含特殊文件");
            }
        }
    }
    files.sort();
    Ok(files)
}
