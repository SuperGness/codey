//! Serialization and durable storage for the runtime subagent policy.
//!
//! Hook authorization decisions stay in the parent gate module; this module
//! owns only the policy and pending-update wire formats.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{
    RUNTIME_SUBAGENT_POLICY_FILE, RUNTIME_SUBAGENT_POLICY_PENDING_FILE,
    RUNTIME_SUBAGENT_POLICY_SCHEMA_VERSION, STATE_DIRECTORY,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RuntimeSubagentPolicy {
    pub(super) schema_version: u32,
    pub(super) roles: BTreeMap<String, crate::config::SubagentRoleConfig>,
    pub(super) runtime_agent_hashes: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeSubagentPolicyUpdate {
    schema_version: u32,
    target_policy_sha256: String,
}

pub(crate) fn runtime_subagent_policy_paths(home: &Path) -> (PathBuf, PathBuf) {
    let state_root = home.join(STATE_DIRECTORY);
    (
        state_root.join(RUNTIME_SUBAGENT_POLICY_FILE),
        state_root.join(RUNTIME_SUBAGENT_POLICY_PENDING_FILE),
    )
}

pub(crate) fn runtime_subagent_policy_bytes(
    roles: &BTreeMap<String, crate::config::SubagentRoleConfig>,
    runtime_agent_hashes: &BTreeMap<String, String>,
) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(&RuntimeSubagentPolicy {
        schema_version: RUNTIME_SUBAGENT_POLICY_SCHEMA_VERSION,
        roles: roles.clone(),
        runtime_agent_hashes: runtime_agent_hashes.clone(),
    })
    .context("序列化 Codey 子代理运行时策略失败")
}

pub(crate) fn begin_runtime_subagent_policy_update(
    home: &Path,
    roles: &BTreeMap<String, crate::config::SubagentRoleConfig>,
    runtime_agent_hashes: &BTreeMap<String, String>,
) -> Result<()> {
    let (_, pending_path) = runtime_subagent_policy_paths(home);
    let target = runtime_subagent_policy_bytes(roles, runtime_agent_hashes)?;
    let pending = serde_json::to_vec_pretty(&RuntimeSubagentPolicyUpdate {
        schema_version: RUNTIME_SUBAGENT_POLICY_SCHEMA_VERSION,
        target_policy_sha256: crate::fs_util::sha256_hex(&target),
    })
    .context("序列化 Codey 子代理运行时更新标记失败")?;
    crate::fs_util::atomic_write_private_with_parent(&pending_path, &pending).with_context(|| {
        format!(
            "写入 Codey 子代理运行时更新标记失败：{}",
            pending_path.display()
        )
    })
}

pub(crate) fn commit_runtime_subagent_policy(
    home: &Path,
    roles: &BTreeMap<String, crate::config::SubagentRoleConfig>,
    runtime_agent_hashes: &BTreeMap<String, String>,
) -> Result<()> {
    let (policy_path, pending_path) = runtime_subagent_policy_paths(home);
    let policy = runtime_subagent_policy_bytes(roles, runtime_agent_hashes)?;
    crate::fs_util::atomic_write_private_with_parent(&policy_path, &policy)
        .with_context(|| format!("写入 Codey 子代理运行时策略失败：{}", policy_path.display()))?;
    remove_optional_runtime_policy_file(&pending_path)
}

pub(crate) fn runtime_subagent_policy_matches(
    home: &Path,
    roles: &BTreeMap<String, crate::config::SubagentRoleConfig>,
    runtime_agent_hashes: &BTreeMap<String, String>,
) -> Result<bool> {
    let (policy_path, pending_path) = runtime_subagent_policy_paths(home);
    if read_optional_runtime_policy_file(&pending_path)?.is_some() {
        return Ok(false);
    }
    let Some(actual) = read_optional_runtime_policy_file(&policy_path)? else {
        return Ok(false);
    };
    Ok(actual == runtime_subagent_policy_bytes(roles, runtime_agent_hashes)?)
}

pub(crate) fn clear_runtime_subagent_policy(home: &Path) -> Result<()> {
    let (policy_path, pending_path) = runtime_subagent_policy_paths(home);
    remove_optional_runtime_policy_file(&pending_path)?;
    remove_optional_runtime_policy_file(&policy_path)
}

pub(super) fn read_optional_runtime_policy_file(path: &Path) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Codey 子代理运行时策略状态不是可信普通文件：{}",
        path.display()
    );
    fs::read(path)
        .map(Some)
        .with_context(|| format!("读取 Codey 子代理运行时策略状态失败：{}", path.display()))
}

fn remove_optional_runtime_policy_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("清理 Codey 子代理运行时策略状态失败：{}", path.display())),
    }
}
