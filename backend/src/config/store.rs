use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;

use super::CodeyConfig;

pub fn default_config_path() -> PathBuf {
    ProjectDirs::from("com", "Codey", "Codey")
        .map(|dirs| dirs.config_dir().join("config.json"))
        .unwrap_or_else(|| PathBuf::from(".codey").join("config.json"))
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

const CONFIG_BACKUP_COUNT: usize = 3;

impl Default for ConfigStore {
    fn default() -> Self {
        Self::new(default_config_path())
    }
}

impl ConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<CodeyConfig> {
        let primary = read_config_file(&self.path);
        if let Ok(config) = primary {
            return Ok(config);
        }
        let primary_missing = primary.as_ref().is_err_and(|error| {
            error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        });
        let primary_error = primary.unwrap_err();
        let mut found_backup = false;
        let mut backup_errors = Vec::new();
        for index in 1..=CONFIG_BACKUP_COUNT {
            let path = self.backup_path(index);
            match read_config_file(&path) {
                Ok(config) => return Ok(config),
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
                Err(error) => {
                    found_backup = true;
                    backup_errors.push(format!("{}：{error:#}", path.display()));
                }
            }
        }
        if primary_missing && !found_backup {
            return Ok(CodeyConfig::default());
        }
        let backup_summary = if backup_errors.is_empty() {
            "没有可用的配置备份".to_string()
        } else {
            format!("配置备份也无法读取：{}", backup_errors.join("；"))
        };
        Err(primary_error).context(backup_summary)
    }

    pub(crate) fn persist(&self, config: CodeyConfig) -> Result<CodeyConfig> {
        let config = config.normalize();
        let bytes = serde_json::to_vec_pretty(&config)?;
        self.rotate_backups_best_effort(&bytes);
        crate::fs_util::atomic_write_private_with_parent(&self.path, &bytes)
            .with_context(|| format!("替换 Codey 配置失败：{}", self.path.display()))?;
        Ok(config)
    }

    #[cfg(test)]
    pub fn save(&self, config: &CodeyConfig) -> Result<()> {
        self.persist(config.clone()).map(|_| ())
    }

    /// Copies the config and backups that failed to load out of the backup
    /// rotation, so later saves of the fallback defaults cannot push the
    /// user's original out of reach. Copies are named after their content, so
    /// relaunching with the same broken files adds nothing.
    pub fn preserve_unreadable(&self) -> Vec<PathBuf> {
        use sha2::{Digest, Sha256};

        std::iter::once(self.path.clone())
            .chain((1..=CONFIG_BACKUP_COUNT).map(|index| self.backup_path(index)))
            .filter_map(|path| {
                let bytes = fs::read(&path).ok()?;
                let digest = format!("{:x}", Sha256::digest(&bytes));
                let file_name = path.file_name()?.to_string_lossy().into_owned();
                let target =
                    path.with_file_name(format!("{file_name}.unreadable-{}", &digest[..12]));
                if !target.exists()
                    && let Err(error) =
                        crate::fs_util::atomic_write_private_with_parent(&target, &bytes)
                {
                    crate::error_log::record_failure(
                        "config_preserve_failed",
                        "preserve_unreadable_codey_config",
                        format!("{error:#}"),
                        serde_json::json!({ "from": path.display().to_string() }),
                    );
                    return None;
                }
                Some(target)
            })
            .collect()
    }

    pub(super) fn backup_path(&self, index: usize) -> PathBuf {
        let file_name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config.json".to_string());
        self.path.with_file_name(format!("{file_name}.bak.{index}"))
    }

    /// Shift the current file into the backup chain with renames. Renames keep
    /// the private file mode and avoid re-parsing every backup on each save.
    /// Backups are best-effort: a failure here must never block persisting the
    /// user's settings, so errors are logged instead of returned.
    fn rotate_backups_best_effort(&self, next_bytes: &[u8]) {
        let Ok(current) = fs::read(&self.path) else {
            return;
        };
        // Saving identical content again would only push duplicates through
        // the chain and evict an older distinct snapshot.
        if current == next_bytes {
            return;
        }
        if fs::read(self.backup_path(1)).is_ok_and(|newest| newest == current) {
            return;
        }
        for index in (1..CONFIG_BACKUP_COUNT).rev() {
            let from = self.backup_path(index);
            if !from.exists() {
                continue;
            }
            if let Err(error) = fs::rename(&from, self.backup_path(index + 1)) {
                crate::error_log::record_failure(
                    "config_backup_rotate_failed",
                    "rotate_codey_config_backups",
                    // 去重只看错误文本，带上路径才不会吞掉其他备份的同类错误。
                    format!("{}: {error:#}", from.display()),
                    serde_json::json!({ "from": from.display().to_string() }),
                );
            }
        }
        if let Err(error) = fs::rename(&self.path, self.backup_path(1)) {
            crate::error_log::record_failure(
                "config_backup_rotate_failed",
                "rotate_codey_config_backups",
                format!("{error:#}"),
                serde_json::json!({ "from": self.path.display().to_string() }),
            );
        }
    }
}

pub(super) fn read_config_file(path: &Path) -> Result<CodeyConfig> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("读取 Codey 配置失败：{}", path.display()))?;
    parse_config_contents(&contents, path)
}

pub(super) fn parse_config_contents(contents: &str, path: &Path) -> Result<CodeyConfig> {
    // `normalize` marks non-empty legacy configs as imported, so the previous
    // explicit marker probe (a second full JSON parse) was redundant.
    let config = serde_json::from_str::<CodeyConfig>(contents)
        .with_context(|| format!("解析 Codey 配置失败：{}", path.display()))?;
    Ok(config.normalize())
}
