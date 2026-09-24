use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

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
    let cache = load(state_root).ok()?;
    let normalized = crate::subagent::rules::normalize_tool_name(tool_name);
    let key = cache_key(&normalized, tool_input);
    let entry = cache.entries.get(&key)?;
    (entry.tool_name == normalized
        && entry.tool_fingerprint == input_fingerprint(tool_input)
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
    let key = cache_key(&normalized, tool_input);
    let mut cache = load(state_root).unwrap_or_default();
    cache.schema_version = CACHE_SCHEMA_VERSION;
    cache.entries.insert(
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
    while cache.entries.len() > MAX_ENTRIES {
        let Some(oldest) = cache
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.last_verified_at_ms)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        cache.entries.remove(&oldest);
    }
    let bytes = serde_json::to_vec_pretty(&cache).context("序列化子代理工具能力缓存失败")?;
    crate::fs_util::atomic_write_private_with_parent(&cache_path(state_root), &bytes)
        .context("写入子代理工具能力缓存失败")
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

fn load(state_root: &Path) -> Result<CacheFile> {
    let path = cache_path(state_root);
    let bytes = match fs::read(&path) {
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

fn cache_key(tool_name: &str, tool_input: Option<&Value>) -> String {
    format!("{tool_name}:{}", input_fingerprint(tool_input))
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
}
