use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const CACHE_FILE: &str = "tool-capability-cache-v1.json";
const CACHE_SCHEMA_VERSION: u32 = 1;
const CAPABILITY_VERSION: u32 = 1;
const MAX_ENTRIES: usize = 512;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CacheEntry {
    tool_name: String,
    tool_fingerprint: String,
    policy_revision: u64,
    capability_version: u32,
    capability: String,
    last_verified_at_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CacheFile {
    schema_version: u32,
    entries: BTreeMap<String, CacheEntry>,
}

pub(crate) fn cached_read_only_class(
    state_root: &Path,
    tool_name: &str,
    tool_input: Option<&Value>,
    policy_revision: u64,
) -> Option<()> {
    let normalized = crate::subagent::rules::normalize_tool_name(tool_name);
    let fingerprint = input_fingerprint(tool_input);
    let key = format!("{normalized}:{fingerprint}");
    let guard = load_guard(state_root);
    let entry = guard.as_ref()?.file.entries.get(&key)?;
    (entry.tool_name == normalized
        && entry.tool_fingerprint == fingerprint
        && entry.policy_revision == policy_revision
        && entry.capability_version == CAPABILITY_VERSION
        && entry.capability == "read_only")
        .then_some(())
}

pub(crate) fn record_read_only(
    state_root: &Path,
    tool_name: &str,
    tool_input: Option<&Value>,
    policy_revision: u64,
    now_ms: u64,
) -> Result<()> {
    let normalized = crate::subagent::rules::normalize_tool_name(tool_name);
    let fingerprint = input_fingerprint(tool_input);
    let key = format!("{normalized}:{fingerprint}");
    let mut guard = load_guard(state_root);
    let cached = guard.as_mut().expect("工具能力缓存已装入");
    if let Some(entry) = cached.file.entries.get_mut(&key)
        && entry.tool_name == normalized
        && entry.tool_fingerprint == fingerprint
        && entry.policy_revision == policy_revision
        && entry.capability_version == CAPABILITY_VERSION
        && entry.capability == "read_only"
    {
        // 分类没变时只刷新内存里的最近使用时间，避免每次工具调用都重写整份缓存。
        entry.last_verified_at_ms = now_ms;
        return Ok(());
    }
    cached.file.schema_version = CACHE_SCHEMA_VERSION;
    cached.file.entries.insert(
        key,
        CacheEntry {
            tool_name: normalized,
            tool_fingerprint: fingerprint,
            policy_revision,
            capability_version: CAPABILITY_VERSION,
            capability: "read_only".to_string(),
            last_verified_at_ms: now_ms,
        },
    );
    while cached.file.entries.len() > MAX_ENTRIES {
        let Some(oldest) = cached
            .file
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.last_verified_at_ms)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        cached.file.entries.remove(&oldest);
    }
    let bytes = serde_json::to_vec_pretty(&cached.file).context("序列化子代理工具能力缓存失败")?;
    let path = cached.path.clone();
    crate::fs_util::atomic_write_private_with_parent(&path, &bytes)
        .context("写入子代理工具能力缓存失败")?;
    cached.identity = file_identity(&path).unwrap_or(FileIdentity {
        exists: true,
        modified: None,
        len: bytes.len() as u64,
    });
    Ok(())
}

pub(crate) fn looks_read_only(tool_name: &str) -> bool {
    let normalized = crate::subagent::rules::normalize_tool_name(tool_name);
    let read_marker = [
        "read", "list", "get", "search", "find", "inspect", "query", "status", "describe",
        "lookup", "metadata", "discover", "fetch", "view", "open",
    ];
    let write_marker = [
        "write", "create", "delete", "remove", "update", "edit", "apply", "patch", "send",
        "execute", "run", "command", "publish", "upload", "install",
    ];
    read_marker.iter().any(|marker| normalized.contains(marker))
        && !write_marker
            .iter()
            .any(|marker| normalized.contains(marker))
}

fn cache_path(state_root: &Path) -> PathBuf {
    state_root.join(CACHE_FILE)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    exists: bool,
    modified: Option<SystemTime>,
    len: u64,
}

struct MemoryCache {
    path: PathBuf,
    identity: FileIdentity,
    file: CacheFile,
}

fn memory_cache() -> &'static Mutex<Option<MemoryCache>> {
    static MEMORY: Mutex<Option<MemoryCache>> = Mutex::new(None);
    &MEMORY
}

fn lock_memory() -> std::sync::MutexGuard<'static, Option<MemoryCache>> {
    memory_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn file_identity(path: &Path) -> std::io::Result<FileIdentity> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(FileIdentity {
            exists: true,
            modified: metadata.modified().ok(),
            len: metadata.len(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(FileIdentity {
            exists: false,
            modified: None,
            len: 0,
        }),
        Err(error) => Err(error),
    }
}

fn read_cache_file(path: &Path) -> Result<CacheFile> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CacheFile::default());
        }
        Err(error) => return Err(error.into()),
    };
    let cache: CacheFile = serde_json::from_slice(&bytes)
        .with_context(|| format!("解析子代理工具能力缓存失败：{}", path.display()))?;
    anyhow::ensure!(
        cache.schema_version == CACHE_SCHEMA_VERSION,
        "子代理工具能力缓存版本不受支持：{}",
        cache.schema_version
    );
    Ok(cache)
}

/// 同一进程内按路径和文件标识复用已解析的缓存。磁盘仍是跨进程的事实来源：
/// 标识变化时重新读取。
fn load_guard(state_root: &Path) -> std::sync::MutexGuard<'static, Option<MemoryCache>> {
    let path = cache_path(state_root);
    let identity = file_identity(&path).unwrap_or(FileIdentity {
        exists: false,
        modified: None,
        len: 0,
    });
    {
        let guard = lock_memory();
        if guard
            .as_ref()
            .is_some_and(|cached| cached.path == path && cached.identity == identity)
        {
            return guard;
        }
    }
    let file = read_cache_file(&path).unwrap_or_default();
    let identity = file_identity(&path).unwrap_or(identity);
    let mut guard = lock_memory();
    *guard = Some(MemoryCache {
        path,
        identity,
        file,
    });
    guard
}

fn input_fingerprint(tool_input: Option<&Value>) -> String {
    let shape = tool_input.map(value_shape).unwrap_or(Value::Null);
    crate::fs_util::sha256_hex(serde_json::to_string(&shape).unwrap_or_default().as_bytes())
}

fn value_shape(value: &Value) -> Value {
    match value {
        Value::Null => Value::String("null".into()),
        Value::Bool(_) => Value::String("bool".into()),
        Value::Number(_) => Value::String("number".into()),
        Value::String(_) => Value::String("string".into()),
        Value::Array(values) => Value::Array(values.iter().map(value_shape).collect()),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), value_shape(value)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_only_name_heuristic_rejects_side_effect_markers() {
        assert!(looks_read_only("mcp__docs__list_pages"));
        assert!(looks_read_only("resource_inspect"));
        assert!(!looks_read_only("mcp__docs__update_page"));
        assert!(!looks_read_only("command_run"));
    }

    #[test]
    fn cache_round_trip_is_keyed_by_input_shape_and_policy() {
        let root = tempfile::tempdir().unwrap();
        let input = json!({"query": "secret"});
        record_read_only(root.path(), "mcp__docs__search", Some(&input), 7, 10).unwrap();
        assert!(
            cached_read_only_class(
                root.path(),
                "mcp__docs__search",
                Some(&json!({"query": "other"})),
                7
            )
            .is_some()
        );
        assert!(
            cached_read_only_class(
                root.path(),
                "mcp__docs__search",
                Some(&json!({"limit": 1})),
                7
            )
            .is_none()
        );
        assert!(
            cached_read_only_class(root.path(), "mcp__docs__search", Some(&input), 8).is_none()
        );
    }

    #[test]
    fn repeat_classification_keeps_the_cache_file_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let input = json!({"query": "secret"});
        record_read_only(root.path(), "mcp__docs__search", Some(&input), 7, 10).unwrap();
        let path = root.path().join(CACHE_FILE);
        let before = fs::read(&path).unwrap();
        record_read_only(root.path(), "mcp__docs__search", Some(&input), 7, 99).unwrap();
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(
            cached_read_only_class(root.path(), "mcp__docs__search", Some(&input), 7).is_some()
        );
    }

    #[test]
    fn external_cache_rewrite_invalidates_the_memory_copy() {
        let root = tempfile::tempdir().unwrap();
        let input = json!({"query": "secret"});
        record_read_only(root.path(), "mcp__docs__search", Some(&input), 7, 10).unwrap();
        let path = root.path().join(CACHE_FILE);
        fs::write(&path, br#"{"schemaVersion":1,"entries":{}}"#).unwrap();
        assert!(
            cached_read_only_class(root.path(), "mcp__docs__search", Some(&input), 7).is_none()
        );
    }

    #[test]
    fn policy_change_replaces_the_cached_classification() {
        let root = tempfile::tempdir().unwrap();
        let input = json!({"query": "secret"});
        record_read_only(root.path(), "mcp__docs__search", Some(&input), 7, 10).unwrap();
        record_read_only(root.path(), "mcp__docs__search", Some(&input), 8, 11).unwrap();
        assert!(
            cached_read_only_class(root.path(), "mcp__docs__search", Some(&input), 7).is_none()
        );
        assert!(
            cached_read_only_class(root.path(), "mcp__docs__search", Some(&input), 8).is_some()
        );
    }
}
