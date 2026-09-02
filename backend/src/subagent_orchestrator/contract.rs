//! Native `agents.spawn_agent` task-capsule validation.
//!
//! Resource paths are conservative coordination metadata. Codex remains the
//! filesystem authority.

use std::fs;
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::subagent::api::TraceContext;
use crate::subagent::rules::{RoleAccess, RolePolicy, RuleSet};

use super::identity::consistent_string_field;

#[derive(Clone, Debug)]
pub(super) struct TaskCapsule {
    pub(super) id: String,
}

#[derive(Clone, Debug)]
pub(super) struct PreparedContract {
    pub(super) capsule: TaskCapsule,
    pub(super) role: String,
    pub(super) policy: RolePolicy,
    pub(super) workspace_root: Option<String>,
    pub(super) trace: TraceContext,
    pub(super) capabilities: Vec<String>,
}

pub(super) fn prepare_task_capsule(
    tool_input: Option<&Value>,
    hook_workspace_root: Option<&str>,
    rule_set: &RuleSet,
) -> std::result::Result<PreparedContract, String> {
    let input = tool_input
        .and_then(Value::as_object)
        .ok_or_else(|| contract_error("spawn 输入不是 JSON object"))?;
    let task_name = spawn_field(input, &["task_name", "taskName"], "task_name")?
        .ok_or_else(|| contract_error("缺少 task_name"))?;
    let role = spawn_field(
        input,
        &["agent_type", "agentType", "agent_role", "agentRole"],
        "agent_type",
    )?
    .unwrap_or(crate::config::SUBAGENT_ROLE_DEFAULT);
    let message = spawn_field(input, &["message", "prompt"], "message")?
        .ok_or_else(|| contract_error("缺少 message"))?;
    if message.trim().is_empty() {
        return Err(contract_error("message 为空"));
    }
    validate_task_id(task_name)?;
    let policy = rule_set
        .role_policy(role)
        .ok_or_else(|| contract_error(&format!("未知或不允许的 agent_type `{role}`")))?;
    let workspace_root = hook_workspace_root
        .map(normalize_coordination_path)
        .transpose()
        .map_err(|error| contract_error(&format!("工作目录无效：{error}")))?;
    if policy.access == RoleAccess::Write && workspace_root.is_none() {
        return Err(contract_error("写入角色缺少可信工作目录"));
    }
    let capabilities = match policy.access {
        RoleAccess::ReadOnly => vec!["files.read".to_string()],
        // ponytail: one workspace-wide writer lock; add narrower native ownership
        // only if the executor exposes a trusted path field.
        RoleAccess::Write => vec![
            "command.execute".to_string(),
            "files.read".to_string(),
            "workspace.write".to_string(),
        ],
    };
    Ok(PreparedContract {
        capsule: TaskCapsule {
            id: task_name.to_string(),
        },
        role: role.to_string(),
        policy,
        workspace_root,
        trace: TraceContext::new(None),
        capabilities,
    })
}

fn spawn_field<'a>(
    input: &'a Map<String, Value>,
    aliases: &[&str],
    field_name: &str,
) -> std::result::Result<Option<&'a str>, String> {
    consistent_string_field(input, aliases)
        .map_err(|()| contract_error(&format!("{field_name} 别名冲突或类型无效")))
}

pub(super) fn contract_error(detail: &str) -> String {
    format!("Codey 子代理派发门禁：{detail}。")
}

pub(super) fn validate_task_id(value: &str) -> std::result::Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(contract_error(
            "task_name 只允许 1..=64 个小写字母、数字或下划线",
        ));
    }
    Ok(())
}

/// Canonicalize existing ancestors when possible so coordination claims for
/// obvious aliases overlap. These paths are scheduling metadata, not a file
/// ACL: metadata/canonicalization failures fall back to the lexical absolute
/// path, while the Codex executor remains the only filesystem authority.
pub(super) fn normalize_coordination_path(value: &str) -> std::result::Result<String, String> {
    let lexical = normalize_absolute_path(value)?;
    let path = PathBuf::from(&lexical);
    if !path.is_absolute() {
        return Ok(lexical);
    }
    let mut ancestor = path.as_path();
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = ancestor.file_name() else {
                    return Ok(lexical);
                };
                missing.push(name.to_os_string());
                let Some(parent) = ancestor.parent() else {
                    return Ok(lexical);
                };
                ancestor = parent;
            }
            Err(_) => return Ok(lexical),
        }
    }
    let Ok(mut resolved) = fs::canonicalize(ancestor) else {
        return Ok(lexical);
    };
    for component in missing.iter().rev() {
        resolved.push(component);
    }
    Ok(normalize_absolute_path(&resolved.to_string_lossy()).unwrap_or(lexical))
}

pub(super) fn normalize_absolute_path(value: &str) -> std::result::Result<String, String> {
    let mut replaced = value.trim().replace('\\', "/");
    if let Some(verbatim) = replaced.strip_prefix("//?/") {
        replaced = if verbatim
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("unc/"))
        {
            format!("//{}", &verbatim[4..])
        } else {
            verbatim.to_string()
        };
    }
    if replaced.is_empty() || replaced.contains(['*', '?', '[', ']']) {
        return Err("必须是无 glob 的绝对路径".to_string());
    }
    let (prefix, rest) = if let Some(rest) = replaced.strip_prefix("//") {
        ("//".to_string(), rest)
    } else if replaced.starts_with('/') {
        ("/".to_string(), replaced.trim_start_matches('/'))
    } else if replaced.len() >= 3
        && replaced.as_bytes()[0].is_ascii_alphabetic()
        && replaced.as_bytes()[1] == b':'
        && replaced.as_bytes()[2] == b'/'
    {
        (
            format!(
                "{}:/",
                (replaced.as_bytes()[0] as char).to_ascii_uppercase()
            ),
            &replaced[3..],
        )
    } else {
        return Err("必须是 Unix、UNC 或盘符绝对路径".to_string());
    };
    let mut components = Vec::new();
    for component in rest.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err("路径不能越过根目录".to_string());
                }
            }
            component => components.push(component),
        }
    }
    let joined = components.join("/");
    let mut result = if joined.is_empty() {
        prefix
    } else {
        format!("{prefix}{joined}")
    };
    if cfg!(windows) {
        result.make_ascii_lowercase();
    }
    Ok(result)
}
