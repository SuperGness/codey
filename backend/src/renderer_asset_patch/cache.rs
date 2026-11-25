//! 渲染资产补丁的磁盘与内存缓存。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{css_asset_name, log_service_event, numeric_asset_name};

/// 磁盘缓存上限，约可容纳三个 Codex 版本的补丁结果。
pub(super) const MAX_CACHE_BYTES: u64 = 96 << 20;
/// 缓存条目上限。数字 chunk 每个都会留一条「无需改写」记录，体积很小但数量
/// 随版本累积，单靠字节上限压不住目录规模。
pub(super) const MAX_CACHE_ENTRIES: usize = 12_000;
/// 「无需改写」记录不占体积，每写入这么多条才触发一次整理检查。
pub(super) const UNCHANGED_STORES_PER_PRUNE: usize = 256;
/// 自动整理最小间隔，避免冷启动批量写缓存时连续多次遍历上万个文件。
pub(super) const PRUNE_MIN_INTERVAL: Duration = Duration::from_secs(60);
/// 当前补丁版本的原子写临时文件（`.{tag}-*.tmp`）保留宽限期，避免并发整理误删正在落盘的临时文件。
pub(super) const IN_FLIGHT_TEMP_GRACE: Duration = Duration::from_secs(300);
/// 命中缓存时不频繁刷新 `.meta.json` mtime，间隔至少 1 小时再写入磁盘。
pub(super) const TOUCH_MIN_INTERVAL: Duration = Duration::from_secs(3600);
pub(super) const CACHE_DIR_NAME: &str = "renderer-patch-cache";

/// 补丁结果磁盘缓存。键为补丁脚本摘要加资产文件名，只缓存文件名带内容哈希的
/// 数字 JS chunk 与样式表；正文按标准 base64 落盘，元数据缺失或长度不符一律按未命中处理。
#[derive(Clone)]
pub(super) struct PatchCache {
    pub(super) dir: PathBuf,
    pub(super) tag: String,
    /// 距上次整理以来写入的「无需改写」记录数。
    unchanged_since_prune: Arc<AtomicUsize>,
    /// 本次运行已确认无需改写的缓存条目名，用于请求阶段免信号量与免磁盘直通。
    unchanged_entries: Arc<RwLock<HashSet<String>>>,
    /// 上次自动整理的时间，配合 `pruning` 防止冷启动重复全目录扫描。
    last_pruned: Arc<Mutex<Option<Instant>>>,
    pruning: Arc<AtomicBool>,
}

pub(super) enum CachedAsset {
    Patched(CachedPatch),
    Unchanged,
}

pub(super) struct CachedPatch {
    pub(super) status: i64,
    pub(super) headers: Vec<Value>,
    pub(super) body_base64: String,
}

pub(super) struct StoredPatch {
    pub(super) status: i64,
    pub(super) headers: Vec<Value>,
    pub(super) body_base64: String,
}

#[derive(Serialize, Deserialize)]
pub(super) struct CacheMeta {
    #[serde(default)]
    pub(super) unchanged: bool,
    #[serde(default)]
    pub(super) status: i64,
    #[serde(default)]
    pub(super) headers: Vec<Value>,
    #[serde(default)]
    pub(super) length: u64,
}

impl PatchCache {
    pub(super) fn new(dir: PathBuf, install_script: &str) -> Self {
        let digest = Sha256::digest(install_script.as_bytes());
        let tag = digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Self {
            dir,
            tag,
            // 首次写入即触发一次整理检查，清掉其他补丁版本留下的条目。
            unchanged_since_prune: Arc::new(AtomicUsize::new(UNCHANGED_STORES_PER_PRUNE - 1)),
            unchanged_entries: Arc::new(RwLock::new(HashSet::new())),
            last_pruned: Arc::new(Mutex::new(None)),
            pruning: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) fn entry_name(&self, url: &str) -> Option<String> {
        let name = url.split(['?', '#']).next()?.rsplit('/').next()?;
        cacheable_asset_name(name).then(|| format!("{}-{name}", self.tag))
    }

    pub(super) fn is_known_unchanged(&self, url: &str) -> bool {
        let Some(name) = self.entry_name(url) else {
            return false;
        };
        self.unchanged_entries
            .read()
            .expect("unchanged cache index poisoned")
            .contains(&name)
    }

    fn mark_unchanged(&self, name: &str, unchanged: bool) {
        let mut guard = self
            .unchanged_entries
            .write()
            .expect("unchanged cache index poisoned");
        if unchanged {
            guard.insert(name.to_string());
        } else {
            guard.remove(name);
        }
    }

    pub(super) async fn load(&self, url: &str) -> Option<CachedAsset> {
        let name = self.entry_name(url)?;
        let dir = self.dir.clone();
        let lookup_name = name.clone();
        let loaded = tokio::task::spawn_blocking(move || load_entry(&dir, &lookup_name))
            .await
            .ok()
            .flatten();
        if matches!(loaded, Some(CachedAsset::Unchanged)) {
            self.mark_unchanged(&name, true);
        }
        loaded
    }

    fn spawn_store(
        &self,
        url: &str,
        patch: Option<StoredPatch>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let name = self.entry_name(url)?;
        self.mark_unchanged(&name, patch.is_none());
        let cache = self.clone();
        let should_try_prune = patch.is_some()
            || (self.unchanged_since_prune.fetch_add(1, Ordering::Relaxed) + 1)
                .is_multiple_of(UNCHANGED_STORES_PER_PRUNE);
        Some(tokio::task::spawn_blocking(move || {
            if let Err(error) = store_entry(&cache.dir, &name, patch) {
                log_service_event(
                    "renderer_asset_patch.cache_write_failed",
                    json!({ "entry": name, "message": format!("{error:#}") }),
                );
            }
            if should_try_prune {
                cache.prune_if_due();
            }
        }))
    }

    /// 后台落盘缓存：立即更新内存索引，不阻塞请求放行或占用并发许可。
    pub(super) fn store_background(&self, url: &str, patch: Option<StoredPatch>) {
        let _ = self.spawn_store(url, patch);
    }

    #[cfg(test)]
    pub(super) async fn store(&self, url: &str, patch: Option<StoredPatch>) {
        if let Some(handle) = self.spawn_store(url, patch) {
            let _ = handle.await;
        }
    }

    /// 节流整理：同一时刻只允许一个整理任务，且距上次整理不满 `PRUNE_MIN_INTERVAL` 时跳过。
    pub(super) fn prune_if_due(&self) -> bool {
        let now = Instant::now();
        {
            let last = self.last_pruned.lock().expect("prune timestamp poisoned");
            if last.is_some_and(|prev| now.saturating_duration_since(prev) < PRUNE_MIN_INTERVAL) {
                return false;
            }
        }
        if self
            .pruning
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        struct ResetFlag<'a>(&'a AtomicBool);
        impl Drop for ResetFlag<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _reset = ResetFlag(&self.pruning);
        self.prune();
        *self.last_pruned.lock().expect("prune timestamp poisoned") = Some(Instant::now());
        true
    }

    pub(super) fn prune(&self) {
        self.prune_to(MAX_CACHE_BYTES, MAX_CACHE_ENTRIES);
    }

    /// 删掉其他补丁版本的条目，并按最近使用时间把总量和条目数压到上限以内。
    pub(super) fn prune_to(&self, max_bytes: u64, max_entries: usize) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let prefix = format!("{}-", self.tag);
        let temp_prefix = format!(".{prefix}");
        let mut current = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&temp_prefix) && name.ends_with(".tmp") {
                let keep = entry.metadata().map_or(true, |meta| {
                    meta.modified()
                        .ok()
                        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                        .is_none_or(|age| age < IN_FLIGHT_TEMP_GRACE)
                });
                if keep {
                    continue;
                }
            }
            if !name.starts_with(&prefix) {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            let Some(base) = name.strip_suffix(".meta.json") else {
                continue;
            };
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let body_size = std::fs::metadata(self.dir.join(base)).map_or(0, |body| body.len());
            let used = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            current.push((used, base.to_string(), meta.len() + body_size));
        }
        current.sort_by_key(|(used, _, _)| std::cmp::Reverse(*used));
        let mut total = 0;
        let mut evicted = Vec::new();
        for (index, (_, base, size)) in current.into_iter().enumerate() {
            total += size;
            if total > max_bytes || index >= max_entries {
                let _ = std::fs::remove_file(self.dir.join(format!("{base}.meta.json")));
                let _ = std::fs::remove_file(self.dir.join(&base));
                evicted.push(base);
            }
        }
        if !evicted.is_empty() {
            let mut guard = self
                .unchanged_entries
                .write()
                .expect("unchanged cache index poisoned");
            for base in evicted {
                guard.remove(&base);
            }
        }
    }
}

/// 仅缓存 rspack 带内容哈希的数字 JS chunk 与样式表。
pub(super) fn cacheable_asset_name(name: &str) -> bool {
    numeric_asset_name(name) || css_asset_name(name)
}

pub(super) fn load_entry(dir: &Path, name: &str) -> Option<CachedAsset> {
    let meta_path = dir.join(format!("{name}.meta.json"));
    let meta: CacheMeta = serde_json::from_slice(&std::fs::read(&meta_path).ok()?).ok()?;
    // 修改时间充当最近使用时间供 prune 淘汰；1 小时内已刷新的条目跳过写操作减少 I/O。
    let should_touch = std::fs::metadata(&meta_path)
        .and_then(|file_meta| file_meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_none_or(|age| age >= TOUCH_MIN_INTERVAL);
    if should_touch && let Ok(file) = std::fs::File::options().append(true).open(&meta_path) {
        let _ = file.set_modified(SystemTime::now());
    }
    if meta.unchanged {
        return Some(CachedAsset::Unchanged);
    }
    let body = std::fs::read(dir.join(name)).ok()?;
    if body.len() as u64 != meta.length {
        return None;
    }
    let body_base64 = String::from_utf8(body).ok()?;
    Some(CachedAsset::Patched(CachedPatch {
        status: meta.status,
        headers: meta.headers,
        body_base64,
    }))
}

pub(super) fn store_entry(dir: &Path, name: &str, patch: Option<StoredPatch>) -> Result<()> {
    std::fs::create_dir_all(dir).context("创建补丁缓存目录失败")?;
    let meta = match patch {
        Some(patch) => {
            write_atomically(&dir.join(name), patch.body_base64.as_bytes())?;
            CacheMeta {
                unchanged: false,
                status: patch.status,
                headers: patch.headers,
                length: patch.body_base64.len() as u64,
            }
        }
        None => CacheMeta {
            unchanged: true,
            status: 0,
            headers: Vec::new(),
            length: 0,
        },
    };
    write_atomically(
        &dir.join(format!("{name}.meta.json")),
        &serde_json::to_vec(&meta).context("补丁缓存元数据序列化失败")?,
    )
}

fn write_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    crate::fs_util::atomic_write(path, contents).map_err(anyhow::Error::msg)
}
