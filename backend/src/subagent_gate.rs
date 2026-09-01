#[cfg(test)]
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::subagent::protocol::{self, AgentState as ObservedAgentState};

mod runtime_policy;
mod state;

use runtime_policy::{RuntimeSubagentPolicy, read_optional_runtime_policy_file};
pub(crate) use runtime_policy::{
    begin_runtime_subagent_policy_update, clear_runtime_subagent_policy,
    commit_runtime_subagent_policy, runtime_subagent_policy_matches, runtime_subagent_policy_paths,
    write_runtime_subagent_policy,
};
use state::*;

pub(crate) const HOOK_ARGUMENT: &str = "--codey-subagent-gate-hook";
pub(crate) const COMBINED_HOOK_ARGUMENT: &str = "--codey-subagent-gate-hook-with-fastctx";
pub(crate) const RUNTIME_ACTIVE_ENV: &str = "CODEY_SUBAGENT_GATE_ACTIVE";
pub(crate) const RUNTIME_ID_ENV: &str = "CODEY_SUBAGENT_GATE_RUNTIME_ID";
pub(crate) const HOOK_TIMEOUT_SECONDS: u64 = 5;
pub(crate) const SESSION_END_HOOK_TIMEOUT_SECONDS: u64 = 3;
const MAX_HOOK_INPUT_BYTES: u64 = 1024 * 1024;
const MAX_RENDERED_TOOL_RESULT_CHARS: usize = 8 * 1024;
pub(crate) const STATE_DIRECTORY: &str = "codey-subagent-gate-v3";
const ACTIVE_MARKER_SCHEMA_VERSION: u32 = 1;
const RUNTIME_SUBAGENT_POLICY_FILE: &str = "runtime-subagent-policy.json";
const RUNTIME_SUBAGENT_POLICY_PENDING_FILE: &str = "runtime-subagent-policy.pending.json";
const RUNTIME_SUBAGENT_POLICY_SCHEMA_VERSION: u32 = 1;
const RUNTIME_SUBAGENT_ATTESTATION_SCHEMA_VERSION: u32 = 1;
const RUNTIME_SUBAGENT_ATTESTATION_PREFIX: &str = "runtime-attestation-";
const MAX_RUNTIME_ATTESTATION_TRANSCRIPT_BYTES: u64 = 2 * 1024 * 1024;
const LEGACY_RUNTIME_ID: &str = "legacy-runtime";
const PENDING_INIT_GRACE_MILLIS: u64 = 10 * 60 * 1000;
const STOP_STALL_GRACE_MILLIS: u64 = 10 * 60 * 1000;
const STOP_ABSOLUTE_GRACE_MILLIS: u64 = 60 * 60 * 1000;
const PENDING_INIT_OBSERVED_FILE: &str = "pending-init-observed.state";
const STOP_BLOCKED_SINCE_FILE: &str = "stop-blocked-since.state";
const STOP_ABSOLUTE_SINCE_FILE: &str = "stop-absolute-since.state";
const STATUS_PROGRESS_FINGERPRINT_FILE: &str = "status-progress.state";
const STATE_ERROR_SINCE_FILE: &str = "state-error-since.state";
const SUBAGENT_CONTEXT_OBSERVED_FILE: &str = "subagent-context-observed.state";
const PROTOCOL_HEALTH_FILE: &str = "protocol-health.json";
const PROTOCOL_HEALTH_SCHEMA_VERSION: u32 = 1;
const ROOT_TURN_BINDING_FILE: &str = "root-turn-binding.json";
const ROOT_TURN_BINDING_SCHEMA_VERSION: u32 = 1;
const MISSING_AGENT_ID_MARKER: &str = "__codey_missing_agent_id__";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HookCommands {
    pub command: String,
    pub command_windows: String,
}

#[derive(Debug, Deserialize)]
struct HookInput {
    #[serde(alias = "hookEventName")]
    hook_event_name: String,
    #[serde(alias = "sessionId")]
    session_id: String,
    #[serde(
        default,
        alias = "agentId",
        alias = "agent_name",
        alias = "agentName",
        alias = "subagent_id",
        alias = "subagentId"
    )]
    agent_id: Option<String>,
    #[serde(
        default,
        alias = "agentType",
        alias = "subagent_type",
        alias = "subagentType"
    )]
    agent_type: Option<String>,
    #[serde(default, alias = "toolName")]
    tool_name: Option<String>,
    #[serde(default, alias = "toolInput")]
    tool_input: Option<Value>,
    #[serde(default, alias = "toolResponse")]
    tool_response: Option<Value>,
    #[serde(default, alias = "turnId")]
    turn_id: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default, alias = "transcriptPath")]
    transcript_path: Option<String>,
    #[serde(default, alias = "agentTranscriptPath")]
    agent_transcript_path: Option<String>,
    #[serde(default, alias = "working_dir", alias = "workingDirectory")]
    cwd: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeSubagentAttestation {
    schema_version: u32,
    runtime_id_hash: String,
    agent_id_hash: String,
    role: String,
    model: String,
    reasoning_effort: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservedRuntimeSubagentSelection {
    model: String,
    reasoning_effort: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HookMode {
    SubagentOnly,
    WithFastctx,
}

pub fn run_hook_if_requested() -> Result<bool> {
    let mode = match std::env::args_os().nth(1).as_deref() {
        Some(argument) if argument == OsStr::new(HOOK_ARGUMENT) => HookMode::SubagentOnly,
        Some(argument) if argument == OsStr::new(COMBINED_HOOK_ARGUMENT) => HookMode::WithFastctx,
        _ => return Ok(false),
    };
    let gate_active = runtime_gate_is_active(std::env::var_os(RUNTIME_ACTIVE_ENV).as_deref());
    if mode == HookMode::SubagentOnly && !gate_active {
        write_hook_output(&json!({}))?;
        return Ok(true);
    }

    let mut raw = Vec::new();
    std::io::stdin()
        .take(MAX_HOOK_INPUT_BYTES + 1)
        .read_to_end(&mut raw)
        .context("读取 Codex 子代理门禁 Hook 输入失败")?;
    let input = match parse_hook_input(&raw) {
        Ok(input) => input,
        Err(output) => {
            write_hook_output(&output)?;
            return Ok(true);
        }
    };
    let state_root = crate::codex_config::codex_home().join(STATE_DIRECTORY);
    let runtime_id = current_runtime_id();
    let output = match mode {
        HookMode::SubagentOnly => handle_hook_for_runtime(&input, &state_root, &runtime_id),
        HookMode::WithFastctx => {
            combined_hook_output_for_runtime(&input, &state_root, &runtime_id, gate_active)
        }
    }
    .unwrap_or_else(|error| {
        eprintln!("Codey 子代理门禁 Hook 失败：{error:#}");
        fail_closed_output(&input, &error)
    });
    write_hook_output(&output)?;
    Ok(true)
}

fn parse_hook_input(raw: &[u8]) -> std::result::Result<HookInput, Value> {
    if raw.len() as u64 > MAX_HOOK_INPUT_BYTES {
        return Err(undetermined_event_denial(
            "Hook 输入超过 1 MiB 上限，无法确认子代理状态；请缩小单次工具输入",
        ));
    }
    serde_json::from_slice(raw).map_err(|error| {
        undetermined_event_denial(&format!(
            "Hook 输入 JSON 解析失败（{error}），无法确认子代理状态"
        ))
    })
}

// 输入本身不可用时事件名未知，两种 Hook 输出形状都带上，确保 Codex 按拒绝处理。
fn undetermined_event_denial(detail: &str) -> Value {
    let reason = format!("Codey 子代理门禁 fail-closed：{detail}，已拒绝本次操作。");
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason.clone(),
        },
        "decision": "block",
        "reason": reason,
    })
}

fn runtime_gate_is_active(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("1"))
}

fn current_runtime_id() -> String {
    std::env::var(RUNTIME_ID_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| LEGACY_RUNTIME_ID.to_string())
}

fn write_hook_output(output: &Value) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, output).context("序列化 Codex 子代理门禁 Hook 输出失败")?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn hook_commands() -> Result<HookCommands> {
    hook_commands_for(HOOK_ARGUMENT)
}

pub(crate) fn hook_commands_for(argument: &str) -> Result<HookCommands> {
    let executable = std::env::current_exe().context("定位 Codey 子代理门禁程序失败")?;
    Ok(HookCommands {
        command: format!("{} {argument}", quote_posix(&executable)),
        command_windows: format!(
            "{} {argument}",
            powershell_executable_invocation(&executable)
        ),
    })
}

pub(crate) fn hook_trust_hash(
    event_name: &str,
    matcher: Option<&str>,
    command: &str,
    timeout_seconds: u64,
) -> String {
    let mut handler = Map::new();
    handler.insert("async".to_string(), Value::Bool(false));
    handler.insert("command".to_string(), Value::String(command.to_string()));
    handler.insert("timeout".to_string(), Value::Number(timeout_seconds.into()));
    handler.insert("type".to_string(), Value::String("command".to_string()));

    let mut identity = Map::new();
    identity.insert(
        "event_name".to_string(),
        Value::String(event_name.to_string()),
    );
    identity.insert(
        "hooks".to_string(),
        Value::Array(vec![Value::Object(handler)]),
    );
    if let Some(matcher) = matcher {
        identity.insert("matcher".to_string(), Value::String(matcher.to_string()));
    }
    let canonical = canonical_json(&Value::Object(identity));
    let serialized =
        serde_json::to_vec(&canonical).expect("canonical JSON values must be serializable");
    let digest = Sha256::digest(serialized);
    format!("sha256:{digest:x}")
}

#[cfg(test)]
fn handle_hook(input: &HookInput, state_root: &Path) -> Result<Value> {
    handle_hook_for_runtime(input, state_root, &current_runtime_id())
}

fn handle_hook_for_runtime(
    input: &HookInput,
    state_root: &Path,
    runtime_id: &str,
) -> Result<Value> {
    handle_hook_for_runtime_at(input, state_root, runtime_id, current_timestamp_millis())
}

fn combined_hook_output_for_runtime(
    input: &HookInput,
    state_root: &Path,
    runtime_id: &str,
    gate_active: bool,
) -> Result<Value> {
    let gate_output = if gate_active {
        handle_hook_for_runtime(input, state_root, runtime_id)?
    } else {
        json!({})
    };
    Ok(crate::subagent::hook_composer::first_decision(
        gate_output,
        || {
            crate::fastctx_route_gate::hook_output(
                &input.hook_event_name,
                input.tool_name.as_deref(),
                input.tool_input.as_ref(),
            )
        },
    ))
}

fn handle_hook_for_runtime_at(
    input: &HookInput,
    state_root: &Path,
    runtime_id: &str,
    now_ms: u64,
) -> Result<Value> {
    match input.hook_event_name.as_str() {
        "UserPromptSubmit" => user_prompt_submit_output(input, state_root, runtime_id, now_ms),
        "SubagentStart" => {
            if let Some(agent_id) = nonempty(input.agent_id.as_deref()) {
                let should_track = crate::subagent_orchestrator::subagent_started_with_context(
                    state_root,
                    runtime_id,
                    &input.session_id,
                    agent_id,
                    nonempty(input.agent_type.as_deref()),
                    nonempty(input.transcript_path.as_deref()),
                    now_ms,
                )?;
                if should_track {
                    create_active_marker(state_root, runtime_id, &input.session_id, agent_id)?;
                }
            } else {
                if nonempty(input.agent_type.as_deref()).is_some() {
                    record_subagent_context_observed(
                        state_root,
                        runtime_id,
                        &input.session_id,
                        now_ms,
                    )?;
                }
                record_protocol_issue(
                    state_root,
                    runtime_id,
                    &input.session_id,
                    ProtocolIssueKind::MissingAgentId,
                    "SubagentStart 载荷缺少 agent_id，无法可靠区分父子代理",
                    now_ms,
                )?;
                create_active_marker(
                    state_root,
                    runtime_id,
                    &input.session_id,
                    MISSING_AGENT_ID_MARKER,
                )?;
            }
            Ok(json!({}))
        }
        "SubagentStop" => {
            if let Some(agent_id) = nonempty(input.agent_id.as_deref()) {
                crate::subagent_orchestrator::subagent_stopped_with_context(
                    state_root,
                    runtime_id,
                    &input.session_id,
                    agent_id,
                    nonempty(input.agent_type.as_deref()),
                    nonempty(input.agent_transcript_path.as_deref()),
                    now_ms,
                )?;
                remove_active_marker(state_root, runtime_id, &input.session_id, agent_id)?;
                if active_agent_count_for_runtime(state_root, runtime_id, &input.session_id)? == 0 {
                    let _ = crate::subagent_orchestrator::open_batch_decision_if_settled(
                        state_root,
                        runtime_id,
                        &input.session_id,
                        0,
                        now_ms,
                    )?;
                    remove_session_state(state_root, runtime_id, &input.session_id)?;
                }
            } else {
                if nonempty(input.agent_type.as_deref()).is_some() {
                    record_subagent_context_observed(
                        state_root,
                        runtime_id,
                        &input.session_id,
                        now_ms,
                    )?;
                }
                let settlement = crate::subagent_orchestrator::settle_unique_anonymous_stop(
                    state_root,
                    runtime_id,
                    &input.session_id,
                    nonempty(input.agent_type.as_deref()),
                    now_ms,
                )?;
                record_protocol_issue(
                    state_root,
                    runtime_id,
                    &input.session_id,
                    ProtocolIssueKind::MissingAgentId,
                    if settlement.is_some() {
                        "SubagentStop 载荷缺少 agent_id；已按唯一活动账本候选保守结算"
                    } else {
                        "SubagentStop 载荷缺少 agent_id，且活动候选不唯一，已保留门禁等待权威对账"
                    },
                    now_ms,
                )?;
                if let Some(settlement) = settlement {
                    if let Some(agent_id_hash) = settlement.agent_id_hash.as_deref() {
                        remove_active_marker_by_hash(
                            state_root,
                            runtime_id,
                            &input.session_id,
                            agent_id_hash,
                        )?;
                    }
                    let active =
                        active_agent_count_for_runtime(state_root, runtime_id, &input.session_id)?;
                    if active == 0 {
                        remove_active_marker(
                            state_root,
                            runtime_id,
                            &input.session_id,
                            MISSING_AGENT_ID_MARKER,
                        )?;
                        let _ = crate::subagent_orchestrator::open_batch_decision_if_settled(
                            state_root,
                            runtime_id,
                            &input.session_id,
                            0,
                            now_ms,
                        )?;
                        remove_session_state(state_root, runtime_id, &input.session_id)?;
                    }
                }
            }
            Ok(json!({}))
        }
        "SessionEnd" => {
            crate::subagent_orchestrator::end_session(
                state_root,
                runtime_id,
                &input.session_id,
                now_ms,
            )?;
            remove_session_state(state_root, runtime_id, &input.session_id)?;
            Ok(json!({}))
        }
        "PreToolUse" => pre_tool_use_output(input, state_root, runtime_id, now_ms),
        "PostToolUse" => post_tool_use_output(input, state_root, runtime_id, now_ms),
        "Stop" => stop_output(input, state_root, runtime_id, now_ms),
        _ => Ok(json!({})),
    }
}

fn user_prompt_submit_output(
    input: &HookInput,
    state_root: &Path,
    runtime_id: &str,
    now_ms: u64,
) -> Result<Value> {
    if input_has_subagent_context(input) {
        return Ok(json!({}));
    }
    let active = active_agent_count_for_runtime(state_root, runtime_id, &input.session_id)?;
    if active == 0 {
        return Ok(json!({}));
    }

    let rebound = if let Some(turn_id) = nonempty(input.turn_id.as_deref()) {
        bind_root_turn(state_root, runtime_id, &input.session_id, turn_id, now_ms)?;
        true
    } else {
        false
    };
    let compatibility = if rebound {
        String::new()
    } else {
        " 当前 Hook 载荷缺少 turn_id，根身份无法重新绑定；除无筛选 list/wait 外的协作调用仍会 fail-closed。"
            .to_string()
    };
    let _prompt_was_present = nonempty(input.prompt.as_deref()).is_some();
    Ok(json!({
        "hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit",
            "additionalContext": format!(
                "Codey 检测到本轮用户输入到达时仍有 {active} 个子代理未确认终态。当前用户输入优先于旧任务描述：先调用一次不带筛选的 agents.list_agents 对账；若用户明确取消或缩小了某个子任务，只中断仍非终态且被明确取消的 target，再继续 wait/list 直到本批全部终态。普通状态询问或补充信息不得被解释为取消全部代理；完成对账前不得恢复非协作本地工作。{compatibility}"
            )
        }
    }))
}

fn runtime_subagent_attestation_denial(
    input: &HookInput,
    state_root: &Path,
    runtime_id: &str,
) -> Result<Option<String>> {
    let Some(agent_id) = nonempty(input.agent_id.as_deref()) else {
        return Ok(None);
    };
    let role =
        nonempty(input.agent_type.as_deref()).unwrap_or(crate::config::SUBAGENT_ROLE_DEFAULT);
    let session_dir = session_state_dir(state_root, &input.session_id);
    let attestation_path = runtime_subagent_attestation_path(&session_dir, runtime_id, agent_id);
    if cached_runtime_subagent_attestation_matches(&attestation_path, runtime_id, agent_id, role)? {
        // A child that was already attested may finish while a later role-policy
        // update is pending. The new policy applies only to newly spawned work.
        return Ok(None);
    }

    let (policy_path, pending_path) = (
        state_root.join(RUNTIME_SUBAGENT_POLICY_FILE),
        state_root.join(RUNTIME_SUBAGENT_POLICY_PENDING_FILE),
    );
    if read_optional_runtime_policy_file(&pending_path)?.is_some() {
        return Ok(Some(
            "CODEY_SUBAGENT_RUNTIME_UPDATE_IN_PROGRESS: 子代理角色策略正在切换；当前 child 尚未完成运行配置证明，已暂停工具调用。请让根代理等待本次设置保存完成后重新派发。"
                .to_string(),
        ));
    }
    let Some(policy_bytes) = read_optional_runtime_policy_file(&policy_path)? else {
        // Backward compatibility for runtimes created before attestation policy
        // files existed. Every new start/save writes the policy and removes this
        // compatibility branch naturally.
        return Ok(None);
    };
    let policy = match serde_json::from_slice::<RuntimeSubagentPolicy>(&policy_bytes) {
        Ok(policy) if policy.schema_version == RUNTIME_SUBAGENT_POLICY_SCHEMA_VERSION => policy,
        Ok(policy) => {
            return Ok(Some(format!(
                "CODEY_SUBAGENT_RUNTIME_POLICY_INVALID: 子代理运行时策略版本不受支持（实际 {}，预期 {}）；已拒绝在未验证配置上执行工具。",
                policy.schema_version, RUNTIME_SUBAGENT_POLICY_SCHEMA_VERSION
            )));
        }
        Err(error) => {
            return Ok(Some(format!(
                "CODEY_SUBAGENT_RUNTIME_POLICY_INVALID: 子代理运行时策略无法解析（{error}）；已拒绝在未验证配置上执行工具。"
            )));
        }
    };
    let Some(expected) = policy.roles.get(role) else {
        return Ok(Some(format!(
            "CODEY_SUBAGENT_RUNTIME_POLICY_INVALID: 运行时策略缺少角色 `{role}`；已拒绝未经映射的子代理工具调用。"
        )));
    };
    let expected_model = expected.model.trim();
    let expected_effort = expected.reasoning_effort.trim().to_ascii_lowercase();
    let observed = observed_runtime_subagent_selection(
        state_root,
        agent_id,
        nonempty(input.transcript_path.as_deref()),
        nonempty(input.turn_id.as_deref()),
    )?;
    let Some(observed) = observed else {
        return Ok(Some(format!(
            "CODEY_SUBAGENT_RUNTIME_UNVERIFIED: 无法从受信任的 child turn_context 证明角色 `{role}` 实际使用的模型和思考深度；已暂停工具调用。"
        )));
    };
    if observed.model != expected_model || observed.reasoning_effort != expected_effort {
        return Ok(Some(format!(
            "CODEY_SUBAGENT_RUNTIME_CONFIG_MISMATCH: 角色 `{role}` 预期 `{expected_model}` / `{expected_effort}`，实际 turn_context 为 `{}` / `{}`；已拒绝在错误模型映射上继续执行。",
            observed.model, observed.reasoning_effort
        )));
    }

    let attestation = RuntimeSubagentAttestation {
        schema_version: RUNTIME_SUBAGENT_ATTESTATION_SCHEMA_VERSION,
        runtime_id_hash: hash_component(runtime_id),
        agent_id_hash: hash_component(agent_id),
        role: role.to_string(),
        model: observed.model,
        reasoning_effort: observed.reasoning_effort,
    };
    let bytes = serde_json::to_vec(&attestation).context("序列化子代理运行配置证明失败")?;
    crate::fs_util::atomic_write_private_with_parent(&attestation_path, &bytes)
        .with_context(|| format!("保存子代理运行配置证明失败：{}", attestation_path.display()))?;
    Ok(None)
}

fn cached_runtime_subagent_attestation_matches(
    path: &Path,
    runtime_id: &str,
    agent_id: &str,
    role: &str,
) -> Result<bool> {
    let Some(bytes) = read_optional_runtime_policy_file(path)? else {
        return Ok(false);
    };
    let Ok(attestation) = serde_json::from_slice::<RuntimeSubagentAttestation>(&bytes) else {
        return Ok(false);
    };
    Ok(
        attestation.schema_version == RUNTIME_SUBAGENT_ATTESTATION_SCHEMA_VERSION
            && attestation.runtime_id_hash == hash_component(runtime_id)
            && attestation.agent_id_hash == hash_component(agent_id)
            && attestation.role == role,
    )
}

fn runtime_subagent_attestation_path(
    session_dir: &Path,
    runtime_id: &str,
    agent_id: &str,
) -> PathBuf {
    session_dir.join(format!(
        "{}{RUNTIME_SUBAGENT_ATTESTATION_PREFIX}{}.json",
        runtime_marker_prefix(runtime_id),
        hash_component(agent_id)
    ))
}

fn observed_runtime_subagent_selection(
    state_root: &Path,
    agent_id: &str,
    transcript_path: Option<&str>,
    turn_id: Option<&str>,
) -> Result<Option<ObservedRuntimeSubagentSelection>> {
    let Some(transcript_path) = transcript_path.map(Path::new) else {
        return Ok(None);
    };
    if !transcript_path.is_absolute()
        || transcript_path.extension().and_then(|value| value.to_str()) != Some("jsonl")
    {
        return Ok(None);
    }
    let metadata = match fs::symlink_metadata(transcript_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Ok(None);
    }
    let Some(codex_home) = state_root.parent() else {
        return Ok(None);
    };
    let sessions_root = match fs::canonicalize(codex_home.join("sessions")) {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    let canonical_transcript = match fs::canonicalize(transcript_path) {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    if !canonical_transcript.starts_with(&sessions_root)
        || !canonical_transcript
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.ends_with(&format!("-{agent_id}.jsonl")))
    {
        return Ok(None);
    }

    let mut file = fs::File::open(&canonical_transcript).with_context(|| {
        format!(
            "打开子代理 rollout 以验证运行配置失败：{}",
            canonical_transcript.display()
        )
    })?;
    let length = file.metadata()?.len();
    let start = length.saturating_sub(MAX_RUNTIME_ATTESTATION_TRANSCRIPT_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((length - start).min(usize::MAX as u64) as usize);
    file.take(MAX_RUNTIME_ATTESTATION_TRANSCRIPT_BYTES)
        .read_to_end(&mut bytes)?;
    let records = if start == 0 {
        bytes.as_slice()
    } else if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
        &bytes[newline + 1..]
    } else {
        &[]
    };

    let mut observed = None;
    for line in records.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if record.get("type").and_then(Value::as_str) != Some("turn_context") {
            continue;
        }
        let Some(payload) = record.get("payload").and_then(Value::as_object) else {
            continue;
        };
        if let Some(turn_id) = turn_id
            && payload.get("turn_id").and_then(Value::as_str) != Some(turn_id)
            && payload.get("turnId").and_then(Value::as_str) != Some(turn_id)
        {
            continue;
        }
        let Some(model) = json_nonempty_string(payload, &["model"]) else {
            continue;
        };
        let Some(reasoning_effort) = json_nonempty_string(
            payload,
            &[
                "effort",
                "reasoning_effort",
                "reasoningEffort",
                "model_reasoning_effort",
            ],
        ) else {
            continue;
        };
        observed = Some(ObservedRuntimeSubagentSelection {
            model,
            reasoning_effort: reasoning_effort.to_ascii_lowercase(),
        });
    }
    Ok(observed)
}

fn json_nonempty_string(payload: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        payload
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn pre_tool_use_output(
    input: &HookInput,
    state_root: &Path,
    runtime_id: &str,
    now_ms: u64,
) -> Result<Value> {
    let child_agent_id = nonempty(input.agent_id.as_deref());
    if input_has_subagent_context(input) {
        let Some(tool_name) = input.tool_name.as_deref() else {
            return Ok(json!({}));
        };
        if let Some(reason) = runtime_subagent_attestation_denial(input, state_root, runtime_id)? {
            return Ok(pre_tool_reason_denial(reason));
        }
        if is_batch_decision_tool(tool_name) || is_prepare_delegation_tool(tool_name) {
            return Ok(pre_tool_reason_denial(
                "Codey 子代理控制门禁：只有根代理可以提交批次决策或写入 sidecar。".to_string(),
            ));
        }
        if let Some(agent_id) = child_agent_id {
            if let Some(reason) = crate::subagent_orchestrator::authorize_child_tool_with_context(
                state_root,
                runtime_id,
                &input.session_id,
                crate::subagent_orchestrator::ChildToolContext {
                    agent_id,
                    agent_type: nonempty(input.agent_type.as_deref()),
                    transcript_path: nonempty(input.transcript_path.as_deref()),
                    tool_name,
                    tool_input: input.tool_input.as_ref(),
                },
                now_ms,
            )? {
                return Ok(pre_tool_reason_denial(reason));
            }
            return Ok(json!({}));
        }
        if crate::subagent_orchestrator::safe_child_reporting_tool(
            tool_name,
            input.tool_input.as_ref(),
        ) {
            return Ok(json!({}));
        }
        return Ok(subagent_identity_missing_denial());
    }
    let active = active_agent_count_for_runtime(state_root, runtime_id, &input.session_id)?;
    let trusted_root_turn = root_turn_matches(
        state_root,
        runtime_id,
        &input.session_id,
        nonempty(input.turn_id.as_deref()),
    )?;
    if active > 0 && !trusted_root_turn {
        let Some(tool_name) = input.tool_name.as_deref() else {
            return Ok(pre_tool_denial(active, None));
        };
        if is_anonymous_reconciliation_tool(tool_name, input.tool_input.as_ref()) {
            return Ok(json!({}));
        }
        if is_collaboration_tool(tool_name)
            || is_batch_decision_tool(tool_name)
            || is_prepare_delegation_tool(tool_name)
        {
            return Ok(pre_tool_reason_denial(format!(
                "Codey 主体身份门禁：仍有 {active} 个活动子代理，但当前 PreToolUse 载荷既没有可信的 child 身份，也没有匹配本批首个根派生调用的 turn_id，无法证明调用者是根代理。为防止匿名 child 派生、追派、中断或操纵批次，当前仅允许 agents.wait_agent 与不带筛选的 agents.list_agents 对账；其余编排调用已按 fail-closed 拒绝。"
            )));
        }
    }
    if input
        .tool_name
        .as_deref()
        .is_some_and(is_prepare_delegation_tool)
    {
        if let Some(role) = requested_spawn_role(input.tool_input.as_ref())
            && let Some(reason) = runtime_role_admission_denial(state_root, role)?
        {
            return Ok(pre_tool_reason_denial(reason));
        }
        let Some(root_turn_id) = nonempty(input.turn_id.as_deref()) else {
            return Ok(pre_tool_reason_denial(
                "Codey 写入 sidecar 门禁：当前 PreToolUse 缺少根 turn_id，无法建立只限本回合消费的一次性写入授权。"
                    .to_string(),
            ));
        };
        let process_cwd = std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned());
        let workspace_root = nonempty(input.cwd.as_deref()).or(process_cwd.as_deref());
        if let Some(reason) = crate::subagent_orchestrator::prepare_delegation_sidecar(
            state_root,
            runtime_id,
            &input.session_id,
            input.tool_input.as_ref(),
            crate::subagent_orchestrator::RootHookContext::new(
                workspace_root,
                Some(root_turn_id),
                active,
                now_ms,
            ),
        )? {
            return Ok(pre_tool_reason_denial(reason));
        }
        if active == 0 {
            bind_root_turn(
                state_root,
                runtime_id,
                &input.session_id,
                root_turn_id,
                now_ms,
            )?;
        }
        return Ok(json!({}));
    }
    if input
        .tool_name
        .as_deref()
        .is_some_and(is_batch_decision_tool)
    {
        if let Some(reason) = crate::subagent_orchestrator::prepare_batch_decision(
            state_root,
            runtime_id,
            &input.session_id,
            input.tool_input.as_ref(),
            active,
            now_ms,
        )? {
            return Ok(pre_tool_reason_denial(reason));
        }
        return Ok(json!({}));
    }
    if input
        .tool_name
        .as_deref()
        .is_some_and(is_followup_task_tool)
    {
        if let Some(reason) = crate::subagent_orchestrator::pre_followup_task(
            state_root,
            runtime_id,
            &input.session_id,
            input.tool_input.as_ref(),
            now_ms,
        )? {
            return Ok(pre_tool_reason_denial(reason));
        }
        return Ok(json!({}));
    }
    if input
        .tool_name
        .as_deref()
        .is_some_and(is_contract_spawn_tool)
    {
        if active > 0
            && let Some(reason) = protocol_issue_reason(state_root, runtime_id, &input.session_id)?
            && !missing_agent_id_has_classified_subagent_context(
                state_root,
                runtime_id,
                &input.session_id,
            )?
        {
            return Ok(pre_tool_reason_denial(format!(
                "Codey Hook 协议兼容性门禁：{reason}。当前无法可靠区分根代理和子代理，已停止继续派生；请先调用不带筛选的 agents.list_agents 对账。"
            )));
        }
        if let Some(role) = requested_spawn_role(input.tool_input.as_ref())
            && let Some(reason) = runtime_role_admission_denial(state_root, role)?
        {
            return Ok(pre_tool_reason_denial(reason));
        }
        let process_cwd = std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned());
        let workspace_root = nonempty(input.cwd.as_deref()).or(process_cwd.as_deref());
        if let Some(reason) = crate::subagent_orchestrator::pre_spawn_with_workspace_and_turn(
            state_root,
            runtime_id,
            &input.session_id,
            input.tool_input.as_ref(),
            crate::subagent_orchestrator::RootHookContext::new(
                workspace_root,
                nonempty(input.turn_id.as_deref()),
                active,
                now_ms,
            ),
        )? {
            return Ok(pre_tool_reason_denial(reason));
        }
        if active == 0
            && let Some(turn_id) = nonempty(input.turn_id.as_deref())
        {
            bind_root_turn(state_root, runtime_id, &input.session_id, turn_id, now_ms)?;
        }
        return Ok(json!({}));
    }
    if input
        .tool_name
        .as_deref()
        .is_some_and(is_collaboration_tool)
    {
        return Ok(json!({}));
    }
    if active > 0
        && trusted_root_turn
        && input
            .tool_name
            .as_deref()
            .is_some_and(is_root_local_read_tool)
        && verified_local_read_only_active_count(state_root, runtime_id, &input.session_id, now_ms)?
            == Some(active)
    {
        return Ok(json!({}));
    }
    if active == 0 {
        if let Some(reason) = crate::subagent_orchestrator::pre_root_tool(
            state_root,
            runtime_id,
            &input.session_id,
            input.tool_input.as_ref(),
            now_ms,
        )? {
            return Ok(pre_tool_reason_denial(reason));
        }
        return Ok(json!({}));
    }
    let protocol_issue = protocol_issue_reason(state_root, runtime_id, &input.session_id)?;
    Ok(pre_tool_denial(active, protocol_issue.as_deref()))
}

fn requested_spawn_role(tool_input: Option<&Value>) -> Option<&str> {
    let input = tool_input?.as_object()?;
    let mut roles = ["agent_type", "agentType", "agent_role", "agentRole"]
        .into_iter()
        .filter_map(|key| input.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|role| !role.is_empty());
    let role = roles.next()?;
    roles.all(|candidate| candidate == role).then_some(role)
}

fn runtime_role_admission_denial(state_root: &Path, role: &str) -> Result<Option<String>> {
    let pending_path = state_root.join(RUNTIME_SUBAGENT_POLICY_PENDING_FILE);
    if read_optional_runtime_policy_file(&pending_path)?.is_some() {
        return Ok(Some(
            "CODEY_SUBAGENT_RUNTIME_UPDATE_IN_PROGRESS: 子代理角色策略正在切换；请等待设置保存完成后重新派发。未创建调度账本记录。"
                .to_string(),
        ));
    }
    let policy_path = state_root.join(RUNTIME_SUBAGENT_POLICY_FILE);
    let Some(policy_bytes) = read_optional_runtime_policy_file(&policy_path)? else {
        // Runtimes created before role attestation remain backward compatible.
        return Ok(None);
    };
    let policy = match serde_json::from_slice::<RuntimeSubagentPolicy>(&policy_bytes) {
        Ok(policy) if policy.schema_version == RUNTIME_SUBAGENT_POLICY_SCHEMA_VERSION => policy,
        Ok(policy) => {
            return Ok(Some(format!(
                "CODEY_SUBAGENT_RUNTIME_POLICY_INVALID: 子代理运行时策略版本不受支持（实际 {}，预期 {}）；未创建调度账本记录。",
                policy.schema_version, RUNTIME_SUBAGENT_POLICY_SCHEMA_VERSION
            )));
        }
        Err(error) => {
            return Ok(Some(format!(
                "CODEY_SUBAGENT_RUNTIME_POLICY_INVALID: 子代理运行时策略无法解析（{error}）；未创建调度账本记录。"
            )));
        }
    };
    if policy.roles.contains_key(role) {
        Ok(None)
    } else if crate::config::SUBAGENT_ROLE_IDS.contains(&role) {
        Ok(Some(format!(
            "CODEY_SUBAGENT_ROLE_DISABLED: Codey 子代理角色 `{role}` 已关闭；请在设置中开启后重试，或改用已启用角色。未创建调度账本记录。"
        )))
    } else {
        Ok(Some(format!(
            "CODEY_SUBAGENT_ROLE_UNKNOWN: Codey 子代理角色 `{role}` 不在当前运行时可用角色集合中；未创建调度账本记录。"
        )))
    }
}

fn post_tool_use_output(
    input: &HookInput,
    state_root: &Path,
    runtime_id: &str,
    now_ms: u64,
) -> Result<Value> {
    if input_has_subagent_context(input) {
        return Ok(json!({}));
    }
    let Some(tool_name) = input.tool_name.as_deref() else {
        return Ok(json!({}));
    };
    if is_contract_spawn_tool(tool_name) {
        crate::subagent_orchestrator::post_spawn(
            state_root,
            runtime_id,
            &input.session_id,
            input.tool_input.as_ref(),
            input.tool_response.as_ref(),
            now_ms,
        )?;
        return Ok(json!({}));
    }
    if is_batch_decision_tool(tool_name) {
        if let Some(reason) = crate::subagent_orchestrator::post_batch_decision(
            state_root,
            runtime_id,
            &input.session_id,
            input.tool_input.as_ref(),
            input.tool_response.as_ref(),
            now_ms,
        )? {
            return Ok(json!({
                "decision": "block",
                "reason": reason,
            }));
        }
        return Ok(json!({}));
    }
    if is_prepare_delegation_tool(tool_name) {
        let Some(root_turn_id) = nonempty(input.turn_id.as_deref()) else {
            return Ok(json!({
                "decision": "block",
                "reason": "Codey 写入 sidecar 门禁：PostToolUse 缺少根 turn_id，预备委派未提交。",
            }));
        };
        if !root_turn_matches(
            state_root,
            runtime_id,
            &input.session_id,
            Some(root_turn_id),
        )? {
            return Ok(json!({
                "decision": "block",
                "reason": "Codey 写入 sidecar 门禁：PostToolUse 的根 turn_id 与 PreToolUse 绑定不一致，预备委派未提交。",
            }));
        }
        let process_cwd = std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned());
        let workspace_root = nonempty(input.cwd.as_deref()).or(process_cwd.as_deref());
        let active = active_agent_count_for_runtime(state_root, runtime_id, &input.session_id)?;
        if let Some(reason) = crate::subagent_orchestrator::post_delegation_sidecar(
            state_root,
            runtime_id,
            &input.session_id,
            input.tool_input.as_ref(),
            input.tool_response.as_ref(),
            crate::subagent_orchestrator::RootHookContext::new(
                workspace_root,
                Some(root_turn_id),
                active,
                now_ms,
            ),
        )? {
            return Ok(json!({
                "decision": "block",
                "reason": reason,
            }));
        }
        return Ok(json!({}));
    }
    if is_interrupt_agent_tool(tool_name) {
        if let Some(acknowledgement) = input
            .tool_response
            .as_ref()
            .and_then(protocol::interrupt_acknowledgement)
            && let Some(settlement) =
                crate::subagent_orchestrator::settle_interrupt_acknowledgement(
                    state_root,
                    runtime_id,
                    &input.session_id,
                    input.tool_input.as_ref(),
                    &acknowledgement,
                    now_ms,
                )?
        {
            if let Some(agent_id_hash) = settlement.agent_id_hash.as_deref() {
                remove_active_marker_by_hash(
                    state_root,
                    runtime_id,
                    &input.session_id,
                    agent_id_hash,
                )?;
            }
            let active = active_agent_count_for_runtime(state_root, runtime_id, &input.session_id)?;
            if active == 0
                && let Some(reason) = crate::subagent_orchestrator::open_batch_decision_if_settled(
                    state_root,
                    runtime_id,
                    &input.session_id,
                    0,
                    now_ms,
                )?
            {
                return Ok(json!({ "decision": "block", "reason": reason }));
            }
        }
        return Ok(json!({}));
    }
    crate::subagent_orchestrator::post_root_tool(
        state_root,
        runtime_id,
        &input.session_id,
        input.tool_input.as_ref(),
        input.tool_response.as_ref(),
        now_ms,
    )?;
    if !is_agent_status_tool(tool_name) {
        return Ok(json!({}));
    }

    let response_is_usable = if is_wait_agent_tool(tool_name) {
        wait_agent_response_is_usable(input.tool_response.as_ref())
    } else {
        summarize_list_agents_response(input.tool_response.as_ref())
            != AgentListSnapshotState::Unknown
    };
    if response_is_usable {
        if record_status_progress(
            state_root,
            runtime_id,
            &input.session_id,
            tool_name,
            input.tool_response.as_ref(),
        )? {
            remove_session_auxiliary_file(
                state_root,
                runtime_id,
                &input.session_id,
                STOP_BLOCKED_SINCE_FILE,
            )?;
        }
        clear_unknown_status_protocol_issue(state_root, runtime_id, &input.session_id, now_ms)?;
    } else {
        record_protocol_issue(
            state_root,
            runtime_id,
            &input.session_id,
            ProtocolIssueKind::UnknownStatusResponse,
            if is_wait_agent_tool(tool_name) {
                "wait_agent 响应结构无法识别"
            } else {
                "list_agents 响应结构无法识别"
            },
            now_ms,
        )?;
    }
    if is_wait_agent_tool(tool_name)
        && crate::subagent_orchestrator::active_reservation_count(
            state_root,
            runtime_id,
            &input.session_id,
            now_ms,
        )?
        .is_none()
    {
        // Legacy marker-only sessions cannot map canonical task paths through a
        // ledger. Modern sessions defer marker deletion until the uniquely
        // resolved terminal transition has been persisted below.
        remove_completed_agents_from_wait_response(
            state_root,
            runtime_id,
            &input.session_id,
            input.tool_response.as_ref(),
        )?;
    } else if reconcile_list_agents_response(input, state_root, runtime_id, now_ms)? {
        settle_status_response_and_markers(
            state_root,
            runtime_id,
            &input.session_id,
            input.tool_response.as_ref(),
            true,
            now_ms,
        )?;
        remove_session_state(state_root, runtime_id, &input.session_id)?;
        if let Some(reason) = crate::subagent_orchestrator::open_batch_decision_if_settled(
            state_root,
            runtime_id,
            &input.session_id,
            0,
            now_ms,
        )? {
            return Ok(json!({ "decision": "block", "reason": reason }));
        }
        return Ok(json!({}));
    }

    let Some(active) = active_agent_count_or_recover_corrupt_state(
        state_root,
        runtime_id,
        &input.session_id,
        now_ms,
    )?
    else {
        return Ok(json!({}));
    };
    if active == 0 {
        settle_status_response_and_markers(
            state_root,
            runtime_id,
            &input.session_id,
            input.tool_response.as_ref(),
            true,
            now_ms,
        )?;
        remove_session_state(state_root, runtime_id, &input.session_id)?;
        if let Some(reason) = crate::subagent_orchestrator::open_batch_decision_if_settled(
            state_root,
            runtime_id,
            &input.session_id,
            0,
            now_ms,
        )? {
            return Ok(json!({ "decision": "block", "reason": reason }));
        }
        return Ok(json!({}));
    }
    settle_status_response_and_markers(
        state_root,
        runtime_id,
        &input.session_id,
        input.tool_response.as_ref(),
        false,
        now_ms,
    )?;
    let Some(active) = active_agent_count_or_recover_corrupt_state(
        state_root,
        runtime_id,
        &input.session_id,
        now_ms,
    )?
    else {
        return Ok(json!({}));
    };
    if active == 0 {
        remove_session_state(state_root, runtime_id, &input.session_id)?;
        if let Some(reason) = crate::subagent_orchestrator::open_batch_decision_if_settled(
            state_root,
            runtime_id,
            &input.session_id,
            0,
            now_ms,
        )? {
            return Ok(json!({ "decision": "block", "reason": reason }));
        }
        return Ok(json!({}));
    }
    let root_local_reads_allowed = root_turn_matches(
        state_root,
        runtime_id,
        &input.session_id,
        nonempty(input.turn_id.as_deref()),
    )? && verified_local_read_only_active_count(
        state_root,
        runtime_id,
        &input.session_id,
        now_ms,
    )? == Some(active);
    let protocol_issue = protocol_issue_reason(state_root, runtime_id, &input.session_id)?;
    if is_wait_agent_tool(tool_name) {
        Ok(post_wait_continuation(
            active,
            input.tool_response.as_ref(),
            protocol_issue.as_deref(),
            root_local_reads_allowed,
        ))
    } else {
        Ok(post_list_continuation(
            active,
            input.tool_response.as_ref(),
            protocol_issue.as_deref(),
            root_local_reads_allowed,
        ))
    }
}

fn settle_status_response_and_markers(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    tool_response: Option<&Value>,
    all_terminal: bool,
    now_ms: u64,
) -> Result<()> {
    let settlement = crate::subagent_orchestrator::observe_status_response(
        state_root,
        runtime_id,
        session_id,
        tool_response,
        all_terminal,
        now_ms,
    )?;
    for agent_id_hash in &settlement.agent_id_hashes {
        remove_active_marker_by_hash(state_root, runtime_id, session_id, agent_id_hash)?;
    }
    Ok(())
}

fn stop_output(
    input: &HookInput,
    state_root: &Path,
    runtime_id: &str,
    now_ms: u64,
) -> Result<Value> {
    if input_has_subagent_context(input) {
        return Ok(json!({}));
    }
    let Some(mut active) = active_agent_count_or_recover_corrupt_state(
        state_root,
        runtime_id,
        &input.session_id,
        now_ms,
    )?
    else {
        return Ok(json!({}));
    };
    if active == 0 {
        return finalize_root_turn(state_root, runtime_id, &input.session_id, now_ms);
    }
    let ledger_pending_recovery =
        crate::subagent_orchestrator::recover_expired_pending_init_reservations(
            state_root,
            runtime_id,
            &input.session_id,
            now_ms,
            PENDING_INIT_GRACE_MILLIS,
        )?;
    if let Some(recovery) = &ledger_pending_recovery {
        remove_session_auxiliary_file(
            state_root,
            runtime_id,
            &input.session_id,
            PENDING_INIT_OBSERVED_FILE,
        )?;
        for agent_id_hash in &recovery.agent_id_hashes {
            remove_active_marker_by_hash(state_root, runtime_id, &input.session_id, agent_id_hash)?;
        }
        active = active_agent_count_for_runtime(state_root, runtime_id, &input.session_id)?;
        if active == 0 {
            remove_session_state(state_root, runtime_id, &input.session_id)?;
            return finalize_root_turn(state_root, runtime_id, &input.session_id, now_ms);
        }
    }
    // 先检查可重置的停滞窗口，确保绝对放行后如果协作路径不再推进，遗留
    // 活跃标记仍能在后续 10 分钟内回收，而不会被已到期的绝对计时永久短路。
    let legacy_pending_init_elapsed = ledger_pending_recovery.is_none()
        && observation_elapsed_if_present(
            state_root,
            runtime_id,
            &input.session_id,
            PENDING_INIT_OBSERVED_FILE,
            now_ms,
            PENDING_INIT_GRACE_MILLIS,
        )?;
    if legacy_pending_init_elapsed
        || observe_and_check_elapsed(
            state_root,
            runtime_id,
            &input.session_id,
            STOP_BLOCKED_SINCE_FILE,
            now_ms,
            STOP_STALL_GRACE_MILLIS,
        )?
    {
        crate::subagent_orchestrator::recover_active_reservations(
            state_root,
            runtime_id,
            &input.session_id,
            "gate recovery grace elapsed before an authoritative terminal outcome",
            now_ms,
        )?;
        remove_session_state(state_root, runtime_id, &input.session_id)?;
        return finalize_root_turn(state_root, runtime_id, &input.session_id, now_ms);
    }
    // 绝对上限自首次受阻起算，不被有效 wait/list 响应重置；放行时不清理账本，
    // 遗留状态仍由上面的 10 分钟停滞窗口与代次机制兜底。
    if observe_and_check_elapsed(
        state_root,
        runtime_id,
        &input.session_id,
        STOP_ABSOLUTE_SINCE_FILE,
        now_ms,
        STOP_ABSOLUTE_GRACE_MILLIS,
    )? {
        crate::subagent_orchestrator::recover_active_reservations(
            state_root,
            runtime_id,
            &input.session_id,
            "absolute Stop grace elapsed before an authoritative terminal outcome",
            now_ms,
        )?;
        remove_session_state(state_root, runtime_id, &input.session_id)?;
        record_protocol_issue(
            state_root,
            runtime_id,
            &input.session_id,
            ProtocolIssueKind::AbsoluteStopTimeout,
            "根代理 Stop 受阻累计超过 60 分钟，已 fence 活动 attempt 并按绝对上限放行",
            now_ms,
        )?;
        return finalize_root_turn(state_root, runtime_id, &input.session_id, now_ms);
    }
    let protocol_issue = protocol_issue_reason(state_root, runtime_id, &input.session_id)?;
    Ok(stop_continuation(active, protocol_issue.as_deref()))
}

fn finalize_root_turn(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    now_ms: u64,
) -> Result<Value> {
    if let Some(reason) = crate::subagent_orchestrator::pending_acceptance_reason(
        state_root, runtime_id, session_id, now_ms,
    )? {
        return Ok(json!({
            "decision": "block",
            "reason": reason,
        }));
    }
    if let Some(reason) = crate::subagent_orchestrator::batch_decision_stop_reason(
        state_root, runtime_id, session_id, now_ms,
    )? {
        return Ok(json!({
            "decision": "block",
            "reason": reason,
        }));
    }
    crate::subagent_orchestrator::settle_turn(state_root, runtime_id, session_id, now_ms)?;
    Ok(json!({}))
}

fn active_agent_count_or_recover_corrupt_state(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    now_ms: u64,
) -> Result<Option<usize>> {
    match active_agent_count_for_runtime(state_root, runtime_id, session_id) {
        Ok(active) => {
            remove_session_auxiliary_file(
                state_root,
                runtime_id,
                session_id,
                STATE_ERROR_SINCE_FILE,
            )?;
            Ok(Some(active))
        }
        Err(error) => {
            if observe_and_check_elapsed(
                state_root,
                runtime_id,
                session_id,
                STATE_ERROR_SINCE_FILE,
                now_ms,
                STOP_STALL_GRACE_MILLIS,
            )? {
                remove_session_state(state_root, runtime_id, session_id)?;
                Ok(None)
            } else {
                Err(error)
            }
        }
    }
}

fn fail_closed_output(input: &HookInput, error: &anyhow::Error) -> Value {
    let reason = format!(
        "Codey 无法确认子代理运行状态，已暂停主代理继续操作：{error:#}。请调用 agents.wait_agent 或 agents.list_agents 核对状态。若状态存储持续损坏，Stop 路径会在持续 10 分钟后回收当前运行代次；期间不得绕过门禁。"
    );
    match input.hook_event_name.as_str() {
        "PreToolUse"
            if !input_has_subagent_context(input)
                && input.tool_name.as_deref().is_some_and(|tool_name| {
                    is_anonymous_reconciliation_tool(tool_name, input.tool_input.as_ref())
                }) =>
        {
            json!({})
        }
        "PreToolUse" if !input_has_subagent_context(input) => json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        }),
        "PreToolUse"
            if input.tool_name.as_deref().is_some_and(|tool_name| {
                crate::subagent_orchestrator::safe_child_reporting_tool(
                    tool_name,
                    input.tool_input.as_ref(),
                )
            }) =>
        {
            json!({})
        }
        "PreToolUse" => json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": format!(
                    "Codey 无法读取子代理 ownership 账本：{error:#}。已按 fail-closed 暂停子代理的数据、副作用与编排工具；只保留向 `/root` 定向发送消息用于回报。"
                ),
            }
        }),
        "PostToolUse"
            if !input_has_subagent_context(input)
                && input.tool_name.as_deref().is_some_and(is_agent_status_tool) =>
        {
            json!({
                "decision": "block",
                "reason": reason,
            })
        }
        "Stop" if !input_has_subagent_context(input) => json!({
            "decision": "block",
            "reason": reason,
        }),
        _ => json!({}),
    }
}

fn pre_tool_denial(active: usize, protocol_issue: Option<&str>) -> Value {
    let compatibility = protocol_issue
        .map(|issue| format!(" 检测到 Hook 协议兼容性异常：{issue}。"))
        .unwrap_or_else(|| {
            " 如果这次调用实际来自子代理，说明上游 Hook 载荷缺少 agent_id，请重新验证当前 Codex 版本兼容性。"
                .to_string()
        });
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": format!(
                "Codey 子代理门禁：仍有 {active} 个子代理尚未确认进入终态。现在只可调用 agents.* 协作工具；请先调用 agents.list_agents 核对 running、pending_init、completed、errored、shutdown 等状态，再对仍在运行的代理调用 agents.wait_agent。{compatibility}"
            ),
        }
    })
}

fn pre_tool_reason_denial(reason: String) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    })
}

fn subagent_identity_missing_denial() -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": "Codey 子代理门禁：当前调用已确认来自子代理，但 Hook 载荷缺少 agent_id，无法校验 ownership。数据、副作用与编排工具全部拒绝；只可使用 agents.send_message 向 `/root` 回报兼容性诊断。",
        }
    })
}

fn post_wait_continuation(
    active: usize,
    tool_response: Option<&Value>,
    protocol_issue: Option<&str>,
    root_local_reads_allowed: bool,
) -> Value {
    let returned_update = render_tool_result(tool_response, "wait_agent");
    let decryption_recovery = tool_response
        .filter(|response| {
            crate::subagent::protocol::response_reports_task_body_decryption_failure(response)
        })
        .map(|_| {
            "\n\n检测到活动子代理报告任务正文解密失败。不要中断该代理，也不要立即重派；请使用 `agents.send_message` 向对应活动 target 只重述一次自包含的任务目标、输入、范围、约束和验收上下文，然后立即回到 `agents.wait_agent`。若重述无法送达、再次解密失败或代理已进入终态，由主代理接管，禁止循环重试。"
        })
        .unwrap_or_default();
    let compatibility = protocol_issue
        .map(|issue| format!("\n\nHook 协议兼容性诊断：{issue}。"))
        .unwrap_or_default();
    let local_read_guidance = if root_local_reads_allowed {
        " 当前账本与活动 marker 已共同证明剩余子代理均已绑定且只具备 `files.read`；可信根代理可使用 `mcp__codey_fastctx__inspect_local_file`、`mcp__codey_fastctx__grep` 或 `mcp__codey_fastctx__glob` 消化本次部分结果。写入、命令、网络、其他本地工具和结束任务仍被拒绝；完成有界读取后继续 wait/list 汇合。"
    } else {
        " 在所有子代理进入终态或被根成功中断并 fence 前，不得恢复非协作本地工作、形成最终结论或结束当前任务。"
    };
    json!({
        "decision": "block",
        "reason": format!(
            "Codey 子代理汇合门禁：本次 agents.wait_agent 返回后仍有 {active} 个子代理活动标记尚未核销。保留下方内容；可继续使用 agents.wait_agent 或不带筛选的 agents.list_agents 对账。只有当前调用仍携带并匹配本批首个根派生调用的 turn_id 时，才可使用 agents.send_message、agents.followup_task 或 agents.interrupt_agent 协调；缺少该绑定时按匿名主体 fail-closed。completed、errored、shutdown、not_found、FINAL_ANSWER 和 task_complete 都视为终态；只对仍活动且未被根成功中断的 running、pending_init 或 interrupted 代理继续等待。根中断获得结构化成功回执后，该 target 即在 Codey 中永久放弃并视为本批已结算；后来仍显示 pending_init、running 或 interrupted 的上游快照不得触发再次等待。不得自动重派；若持续没有可信终态，Stop 恢复路径会在受控宽限期后 fence 遗留 attempt。{local_read_guidance}\n\n本次 wait_agent 已返回内容：\n{returned_update}{decryption_recovery}{compatibility}"
        ),
    })
}

fn post_list_continuation(
    active: usize,
    tool_response: Option<&Value>,
    protocol_issue: Option<&str>,
    root_local_reads_allowed: bool,
) -> Value {
    let returned_update = render_tool_result(tool_response, "list_agents");
    let compatibility = protocol_issue
        .map(|issue| format!("\n\nHook 协议兼容性诊断：{issue}。"))
        .unwrap_or_default();
    let local_read_guidance = if root_local_reads_allowed {
        " 当前账本与活动 marker 已共同证明剩余子代理均已绑定且只具备 `files.read`；可信根代理可使用 FastCtx 的 inspect、grep 或 glob 消化已返回证据，但写入、命令、网络、其他本地工具和结束任务仍被拒绝，随后必须继续汇合。"
    } else {
        " 所有活动代理结算前继续保持全局本地工具屏障。"
    };
    json!({
        "decision": "block",
        "reason": format!(
            "Codey 子代理汇合门禁：agents.list_agents 核对后仍有 {active} 个子代理尚未确认进入终态。只对仍活动且未被根成功中断的 running、pending_init 或 interrupted 代理继续等待、转向或停止；completed、errored、shutdown 和 not_found 不再阻塞。累计 10 分钟仍无终态时只中断一次对应代理；中断获得结构化成功回执后立即接管，不再等待该 target 的上游状态变化，只有中断失败或目标无法匹配时才继续对账。不得无限 wait 或自动重派。若 pending_init 实际已僵死，门禁会在持续 10 分钟无法进展后释放遗留状态。{local_read_guidance}\n\n本次 list_agents 已返回内容：\n{returned_update}{compatibility}"
        ),
    })
}

fn stop_continuation(active: usize, protocol_issue: Option<&str>) -> Value {
    let compatibility = protocol_issue
        .map(|issue| {
            format!(
                " 检测到 Hook 协议兼容性异常：{issue}；请优先使用不带筛选的 agents.list_agents 对账。"
            )
        })
        .unwrap_or_default();
    json!({
        "decision": "block",
        "reason": format!(
            "Codey 子代理门禁：仍有 {active} 个子代理尚未确认进入终态，当前任务不能结束。请先调用不带筛选的 agents.list_agents 对账，再对仍活动且未被根成功中断的 running、pending_init 或 interrupted 代理调用 agents.wait_agent；累计 10 分钟仍无终态时只中断一次对应代理。中断获得结构化成功回执后立即接管，不再等待该 target；只有中断失败或目标无法匹配时才继续对账。不得无限重试或自动重派。若协作工具已经不可用，门禁会在持续 10 分钟无法进展后释放遗留状态。{compatibility}"
        ),
    })
}

fn render_tool_result(tool_response: Option<&Value>, tool_name: &str) -> String {
    let rendered = match tool_response {
        Some(Value::String(response)) => response.clone(),
        Some(response) => {
            serde_json::to_string(response).expect("serde_json::Value must always be serializable")
        }
        None => format!("（{tool_name} 未提供返回内容）"),
    };
    let Some((cut_at, _)) = rendered.char_indices().nth(MAX_RENDERED_TOOL_RESULT_CHARS) else {
        return rendered;
    };
    let mut bounded = rendered;
    bounded.truncate(cut_at);
    bounded.push_str(
        "\n…（协作工具返回内容已截断；请调用不带筛选的 agents.list_agents 获取紧凑状态）",
    );
    bounded
}

fn wait_agent_response_is_usable(tool_response: Option<&Value>) -> bool {
    let Some(tool_response) = tool_response else {
        return false;
    };
    match tool_response {
        Value::Object(values) => {
            (object_value(values, "timedout").is_some_and(Value::is_boolean)
                && (object_value(values, "message").is_some_and(Value::is_string)
                    || object_value(values, "status").is_some()))
                || object_value(values, "updates").is_some_and(Value::is_array)
                || object_value(values, "status").is_some_and(|status| {
                    classify_agent_status(status) != ObservedAgentState::Unknown
                })
                || object_reports_agent_completion(values)
        }
        Value::String(value) => serde_json::from_str::<Value>(value)
            .ok()
            .as_ref()
            .is_some_and(|value| wait_agent_response_is_usable(Some(value))),
        _ => false,
    }
}

/// Records semantic collaboration progress instead of treating every usable
/// poll as progress. Repeated `interrupted` or timeout snapshots therefore do
/// not postpone the bounded Stop recovery window indefinitely.
fn record_status_progress(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    tool_name: &str,
    tool_response: Option<&Value>,
) -> Result<bool> {
    let Some(fingerprint) = status_progress_fingerprint(tool_name, tool_response) else {
        return Ok(false);
    };
    let session_dir = session_state_dir(state_root, session_id);
    fs::create_dir_all(&session_dir).with_context(|| {
        format!(
            "创建 Codex 子代理状态进展目录失败：{}",
            session_dir.display()
        )
    })?;
    let path = session_auxiliary_path(&session_dir, runtime_id, STATUS_PROGRESS_FINGERPRINT_FILE);
    match fs::read_to_string(&path) {
        Ok(previous) if previous.trim() == fingerprint => Ok(false),
        Ok(_) => {
            crate::fs_util::atomic_write(&path, format!("{fingerprint}\n").as_bytes())
                .with_context(|| {
                    format!("写入 Codex 子代理状态进展指纹失败：{}", path.display())
                })?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::fs_util::atomic_write(&path, format!("{fingerprint}\n").as_bytes())
                .with_context(|| {
                    format!("写入 Codex 子代理状态进展指纹失败：{}", path.display())
                })?;
            Ok(true)
        }
        Err(error) => Err(error)
            .with_context(|| format!("读取 Codex 子代理状态进展指纹失败：{}", path.display())),
    }
}

fn status_progress_fingerprint(tool_name: &str, tool_response: Option<&Value>) -> Option<String> {
    let response = tool_response?;
    let decoded;
    let response = if let Value::String(encoded) = response {
        decoded = serde_json::from_str::<Value>(encoded).ok();
        decoded.as_ref().unwrap_or(response)
    } else {
        response
    };
    let mut tokens = Vec::new();
    collect_status_progress_tokens(response, &mut tokens, 0);
    tokens.sort();
    tokens.dedup();
    let encoded = serde_json::to_string(&tokens).ok()?;
    Some(hash_component(&format!(
        "{}|{encoded}",
        normalized_collaboration_tool(tool_name)
    )))
}

fn collect_status_progress_tokens(value: &Value, tokens: &mut Vec<String>, depth: usize) {
    if depth > 8 {
        return;
    }
    match value {
        Value::Array(values) => {
            for value in values {
                collect_status_progress_tokens(value, tokens, depth + 1);
            }
        }
        Value::Object(values) => {
            let identifier = object_value_any(
                values,
                &["agentid", "agentname", "subagentid", "taskname", "name"],
            )
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(normalized_ascii_identifier)
            .unwrap_or_else(|| "_".to_string());
            let status = object_value_any(
                values,
                &["previousstatus", "agentstatus", "status", "state"],
            )
            .and_then(Value::as_str)
            .map(normalized_ascii_identifier);
            if let Some(status) = status.as_deref() {
                tokens.push(format!("state:{identifier}:{status}"));
                if matches!(status, "message" | "partial")
                    && let Some(message) = object_value_any(values, &["message", "output", "text"])
                {
                    tokens.push(format!(
                        "message:{identifier}:{}",
                        hash_component(&canonical_json(message).to_string())
                    ));
                }
            }
            if object_value(values, "timedout").and_then(Value::as_bool) == Some(true) {
                tokens.push("timeout".to_string());
            }
            for (key, value) in values {
                let key = normalized_ascii_identifier(key);
                if matches!(
                    key.as_str(),
                    "completed" | "errored" | "failed" | "shutdown" | "notfound"
                ) && !matches!(value, Value::Bool(false) | Value::Null)
                {
                    tokens.push(format!("terminal:{identifier}:{key}"));
                }
                if protocol::is_agent_collection_field(&key)
                    || protocol::is_provider_envelope_field(&key)
                {
                    collect_status_progress_tokens(value, tokens, depth + 1);
                }
            }
        }
        Value::String(encoded) => {
            if let Ok(decoded) = serde_json::from_str::<Value>(encoded) {
                collect_status_progress_tokens(&decoded, tokens, depth + 1);
            }
        }
        _ => {}
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentListSnapshotState {
    AllChildrenTerminal,
    OnlyPendingInit,
    HasLiveChildren,
    Unknown,
}

fn reconcile_list_agents_response(
    input: &HookInput,
    state_root: &Path,
    runtime_id: &str,
    now_ms: u64,
) -> Result<bool> {
    if !list_agents_query_is_full(input.tool_input.as_ref()) {
        return Ok(false);
    }
    let snapshot = summarize_list_agents_response(input.tool_response.as_ref());
    if snapshot == AgentListSnapshotState::Unknown {
        return Ok(false);
    }

    if let Some(recovery) = crate::subagent_orchestrator::reconcile_pending_init_status_response(
        state_root,
        runtime_id,
        &input.session_id,
        input.tool_response.as_ref(),
        now_ms,
        PENDING_INIT_GRACE_MILLIS,
    )? {
        // Ledger-backed sessions keep the timer in the authoritative
        // reservation. Remove a pre-upgrade session-wide observation so Stop
        // cannot later fence healthy siblings with the legacy all-or-nothing
        // recovery path.
        remove_session_auxiliary_file(
            state_root,
            runtime_id,
            &input.session_id,
            PENDING_INIT_OBSERVED_FILE,
        )?;
        for agent_id_hash in &recovery.agent_id_hashes {
            remove_active_marker_by_hash(state_root, runtime_id, &input.session_id, agent_id_hash)?;
        }
        if snapshot == AgentListSnapshotState::AllChildrenTerminal {
            return Ok(true);
        }
        return Ok(false);
    }

    // Legacy marker-only sessions have no reversible identity mapping. Preserve
    // the conservative session-level fallback instead of guessing which opaque
    // marker belongs to a canonical task path.
    match snapshot {
        AgentListSnapshotState::AllChildrenTerminal => Ok(true),
        AgentListSnapshotState::OnlyPendingInit => {
            if observe_and_check_elapsed(
                state_root,
                runtime_id,
                &input.session_id,
                PENDING_INIT_OBSERVED_FILE,
                now_ms,
                PENDING_INIT_GRACE_MILLIS,
            )? {
                Ok(true)
            } else {
                Ok(false)
            }
        }
        AgentListSnapshotState::HasLiveChildren => {
            remove_session_auxiliary_file(
                state_root,
                runtime_id,
                &input.session_id,
                PENDING_INIT_OBSERVED_FILE,
            )?;
            Ok(false)
        }
        AgentListSnapshotState::Unknown => Ok(false),
    }
}

fn list_agents_query_is_full(tool_input: Option<&Value>) -> bool {
    match tool_input {
        None | Some(Value::Null) => true,
        Some(Value::Object(values)) if values.is_empty() => true,
        Some(Value::Object(values)) if values.len() == 1 => values.iter().all(|(key, value)| {
            normalized_ascii_identifier(key) == "pathprefix"
                && (matches!(value, Value::Null)
                    || value.as_str().is_some_and(|value| value.trim().is_empty()))
        }),
        Some(Value::Object(_)) => false,
        Some(Value::String(value)) => serde_json::from_str::<Value>(value)
            .ok()
            .as_ref()
            .is_some_and(|value| list_agents_query_is_full(Some(value))),
        Some(_) => false,
    }
}

fn summarize_list_agents_response(tool_response: Option<&Value>) -> AgentListSnapshotState {
    tool_response
        .and_then(summarize_agents_response_value)
        .unwrap_or(AgentListSnapshotState::Unknown)
}

fn summarize_agents_response_value(value: &Value) -> Option<AgentListSnapshotState> {
    match value {
        Value::Object(values) => {
            if let Some(Value::Array(agents)) =
                object_value_any(values, &["agents", "subagents", "children"])
            {
                return Some(summarize_agents(agents));
            }
            values.iter().find_map(|(key, value)| {
                protocol::is_provider_envelope_field(key)
                    .then(|| summarize_agents_response_value(value))
                    .flatten()
            })
        }
        Value::Array(values) => Some(summarize_agents(values)),
        Value::String(value) => {
            let parsed = serde_json::from_str::<Value>(value).ok()?;
            summarize_agents_response_value(&parsed)
        }
        _ => None,
    }
}

fn summarize_agents(agents: &[Value]) -> AgentListSnapshotState {
    let mut pending_init = 0;
    let mut live = 0;
    let mut unknown = 0;
    for agent in agents {
        let Value::Object(agent) = agent else {
            unknown += 1;
            continue;
        };
        let agent_name =
            object_value_any(agent, &["agentname", "taskname", "name"]).and_then(Value::as_str);
        if agent_name.is_some_and(is_root_agent_name) {
            continue;
        }
        let Some(status) = object_value_any(agent, &["agentstatus", "status", "state"]) else {
            unknown += 1;
            continue;
        };
        match classify_agent_status(status) {
            ObservedAgentState::PendingInit => pending_init += 1,
            ObservedAgentState::Live => live += 1,
            ObservedAgentState::Terminal => {}
            ObservedAgentState::Unknown => unknown += 1,
        }
    }
    if unknown > 0 {
        AgentListSnapshotState::Unknown
    } else if pending_init == 0 && live == 0 {
        AgentListSnapshotState::AllChildrenTerminal
    } else if pending_init > 0 && live == 0 {
        AgentListSnapshotState::OnlyPendingInit
    } else {
        AgentListSnapshotState::HasLiveChildren
    }
}

fn object_value<'a>(values: &'a Map<String, Value>, normalized_key: &str) -> Option<&'a Value> {
    values.iter().find_map(|(key, value)| {
        (normalized_ascii_identifier(key) == normalized_key).then_some(value)
    })
}

fn object_value_any<'a>(
    values: &'a Map<String, Value>,
    normalized_keys: &[&str],
) -> Option<&'a Value> {
    normalized_keys
        .iter()
        .find_map(|key| object_value(values, key))
}

fn is_root_agent_name(value: &str) -> bool {
    matches!(value.trim().trim_end_matches('/'), "root" | "/root")
}

fn classify_agent_status(value: &Value) -> ObservedAgentState {
    protocol::classify_agent_status(value)
}

fn remove_completed_agents_from_wait_response(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    tool_response: Option<&Value>,
) -> Result<()> {
    let Some(tool_response) = tool_response else {
        return Ok(());
    };
    let mut completed_agent_ids = Vec::new();
    collect_completed_agent_ids(tool_response, &mut completed_agent_ids);
    completed_agent_ids.sort();
    completed_agent_ids.dedup();
    for agent_id in completed_agent_ids {
        remove_active_marker(state_root, runtime_id, session_id, &agent_id)?;
    }
    Ok(())
}

fn collect_completed_agent_ids(value: &Value, completed_agent_ids: &mut Vec<String>) {
    protocol::collect_terminal_agent_ids(value, completed_agent_ids);
}

fn object_reports_agent_completion(values: &Map<String, Value>) -> bool {
    protocol::object_has_terminal_status(values)
}

fn normalized_ascii_identifier(value: &str) -> String {
    protocol::normalize_identifier(value)
}

fn verified_local_read_only_active_count(
    state_root: &Path,
    runtime_id: &str,
    session_id: &str,
    now_ms: u64,
) -> Result<Option<usize>> {
    let marker_hashes = active_marker_hashes_for_runtime(state_root, runtime_id, session_id)?;
    crate::subagent_orchestrator::verified_local_read_only_active_count(
        state_root,
        runtime_id,
        session_id,
        &marker_hashes,
        now_ms,
    )
}

fn is_root_local_read_tool(tool_name: &str) -> bool {
    matches!(
        tool_name.trim().to_ascii_lowercase().as_str(),
        "mcp__codey_fastctx__inspect_local_file"
            | "mcp__codey_fastctx__grep"
            | "mcp__codey_fastctx__glob"
    )
}

fn is_collaboration_tool(tool_name: &str) -> bool {
    matches!(
        normalized_collaboration_tool(tool_name).as_str(),
        "agent"
            | "spawn_agent"
            | "wait_agent"
            | "list_agents"
            | "interrupt_agent"
            | "send_message"
            | "followup_task"
    )
}

fn is_batch_decision_tool(tool_name: &str) -> bool {
    let normalized = tool_name.trim().to_ascii_lowercase();
    normalized == crate::subagent_control_mcp::QUALIFIED_TOOL_NAME
        || normalized == "codey_subagent_control.resolve_batch"
        || normalized == "codey_subagent_control/resolve_batch"
}

fn is_prepare_delegation_tool(tool_name: &str) -> bool {
    let normalized = tool_name.trim().to_ascii_lowercase();
    normalized == crate::subagent_control_mcp::PREPARE_DELEGATION_QUALIFIED_TOOL_NAME
        || normalized == "codey_subagent_control.prepare_delegation"
        || normalized == "codey_subagent_control/prepare_delegation"
}

fn is_wait_agent_tool(tool_name: &str) -> bool {
    normalized_collaboration_tool(tool_name) == "wait_agent"
}

fn is_list_agents_tool(tool_name: &str) -> bool {
    normalized_collaboration_tool(tool_name) == "list_agents"
}

fn is_interrupt_agent_tool(tool_name: &str) -> bool {
    normalized_collaboration_tool(tool_name) == "interrupt_agent"
}

fn is_agent_status_tool(tool_name: &str) -> bool {
    is_wait_agent_tool(tool_name) || is_list_agents_tool(tool_name)
}

fn is_anonymous_reconciliation_tool(tool_name: &str, tool_input: Option<&Value>) -> bool {
    is_wait_agent_tool(tool_name)
        || (is_list_agents_tool(tool_name) && list_agents_query_is_full(tool_input))
}

fn is_contract_spawn_tool(tool_name: &str) -> bool {
    normalized_collaboration_tool(tool_name) == "spawn_agent"
}

fn is_followup_task_tool(tool_name: &str) -> bool {
    normalized_collaboration_tool(tool_name) == "followup_task"
}

fn normalized_collaboration_tool(tool_name: &str) -> String {
    crate::subagent::rules::normalize_tool_name(tool_name)
}

fn input_has_subagent_context(input: &HookInput) -> bool {
    nonempty(input.agent_id.as_deref()).is_some() || nonempty(input.agent_type.as_deref()).is_some()
}
fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn quote_posix(path: &Path) -> String {
    let raw_path = path.to_string_lossy();
    #[cfg(windows)]
    let path = windows_path_to_wsl(&raw_path).unwrap_or_else(|| raw_path.into_owned());
    #[cfg(not(windows))]
    let path = raw_path.into_owned();
    format!("'{}'", path.replace('\'', "'\"'\"'"))
}

#[cfg(any(windows, test))]
fn windows_path_to_wsl(path: &str) -> Option<String> {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    let bytes = path.as_bytes();
    if bytes.len() < 3
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || !matches!(bytes[2], b'\\' | b'/')
    {
        return None;
    }
    Some(format!(
        "/mnt/{}/{}",
        (bytes[0] as char).to_ascii_lowercase(),
        path[3..].replace('\\', "/")
    ))
}

fn powershell_executable_invocation(path: &Path) -> String {
    format!("& '{}'", path.to_string_lossy().replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(event: &str, session: &str) -> HookInput {
        HookInput {
            hook_event_name: event.to_string(),
            session_id: session.to_string(),
            agent_id: None,
            agent_type: None,
            tool_name: None,
            tool_input: None,
            tool_response: None,
            turn_id: None,
            prompt: None,
            transcript_path: None,
            agent_transcript_path: None,
            cwd: None,
        }
    }

    #[test]
    fn child_tools_require_turn_context_model_attestation_and_cache_success() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let state_root = home.join(STATE_DIRECTORY);
        let sessions = home.join("sessions/2026/08/21");
        fs::create_dir_all(&sessions).unwrap();
        let roles = crate::config::default_subagent_roles();
        let hashes = BTreeMap::new();
        write_runtime_subagent_policy(home, &roles, &hashes).unwrap();
        let role = crate::config::SUBAGENT_ROLE_QUICK_SCAN;
        let expected = roles.get(role).unwrap();
        let session_id = "attestation-parent";
        let runtime_id = "runtime-attestation";
        let turn_id = "child-turn-1";

        let write_transcript = |agent_id: &str, model: Option<&str>, effort: Option<&str>| {
            let path = sessions.join(format!("rollout-probe-{agent_id}.jsonl"));
            let mut records = vec![json!({
                "type": "session_meta",
                "payload": {"id": agent_id, "parent_thread_id": session_id}
            })];
            if let (Some(model), Some(effort)) = (model, effort) {
                records.push(json!({
                    "type": "turn_context",
                    "payload": {"turn_id": turn_id, "model": model, "effort": effort}
                }));
            }
            fs::write(
                &path,
                records
                    .into_iter()
                    .map(|record| format!("{}\n", serde_json::to_string(&record).unwrap()))
                    .collect::<String>(),
            )
            .unwrap();
            path
        };
        let child_input = |agent_id: &str, transcript: &Path| {
            let mut child = input("PreToolUse", session_id);
            child.agent_id = Some(agent_id.to_string());
            child.agent_type = Some(role.to_string());
            child.turn_id = Some(turn_id.to_string());
            child.transcript_path = Some(transcript.to_string_lossy().into_owned());
            child.tool_name = Some("mcp__codey_fastctx__glob".to_string());
            child
        };

        let good_agent = "01a01f94-0000-7000-8000-000000000001";
        let good_transcript = write_transcript(
            good_agent,
            Some(expected.model.as_str()),
            Some(expected.reasoning_effort.as_str()),
        );
        let good = child_input(good_agent, &good_transcript);
        assert_eq!(
            runtime_subagent_attestation_denial(&good, &state_root, runtime_id).unwrap(),
            None
        );

        begin_runtime_subagent_policy_update(home, &roles, &hashes).unwrap();
        // Already-attested children may finish their existing turn while new
        // children are fenced until the pending generation is committed.
        assert_eq!(
            runtime_subagent_attestation_denial(&good, &state_root, runtime_id).unwrap(),
            None
        );
        let pending_agent = "01a01f94-0000-7000-8000-000000000002";
        let pending_transcript = write_transcript(
            pending_agent,
            Some(expected.model.as_str()),
            Some(expected.reasoning_effort.as_str()),
        );
        let pending = runtime_subagent_attestation_denial(
            &child_input(pending_agent, &pending_transcript),
            &state_root,
            runtime_id,
        )
        .unwrap()
        .unwrap();
        assert!(pending.contains("CODEY_SUBAGENT_RUNTIME_UPDATE_IN_PROGRESS"));
        commit_runtime_subagent_policy(home, &roles, &hashes).unwrap();

        let wrong_agent = "01a01f94-0000-7000-8000-000000000003";
        let wrong_transcript = write_transcript(
            wrong_agent,
            Some("provider-wrong-model"),
            Some(expected.reasoning_effort.as_str()),
        );
        let mismatch = runtime_subagent_attestation_denial(
            &child_input(wrong_agent, &wrong_transcript),
            &state_root,
            runtime_id,
        )
        .unwrap()
        .unwrap();
        assert!(mismatch.contains("CODEY_SUBAGENT_RUNTIME_CONFIG_MISMATCH"));
        assert!(mismatch.contains("provider-wrong-model"));

        let missing_agent = "01a01f94-0000-7000-8000-000000000004";
        let missing_transcript = write_transcript(missing_agent, None, None);
        let unverified = runtime_subagent_attestation_denial(
            &child_input(missing_agent, &missing_transcript),
            &state_root,
            runtime_id,
        )
        .unwrap()
        .unwrap();
        assert!(unverified.contains("CODEY_SUBAGENT_RUNTIME_UNVERIFIED"));
    }

    #[test]
    fn disabled_runtime_role_is_rejected_before_sidecar_or_spawn_reservation() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let state_root = home.join(STATE_DIRECTORY);
        let mut roles = crate::config::default_subagent_roles();
        roles.remove(crate::config::SUBAGENT_ROLE_WORKER);
        write_runtime_subagent_policy(home, &roles, &BTreeMap::new()).unwrap();

        let mut prepare = input("PreToolUse", "disabled-role-session");
        prepare.turn_id = Some("root-turn-a".to_string());
        prepare.tool_name =
            Some(crate::subagent_control_mcp::PREPARE_DELEGATION_QUALIFIED_TOOL_NAME.to_string());
        prepare.tool_input = Some(json!({
            "task_name": "disabled_worker",
            "agent_type": "codey_worker",
            "preparation_id": "disabled-worker-preparation",
            "contract": {
                "id": "disabled_worker",
                "root": "/repo"
            }
        }));
        let denied = handle_hook_for_runtime_at(&prepare, &state_root, "runtime-a", 10).unwrap();
        assert_eq!(
            denied["hookSpecificOutput"]["permissionDecision"].as_str(),
            Some("deny")
        );
        assert!(
            denied["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("CODEY_SUBAGENT_ROLE_DISABLED"))
        );

        let mut spawn = input("PreToolUse", "disabled-role-session");
        spawn.turn_id = Some("root-turn-a".to_string());
        spawn.cwd = Some("/repo".to_string());
        spawn.tool_name = Some("agents.spawn_agent".to_string());
        spawn.tool_input = Some(json!({
            "task_name": "disabled_worker",
            "agent_type": "codey_worker",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "disabled_worker",
                "why": "implementation",
                "visual": false,
                "root": "/repo",
                "read": [],
                "write": ["backend/src"],
                "checks": [{ "id": "tests", "cmd": "cargo test --lib" }]
            }))
        }));
        let denied = handle_hook_for_runtime_at(&spawn, &state_root, "runtime-a", 20).unwrap();
        assert_eq!(
            denied["hookSpecificOutput"]["permissionDecision"].as_str(),
            Some("deny")
        );
        assert!(
            denied["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("未创建调度账本记录"))
        );
        assert_eq!(
            crate::subagent_orchestrator::active_reservation_count(
                &state_root,
                "runtime-a",
                "disabled-role-session",
                30,
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn hook_input_accepts_common_camel_case_and_subagent_aliases() {
        let input: HookInput = serde_json::from_value(json!({
            "hookEventName": "PreToolUse",
            "sessionId": "session-a",
            "subagentId": "agent-a",
            "agentType": "codey_quick_scan",
            "toolName": "Bash",
            "toolInput": { "command": "true" },
            "toolResponse": { "exitCode": 0 },
            "turnId": "turn-root-a",
            "transcriptPath": "/tmp/root.jsonl",
            "agentTranscriptPath": "/tmp/child.jsonl",
            "workingDirectory": "/repo"
        }))
        .unwrap();

        assert_eq!(input.hook_event_name, "PreToolUse");
        assert_eq!(input.session_id, "session-a");
        assert_eq!(input.agent_id.as_deref(), Some("agent-a"));
        assert_eq!(input.agent_type.as_deref(), Some("codey_quick_scan"));
        assert_eq!(input.tool_name.as_deref(), Some("Bash"));
        assert_eq!(input.turn_id.as_deref(), Some("turn-root-a"));
        assert_eq!(input.transcript_path.as_deref(), Some("/tmp/root.jsonl"));
        assert_eq!(
            input.agent_transcript_path.as_deref(),
            Some("/tmp/child.jsonl")
        );
        assert_eq!(input.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn spawn_hook_does_not_treat_cwd_as_codex_permission_allowlist() {
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path().join("codey-subagent-gate-v3");
        let workspace = temp.path().join("current-workspace");
        let sibling_worktree = temp.path().join("sibling-worktree");
        std::fs::create_dir_all(&state_root).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(sibling_worktree.join("backend/src")).unwrap();

        let mut spawn = input("PreToolUse", "external-worktree-session");
        spawn.cwd = Some(workspace.to_string_lossy().into_owned());
        spawn.tool_name = Some("agents.spawn_agent".to_string());
        spawn.tool_input = Some(json!({
            "task_name": "sibling_reader",
            "agent_type": "codey_deep_research",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "sibling_reader",
                "why": "inspect_sibling_worktree",
                "visual": false,
                "root": sibling_worktree.to_string_lossy(),
                "read": ["backend/src"],
                "write": [],
                "checks": []
            }))
        }));

        // Codey validates the explicit contract. Codex's inherited sandbox and
        // approval layer remains responsible for the actual filesystem access.
        assert_eq!(handle_hook(&spawn, &state_root).unwrap(), json!({}));
    }

    #[test]
    fn spawn_task_receipt_binds_child_while_codex_controls_read_paths() {
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path().join("codey-subagent-gate-v3");
        std::fs::create_dir_all(&state_root).unwrap();
        let workspace = temp.path().join("workspace");
        let scope = workspace.join("scope");
        let sibling_worktree = workspace.join(".worktrees/sibling");
        std::fs::create_dir_all(&scope).unwrap();
        std::fs::create_dir_all(sibling_worktree.join("scope")).unwrap();
        let workspace = workspace.to_string_lossy().into_owned();
        let scope = scope.to_string_lossy().into_owned();
        let sibling_worktree = sibling_worktree.to_string_lossy().into_owned();
        let session_id = "task-receipt-session";

        let mut spawn = input("PreToolUse", session_id);
        spawn.turn_id = Some("root-turn-a".to_string());
        spawn.cwd = Some(workspace.clone());
        spawn.tool_name = Some("agents.spawn_agent".to_string());
        spawn.tool_input = Some(json!({
            "task_name": "receipt_reader",
            "agent_type": "codey_quick_scan",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "receipt_reader",
                "why": "independent_review",
                "visual": false,
                "root": workspace.clone(),
                "read": [scope.clone()],
                "write": [],
                "checks": []
            }))
        }));
        assert_eq!(handle_hook(&spawn, &state_root).unwrap(), json!({}));

        let task_path = "/root/receipt_reader";
        let mut spawned = input("PostToolUse", session_id);
        spawned.tool_name = spawn.tool_name.clone();
        spawned.tool_input = spawn.tool_input.clone();
        spawned.tool_response = Some(Value::String(
            serde_json::to_string(&json!({ "task_name": task_path })).unwrap(),
        ));
        assert_eq!(handle_hook(&spawned, &state_root).unwrap(), json!({}));

        let agent_id = "01a01d5b-1d06-7383-b333-80e54467508e";
        let transcript = temp
            .path()
            .join("sessions/2026/08/20")
            .join(format!("rollout-probe-{agent_id}.jsonl"));
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(
            &transcript,
            format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "type": "session_meta",
                    "payload": {
                        "id": agent_id,
                        "parent_thread_id": session_id,
                        "agent_path": task_path,
                        "agent_role": "codey_quick_scan",
                        "source": {
                            "subagent": {
                                "thread_spawn": {
                                    "parent_thread_id": session_id,
                                    "agent_path": task_path,
                                    "agent_role": "codey_quick_scan"
                                }
                            }
                        }
                    }
                }))
                .unwrap()
            ),
        )
        .unwrap();

        let mut started = input("SubagentStart", session_id);
        started.agent_id = Some(agent_id.to_string());
        started.agent_type = Some("codey_quick_scan".to_string());
        started.transcript_path = Some(transcript.to_string_lossy().into_owned());
        assert_eq!(handle_hook(&started, &state_root).unwrap(), json!({}));

        // Codex returns the canonical task path from spawn, but exposes the
        // opaque child thread id to lifecycle/tool hooks. The child transcript
        // metadata is the provider-owned bridge between those identities.
        let mut first_read = input("PreToolUse", session_id);
        first_read.agent_id = Some(agent_id.to_string());
        first_read.agent_type = Some("codey_quick_scan".to_string());
        first_read.transcript_path = Some(transcript.to_string_lossy().into_owned());
        first_read.cwd = Some(workspace);
        first_read.tool_name = Some("mcp__codey_fastctx__glob".to_string());
        first_read.tool_input = Some(json!({ "path": "scope", "pattern": ["**/*.rs"] }));
        assert_eq!(handle_hook(&first_read, &state_root).unwrap(), json!({}));

        let mut sibling_read = first_read;
        sibling_read.cwd = Some(sibling_worktree);
        assert_eq!(handle_hook(&sibling_read, &state_root).unwrap(), json!({}));
    }

    #[test]
    fn transcript_identity_correlation_rejects_a_wrong_parent_session() {
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path().join("codey-subagent-gate-v3");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&state_root).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let workspace = workspace.to_string_lossy().into_owned();
        let session_id = "expected-parent-session";

        let mut spawn = input("PreToolUse", session_id);
        spawn.cwd = Some(workspace.clone());
        spawn.tool_name = Some("agents.spawn_agent".to_string());
        spawn.tool_input = Some(json!({
            "task_name": "spoof_reader",
            "agent_type": "codey_quick_scan",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "spoof_reader",
                "why": "independent_review",
                "visual": false,
                "root": workspace,
                "read": [],
                "write": [],
                "checks": []
            }))
        }));
        assert_eq!(handle_hook(&spawn, &state_root).unwrap(), json!({}));

        let mut spawned = input("PostToolUse", session_id);
        spawned.tool_name = spawn.tool_name.clone();
        spawned.tool_input = spawn.tool_input.clone();
        spawned.tool_response = Some(Value::String(
            r#"{"task_name":"/root/spoof_reader"}"#.to_string(),
        ));
        assert_eq!(handle_hook(&spawned, &state_root).unwrap(), json!({}));

        let agent_id = "01a01d5b-dead-beef-baad-000000000001";
        let transcript = temp
            .path()
            .join("sessions/2026/08/20")
            .join(format!("rollout-probe-{agent_id}.jsonl"));
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(
            &transcript,
            format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "type": "session_meta",
                    "payload": {
                        "id": agent_id,
                        "parent_thread_id": "different-parent-session",
                        "agent_path": "/root/spoof_reader",
                        "agent_role": "codey_quick_scan"
                    }
                }))
                .unwrap()
            ),
        )
        .unwrap();

        let mut first_read = input("PreToolUse", session_id);
        first_read.agent_id = Some(agent_id.to_string());
        first_read.agent_type = Some("codey_quick_scan".to_string());
        first_read.transcript_path = Some(transcript.to_string_lossy().into_owned());
        first_read.tool_name = Some("mcp__codey_fastctx__glob".to_string());
        first_read.tool_input = Some(json!({ "path": workspace, "pattern": ["**/*.rs"] }));
        let denied = handle_hook(&first_read, &state_root).unwrap();
        assert_eq!(
            denied["hookSpecificOutput"]["permissionDecision"].as_str(),
            Some("deny")
        );
        assert!(
            denied["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("CODEY_SUBAGENT_UNBOUND_ATTEMPT"))
        );
    }

    fn delegation_message(mut contract: Value) -> String {
        if let Some(values) = contract.as_object_mut()
            && !values.contains_key("capabilities")
        {
            let write_capable = values
                .get("write")
                .and_then(Value::as_array)
                .is_some_and(|paths| !paths.is_empty());
            values.insert(
                "capabilities".to_string(),
                if write_capable {
                    json!(["files.read", "workspace.write"])
                } else {
                    json!(["files.read"])
                },
            );
        }
        format!(
            "Do the bounded task.\n{}{}",
            crate::subagent_orchestrator::CONTRACT_PREFIX,
            serde_json::to_string(&contract).unwrap()
        )
    }

    #[test]
    fn runtime_gate_enforces_capabilities_and_mechanical_acceptance() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut spawn = input("PreToolUse", "contract-session");
        spawn.cwd = Some("/repo".to_string());
        spawn.tool_name = Some("agents.spawn_agent".to_string());
        spawn.tool_input = Some(json!({
            "task_name": "worker_a",
            "agent_type": "codey_worker",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "worker_a",
                "why": "independent_work",
                "visual": false,
                "root": "/repo",
                "read": [],
                "write": ["backend/src"],
                "checks": [{ "id": "tests", "cmd": "cargo test -p codey --lib" }]
            }))
        }));
        assert_eq!(handle_hook(&spawn, root).unwrap(), json!({}));

        let mut spawned = input("PostToolUse", "contract-session");
        spawned.tool_name = spawn.tool_name.clone();
        spawned.tool_input = spawn.tool_input.clone();
        spawned.tool_response = Some(json!({ "agent_id": "agent-a" }));
        assert_eq!(handle_hook(&spawned, root).unwrap(), json!({}));

        let mut started = input("SubagentStart", "contract-session");
        started.agent_id = Some("agent-a".to_string());
        handle_hook(&started, root).unwrap();

        let mut owned_patch = input("PreToolUse", "contract-session");
        owned_patch.agent_id = Some("agent-a".to_string());
        owned_patch.cwd = Some("/repo".to_string());
        owned_patch.tool_name = Some("apply_patch".to_string());
        owned_patch.tool_input = Some(json!({
            "patch": "*** Begin Patch\n*** Update File: backend/src/lib.rs\n*** End Patch"
        }));
        assert_eq!(handle_hook(&owned_patch, root).unwrap(), json!({}));

        let mut escaped_patch = owned_patch;
        escaped_patch.tool_input = Some(json!({
            "patch": "*** Begin Patch\n*** Update File: README.md\n*** End Patch"
        }));
        assert_eq!(handle_hook(&escaped_patch, root).unwrap(), json!({}));

        let mut stopped = input("SubagentStop", "contract-session");
        stopped.agent_id = Some("agent-a".to_string());
        handle_hook(&stopped, root).unwrap();
        let blocked = handle_hook(&input("Stop", "contract-session"), root).unwrap();
        assert_eq!(blocked["decision"].as_str(), Some("block"));
        assert!(
            blocked["reason"]
                .as_str()
                .unwrap()
                .contains("codey-accept:worker_a:tests")
        );

        let acceptance_command = "# codey-accept:worker_a:tests\ncargo test -p codey --lib";
        let mut pre_acceptance = input("PreToolUse", "contract-session");
        pre_acceptance.tool_name = Some("Bash".to_string());
        pre_acceptance.tool_input = Some(json!({ "command": acceptance_command }));
        assert_eq!(handle_hook(&pre_acceptance, root).unwrap(), json!({}));

        let mut post_acceptance = input("PostToolUse", "contract-session");
        post_acceptance.tool_name = pre_acceptance.tool_name;
        post_acceptance.tool_input = pre_acceptance.tool_input;
        post_acceptance.tool_response = Some(json!({ "exit_code": 0, "output": "ok" }));
        assert_eq!(handle_hook(&post_acceptance, root).unwrap(), json!({}));
        let decision_block = handle_hook(&input("Stop", "contract-session"), root).unwrap();
        assert_eq!(decision_block["decision"].as_str(), Some("block"));
        assert!(
            decision_block["reason"]
                .as_str()
                .unwrap()
                .contains(crate::subagent_control_mcp::QUALIFIED_TOOL_NAME)
        );

        let decision_input = json!({
            "decision": "complete",
            "batch_number": 1,
            "decision_id": "contract-session-complete",
            "reason": "implementation and acceptance are complete"
        });
        let mut pre_decision = input("PreToolUse", "contract-session");
        pre_decision.tool_name = Some(crate::subagent_control_mcp::QUALIFIED_TOOL_NAME.to_string());
        pre_decision.tool_input = Some(decision_input.clone());
        assert_eq!(handle_hook(&pre_decision, root).unwrap(), json!({}));

        let mut post_decision = input("PostToolUse", "contract-session");
        post_decision.tool_name = pre_decision.tool_name;
        post_decision.tool_input = Some(decision_input.clone());
        post_decision.tool_response = Some(json!({
            "structuredContent": {
                "accepted": true,
                "decision": "complete",
                "batch_number": 1,
                "decision_id": "contract-session-complete",
                "reason": "implementation and acceptance are complete"
            }
        }));
        assert_eq!(handle_hook(&post_decision, root).unwrap(), json!({}));
        assert_eq!(
            handle_hook(&input("Stop", "contract-session"), root).unwrap(),
            json!({})
        );
    }

    #[test]
    fn runtime_gate_bounds_missing_batch_control_tool_stop_retries() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let session_id = "missing-control-tool-session";
        let mut spawn = input("PreToolUse", session_id);
        spawn.cwd = Some("/repo".to_string());
        spawn.tool_name = Some("agents.spawn_agent".to_string());
        spawn.tool_input = Some(json!({
            "task_name": "research_a",
            "agent_type": "codey_deep_research",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "research_a",
                "why": "breadth",
                "visual": false,
                "read": [],
                "write": [],
                "checks": []
            }))
        }));
        assert_eq!(handle_hook(&spawn, root).unwrap(), json!({}));

        let mut spawned = input("PostToolUse", session_id);
        spawned.tool_name = spawn.tool_name.clone();
        spawned.tool_input = spawn.tool_input.clone();
        spawned.tool_response = Some(json!({ "agent_id": "agent-a" }));
        assert_eq!(handle_hook(&spawned, root).unwrap(), json!({}));

        let mut stopped = input("SubagentStop", session_id);
        stopped.agent_id = Some("agent-a".to_string());
        assert_eq!(handle_hook(&stopped, root).unwrap(), json!({}));

        for _ in 0..2 {
            let blocked = handle_hook(&input("Stop", session_id), root).unwrap();
            assert_eq!(blocked["decision"].as_str(), Some("block"));
            assert!(
                blocked["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("resolve_batch"))
            );
        }
        assert_eq!(
            handle_hook(&input("Stop", session_id), root).unwrap(),
            json!({})
        );
    }

    #[test]
    fn followup_task_rejects_unbound_or_terminal_targets_before_reactivation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let session_id = "followup-session";
        let mut followup = input("PreToolUse", session_id);
        followup.turn_id = Some("root-turn-a".to_string());
        followup.tool_name = Some("agents.followup_task".to_string());
        followup.tool_input = Some(json!({
            "target": "/root/followup_worker",
            "message": "continue the write task"
        }));

        let missing = handle_hook(&followup, root).unwrap();
        assert_eq!(
            missing["hookSpecificOutput"]["permissionDecision"].as_str(),
            Some("deny")
        );
        assert!(
            missing["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(
                    |reason| reason.contains("CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT")
                )
        );

        let mut spawn = input("PreToolUse", session_id);
        spawn.turn_id = Some("root-turn-a".to_string());
        spawn.cwd = Some("/repo".to_string());
        spawn.tool_name = Some("agents.spawn_agent".to_string());
        spawn.tool_input = Some(json!({
            "task_name": "followup_worker",
            "agent_type": "codey_worker",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "followup_worker",
                "why": "independent_work",
                "visual": false,
                "root": "/repo",
                "read": [],
                "write": ["backend/src"],
                "checks": [{ "id": "tests", "cmd": "cargo test -p codey --lib" }]
            }))
        }));
        assert_eq!(handle_hook(&spawn, root).unwrap(), json!({}));

        let agent_id = "/root/followup_worker";
        let mut spawned = input("PostToolUse", session_id);
        spawned.tool_name = spawn.tool_name.clone();
        spawned.tool_input = spawn.tool_input.clone();
        spawned.tool_response = Some(json!({ "task_name": agent_id }));
        assert_eq!(handle_hook(&spawned, root).unwrap(), json!({}));
        let mut started = input("SubagentStart", session_id);
        started.agent_id = Some(agent_id.to_string());
        started.agent_type = Some("codey_worker".to_string());
        assert_eq!(handle_hook(&started, root).unwrap(), json!({}));
        assert_eq!(handle_hook(&followup, root).unwrap(), json!({}));

        let mut stopped = input("SubagentStop", session_id);
        stopped.agent_id = Some(agent_id.to_string());
        handle_hook(&stopped, root).unwrap();
        let terminal = handle_hook(&followup, root).unwrap();
        assert_eq!(
            terminal["hookSpecificOutput"]["permissionDecision"].as_str(),
            Some("deny")
        );
        assert!(
            terminal["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("不要等待旧 canonical task 自行恢复"))
        );

        let mut unbound_write = input("PreToolUse", "unbound-child-session");
        unbound_write.agent_id = Some("/root/legacy-worker".to_string());
        unbound_write.agent_type = Some("codey_worker".to_string());
        unbound_write.tool_name = Some("apply_patch".to_string());
        unbound_write.tool_input = Some(json!({ "patch": "*** Begin Patch\n*** End Patch" }));
        let denied_write = handle_hook(&unbound_write, root).unwrap();
        assert!(
            denied_write["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("CODEY_SUBAGENT_UNBOUND_ATTEMPT")
                    && reason.contains("立即把该错误码返回主代理"))
        );
    }

    #[test]
    fn successful_root_interrupt_fences_the_attempt_and_releases_the_gate() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let session_id = "interrupt-abandon-session";
        let target = "/root/interrupt_reader";

        let mut spawn = input("PreToolUse", session_id);
        spawn.turn_id = Some("root-turn-a".to_string());
        spawn.cwd = Some("/repo".to_string());
        spawn.tool_name = Some("agents.spawn_agent".to_string());
        spawn.tool_input = Some(json!({
            "task_name": "interrupt_reader",
            "agent_type": "codey_deep_research",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "interrupt_reader",
                "why": "independent_review",
                "visual": false,
                "root": "/repo",
                "read": [],
                "write": [],
                "checks": []
            }))
        }));
        assert_eq!(
            handle_hook_for_runtime_at(&spawn, root, runtime_id, 10).unwrap(),
            json!({})
        );
        let mut spawned = input("PostToolUse", session_id);
        spawned.tool_name = spawn.tool_name.clone();
        spawned.tool_input = spawn.tool_input.clone();
        spawned.tool_response = Some(json!({ "agent_id": target }));
        handle_hook_for_runtime_at(&spawned, root, runtime_id, 20).unwrap();
        let mut started = input("SubagentStart", session_id);
        started.agent_id = Some(target.to_string());
        handle_hook_for_runtime_at(&started, root, runtime_id, 25).unwrap();
        let marker = agent_marker_path(&session_state_dir(root, session_id), runtime_id, target);
        assert!(marker.exists());

        let mut interrupt = input("PreToolUse", session_id);
        interrupt.turn_id = Some("root-turn-a".to_string());
        interrupt.tool_name = Some("agents.interrupt_agent".to_string());
        interrupt.tool_input = Some(json!({ "target": target }));
        assert_eq!(
            handle_hook_for_runtime_at(&interrupt, root, runtime_id, 30).unwrap(),
            json!({})
        );
        interrupt.hook_event_name = "PostToolUse".to_string();
        interrupt.tool_response = Some(json!({ "previous_status": "interrupted" }));
        let settled = handle_hook_for_runtime_at(&interrupt, root, runtime_id, 31).unwrap();
        assert_eq!(settled["decision"].as_str(), Some("block"));
        assert!(
            settled["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("resolve_batch"))
        );
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            0
        );
        assert!(!marker.exists());

        // The collaboration provider can publish a lagging snapshot after the
        // interrupt acknowledgement. It must not resurrect the fenced attempt
        // or send the root back into an endless wait loop.
        let mut stale_list = input("PostToolUse", session_id);
        stale_list.tool_name = Some("agents.list_agents".to_string());
        stale_list.tool_input = Some(json!({}));
        stale_list.tool_response = Some(json!({
            "agents": [
                { "agent_name": "/root", "status": "running" },
                { "agent_name": target, "status": "pending_init" }
            ]
        }));
        let stale_snapshot = handle_hook_for_runtime_at(&stale_list, root, runtime_id, 32).unwrap();
        assert_eq!(stale_snapshot["decision"].as_str(), Some("block"));
        assert!(
            stale_snapshot["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("resolve_batch"))
        );
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            0
        );

        let mut followup = input("PreToolUse", session_id);
        followup.turn_id = Some("root-turn-a".to_string());
        followup.tool_name = Some("agents.followup_task".to_string());
        followup.tool_input = Some(json!({ "target": target, "message": "resume" }));
        assert!(
            handle_hook_for_runtime_at(&followup, root, runtime_id, 33).unwrap()
                ["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| {
                    reason.contains("CODEY_SUBAGENT_FOLLOWUP_REQUIRES_ACTIVE_ATTEMPT")
                })
        );

        let mut late_stop = input("SubagentStop", session_id);
        late_stop.agent_id = Some(target.to_string());
        assert_eq!(
            handle_hook_for_runtime_at(&late_stop, root, runtime_id, 34).unwrap(),
            json!({})
        );
        assert_eq!(
            handle_hook_for_runtime_at(&late_stop, root, runtime_id, 35).unwrap(),
            json!({})
        );
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            0
        );
    }

    #[test]
    fn terminal_unknown_interrupt_ack_idempotently_reopens_batch_reconciliation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let session_id = "terminal-unknown-interrupt-session";
        let target = "/root/terminal_reader";

        let mut spawn = input("PreToolUse", session_id);
        spawn.turn_id = Some("root-turn-a".to_string());
        spawn.cwd = Some("/repo".to_string());
        spawn.tool_name = Some("agents.spawn_agent".to_string());
        spawn.tool_input = Some(json!({
            "task_name": "terminal_reader",
            "agent_type": "codey_deep_research",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "terminal_reader",
                "why": "independent_review",
                "visual": false,
                "root": "/repo",
                "read": [],
                "write": [],
                "checks": []
            }))
        }));
        handle_hook_for_runtime_at(&spawn, root, runtime_id, 10).unwrap();
        let mut spawned = input("PostToolUse", session_id);
        spawned.tool_name = spawn.tool_name.clone();
        spawned.tool_input = spawn.tool_input.clone();
        spawned.tool_response = Some(json!({ "agent_id": target }));
        handle_hook_for_runtime_at(&spawned, root, runtime_id, 20).unwrap();
        let mut started = input("SubagentStart", session_id);
        started.agent_id = Some(target.to_string());
        handle_hook_for_runtime_at(&started, root, runtime_id, 25).unwrap();

        let mut stopped = input("SubagentStop", session_id);
        stopped.agent_id = Some(target.to_string());
        handle_hook_for_runtime_at(&stopped, root, runtime_id, 30).unwrap();
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            0
        );

        // The provider may acknowledge a root interrupt after lifecycle Stop
        // already produced Terminal/Unknown. Cleanup and batch recomputation are
        // idempotent even though the lifecycle reservation no longer changes.
        let mut interrupt = input("PostToolUse", session_id);
        interrupt.turn_id = Some("root-turn-a".to_string());
        interrupt.tool_name = Some("agents.interrupt_agent".to_string());
        interrupt.tool_input = Some(json!({ "target": target }));
        interrupt.tool_response = Some(json!({ "previous_status": "pending_init" }));
        for now_ms in [31, 32] {
            let output = handle_hook_for_runtime_at(&interrupt, root, runtime_id, now_ms).unwrap();
            assert_eq!(output["decision"].as_str(), Some("block"));
            assert!(
                output["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("resolve_batch"))
            );
        }

        let mut stale_list = input("PostToolUse", session_id);
        stale_list.tool_name = Some("agents.list_agents".to_string());
        stale_list.tool_input = Some(json!({}));
        stale_list.tool_response = Some(json!({
            "agents": [
                { "agent_name": "/root", "status": "running" },
                { "agent_name": target, "status": "pending_init" }
            ]
        }));
        let stale = handle_hook_for_runtime_at(&stale_list, root, runtime_id, 33).unwrap();
        assert_eq!(stale["decision"].as_str(), Some("block"));
        assert!(
            stale["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("resolve_batch"))
        );
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            0
        );
    }

    #[test]
    fn runtime_change_reconciles_interrupted_tombstone_and_allows_batch_decision() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let old_runtime = "runtime-old";
        let new_runtime = "runtime-new";
        let session_id = "runtime-migration-interrupt-session";
        let target = "/root/migrated_reader";

        let mut spawn = input("PreToolUse", session_id);
        spawn.turn_id = Some("root-turn-old".to_string());
        spawn.cwd = Some("/repo".to_string());
        spawn.tool_name = Some("agents.spawn_agent".to_string());
        spawn.tool_input = Some(json!({
            "task_name": "migrated_reader",
            "agent_type": "codey_deep_research",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "migrated_reader",
                "why": "independent_review",
                "visual": false,
                "root": "/repo",
                "read": [],
                "write": [],
                "checks": []
            }))
        }));
        handle_hook_for_runtime_at(&spawn, root, old_runtime, 10).unwrap();
        let mut spawned = input("PostToolUse", session_id);
        spawned.tool_name = spawn.tool_name.clone();
        spawned.tool_input = spawn.tool_input.clone();
        spawned.tool_response = Some(json!({ "agent_id": target }));
        handle_hook_for_runtime_at(&spawned, root, old_runtime, 20).unwrap();
        let mut started = input("SubagentStart", session_id);
        started.agent_id = Some(target.to_string());
        handle_hook_for_runtime_at(&started, root, old_runtime, 25).unwrap();

        let session_dir = session_state_dir(root, session_id);
        let old_marker = agent_marker_path(&session_dir, old_runtime, target);
        let new_marker = agent_marker_path(&session_dir, new_runtime, target);
        assert!(old_marker.exists());

        let mut interrupted_list = input("PostToolUse", session_id);
        interrupted_list.tool_name = Some("agents.list_agents".to_string());
        interrupted_list.tool_input = Some(json!({}));
        interrupted_list.tool_response = Some(json!({
            "agents": [
                { "agent_name": "/root", "status": "running" },
                { "agent_name": target, "status": "interrupted" }
            ]
        }));
        let migrated =
            handle_hook_for_runtime_at(&interrupted_list, root, new_runtime, 30).unwrap();
        assert_eq!(migrated["decision"].as_str(), Some("block"));
        assert!(
            migrated["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("resolve_batch"))
        );
        assert!(!old_marker.exists());
        assert!(!new_marker.exists());
        assert_eq!(
            active_agent_count_for_runtime(root, new_runtime, session_id).unwrap(),
            0
        );

        // A late hook from the retired runtime cannot migrate the ledger back
        // or recreate a marker under either generation.
        let stale_error = handle_hook_for_runtime_at(&started, root, old_runtime, 31).unwrap_err();
        assert!(format!("{stale_error:#}").contains("CODEY_SUBAGENT_STALE_RUNTIME_EVENT"));
        assert!(!old_marker.exists());
        assert!(!new_marker.exists());
        assert_eq!(
            handle_hook_for_runtime_at(&input("SessionEnd", session_id), root, old_runtime, 31,)
                .unwrap(),
            json!({})
        );
        assert_eq!(
            active_agent_count_for_runtime(root, new_runtime, session_id).unwrap(),
            0
        );

        let mut interrupt = input("PostToolUse", session_id);
        interrupt.tool_name = Some("agents.interrupt_agent".to_string());
        interrupt.tool_input = Some(json!({ "target": target }));
        interrupt.tool_response = Some(json!({ "previous_status": "interrupted" }));
        let reconciled = handle_hook_for_runtime_at(&interrupt, root, new_runtime, 32).unwrap();
        assert_eq!(reconciled["decision"].as_str(), Some("block"));
        assert!(
            reconciled["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("resolve_batch"))
        );

        interrupted_list.tool_response = Some(json!({
            "agents": [
                { "agent_name": "/root", "status": "running" },
                { "agent_name": target, "status": "pending_init" }
            ]
        }));
        let lagging = handle_hook_for_runtime_at(&interrupted_list, root, new_runtime, 33).unwrap();
        assert_eq!(lagging["decision"].as_str(), Some("block"));
        assert!(
            lagging["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("resolve_batch"))
        );

        let decision = json!({
            "decision": "complete",
            "batch_number": 1,
            "decision_id": "runtime-migration-complete",
            "reason": "the migrated attempt is permanently fenced"
        });
        let mut pre_decision = input("PreToolUse", session_id);
        pre_decision.tool_name = Some(crate::subagent_control_mcp::QUALIFIED_TOOL_NAME.to_string());
        pre_decision.tool_input = Some(decision.clone());
        assert_eq!(
            handle_hook_for_runtime_at(&pre_decision, root, new_runtime, 34).unwrap(),
            json!({})
        );

        let mut post_decision = input("PostToolUse", session_id);
        post_decision.tool_name = pre_decision.tool_name;
        post_decision.tool_input = Some(decision);
        post_decision.tool_response = Some(json!({
            "structuredContent": {
                "accepted": true,
                "decision": "complete",
                "batch_number": 1,
                "decision_id": "runtime-migration-complete",
                "reason": "the migrated attempt is permanently fenced"
            }
        }));
        assert_eq!(
            handle_hook_for_runtime_at(&post_decision, root, new_runtime, 35).unwrap(),
            json!({})
        );
        assert_eq!(
            handle_hook_for_runtime_at(&input("Stop", session_id), root, new_runtime, 36).unwrap(),
            json!({})
        );
    }

    #[test]
    fn interrupt_after_target_completion_preserves_success_instead_of_abandoning_it() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let session_id = "interrupt-after-completion";
        let target = "/root/completed_reader";

        let mut spawn = input("PreToolUse", session_id);
        spawn.turn_id = Some("root-turn-a".to_string());
        spawn.tool_name = Some("agents.spawn_agent".to_string());
        spawn.tool_input = Some(json!({
            "task_name": "completed_reader",
            "agent_type": "codey_deep_research",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "completed_reader",
                "why": "independent_review",
                "visual": false,
                "read": [],
                "write": [],
                "checks": []
            }))
        }));
        handle_hook_for_runtime_at(&spawn, root, runtime_id, 10).unwrap();
        let mut spawned = input("PostToolUse", session_id);
        spawned.tool_name = spawn.tool_name.clone();
        spawned.tool_input = spawn.tool_input.clone();
        spawned.tool_response = Some(json!({ "agent_id": target }));
        handle_hook_for_runtime_at(&spawned, root, runtime_id, 20).unwrap();
        let mut started = input("SubagentStart", session_id);
        started.agent_id = Some(target.to_string());
        handle_hook_for_runtime_at(&started, root, runtime_id, 25).unwrap();

        let mut interrupt = input("PostToolUse", session_id);
        interrupt.turn_id = Some("root-turn-a".to_string());
        interrupt.tool_name = Some("agents.interrupt_agent".to_string());
        interrupt.tool_input = Some(json!({ "target": target }));
        interrupt.tool_response = Some(json!({
            "agent_id": target,
            "previous_status": "completed"
        }));
        let settled = handle_hook_for_runtime_at(&interrupt, root, runtime_id, 30).unwrap();
        assert_eq!(settled["decision"].as_str(), Some("block"));
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            0
        );

        let events = std::fs::read_to_string(crate::subagent::telemetry::trace_file(root)).unwrap();
        let terminal = events
            .lines()
            .map(|line| {
                serde_json::from_str::<crate::subagent::telemetry::SubagentTraceEvent>(line)
                    .unwrap()
            })
            .find(|event| event.timestamp_ms == 30)
            .unwrap();
        assert_eq!(
            terminal.event,
            crate::subagent::telemetry::TraceEventKind::Completed
        );
        assert_eq!(
            terminal.status,
            crate::subagent::telemetry::ExecutionStatus::Succeeded
        );
        assert_eq!(terminal.error_code, None);
    }

    #[test]
    fn failed_or_unmatched_interrupt_does_not_release_an_active_attempt() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let session_id = "failed-interrupt-session";
        let target = "/root/active_reader";

        let mut spawn = input("PreToolUse", session_id);
        spawn.turn_id = Some("root-turn-a".to_string());
        spawn.cwd = Some("/repo".to_string());
        spawn.tool_name = Some("agents.spawn_agent".to_string());
        spawn.tool_input = Some(json!({
            "task_name": "active_reader",
            "agent_type": "codey_deep_research",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "active_reader",
                "why": "independent_review",
                "visual": false,
                "root": "/repo",
                "read": [],
                "write": [],
                "checks": []
            }))
        }));
        handle_hook_for_runtime_at(&spawn, root, runtime_id, 10).unwrap();
        let mut spawned = input("PostToolUse", session_id);
        spawned.tool_name = spawn.tool_name.clone();
        spawned.tool_input = spawn.tool_input.clone();
        spawned.tool_response = Some(json!({ "agent_id": target }));
        handle_hook_for_runtime_at(&spawned, root, runtime_id, 20).unwrap();

        let mut interrupt = input("PostToolUse", session_id);
        interrupt.tool_name = Some("agents.interrupt_agent".to_string());
        interrupt.tool_input = Some(json!({ "target": target }));
        interrupt.tool_response = Some(json!({
            "isError": true,
            "error": "interrupt transport failed",
            "previous_status": "running"
        }));
        assert_eq!(
            handle_hook_for_runtime_at(&interrupt, root, runtime_id, 30).unwrap(),
            json!({})
        );
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            1
        );

        interrupt.tool_response = Some(json!({
            "agent_id": "/root/different_reader",
            "previous_status": "running"
        }));
        assert_eq!(
            handle_hook_for_runtime_at(&interrupt, root, runtime_id, 31).unwrap(),
            json!({})
        );
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            1
        );

        interrupt.tool_input = Some(json!({ "target": "/root/unknown_reader" }));
        interrupt.tool_response = Some(json!({ "previous_status": "interrupted" }));
        assert_eq!(
            handle_hook_for_runtime_at(&interrupt, root, runtime_id, 32).unwrap(),
            json!({})
        );
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            1
        );
    }

    #[test]
    fn runtime_gate_keeps_encrypted_spawns_read_only() {
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path();
        let workspace = state_root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let workspace = workspace.to_string_lossy().into_owned();

        let mut spawn = input("PreToolUse", "encrypted-contract-session");
        spawn.turn_id = Some("root-turn-encrypted".to_string());
        spawn.cwd = Some(workspace.clone());
        spawn.tool_name = Some("agents.spawn_agent".to_string());
        spawn.tool_input = Some(json!({
            "task_name": "encrypted_worker",
            "agent_type": "codey_worker",
            "fork_turns": "none",
            "message": format!("gAAAAA{}", "A".repeat(160))
        }));
        let denied = handle_hook(&spawn, state_root).unwrap();
        assert_eq!(
            denied["hookSpecificOutput"]["permissionDecision"].as_str(),
            Some("deny")
        );
        assert!(
            denied["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("缺少可验证 sidecar"))
        );

        spawn.tool_input = Some(json!({
            "task_name": "encrypted_reader",
            "agent_type": "codey_quick_scan",
            "fork_turns": "none",
            "message": format!("gAAAAA{}", "A".repeat(160))
        }));
        assert_eq!(handle_hook(&spawn, state_root).unwrap(), json!({}));

        let mut spawned = input("PostToolUse", "encrypted-contract-session");
        spawned.tool_name = spawn.tool_name.clone();
        spawned.tool_input = spawn.tool_input.clone();
        spawned.tool_response = Some(json!({ "task_name": "/root/encrypted_reader" }));
        handle_hook(&spawned, state_root).unwrap();

        let mut started = input("SubagentStart", "encrypted-contract-session");
        started.agent_id = Some("/root/encrypted_reader".to_string());
        handle_hook(&started, state_root).unwrap();

        let mut owned_patch = input("PreToolUse", "encrypted-contract-session");
        owned_patch.agent_id = Some("/root/encrypted_reader".to_string());
        owned_patch.cwd = Some(workspace);
        owned_patch.tool_name = Some("apply_patch".to_string());
        owned_patch.tool_input = Some(json!({
            "patch": "*** Begin Patch\n*** Update File: src/lib.rs\n*** End Patch"
        }));
        assert_eq!(
            handle_hook(&owned_patch, state_root).unwrap()["hookSpecificOutput"]
                ["permissionDecision"]
                .as_str(),
            Some("deny")
        );

        let mut local_read = owned_patch;
        local_read.tool_name = Some("mcp__codey_fastctx__grep".to_string());
        local_read.tool_input = Some(json!({ "path": "src" }));
        assert_eq!(handle_hook(&local_read, state_root).unwrap(), json!({}));

        let mut external_read = local_read;
        external_read.tool_name = Some("mcp__codey_fastctx__inspect_local_file".to_string());
        external_read.tool_input = Some(json!({
            "file_path": state_root.join("sibling-worktree/backend/src/lib.rs")
        }));
        assert_eq!(handle_hook(&external_read, state_root).unwrap(), json!({}));
    }

    #[test]
    fn root_control_sidecar_allows_only_the_same_turn_encrypted_writer() {
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path();
        let workspace = state_root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let workspace = workspace.to_string_lossy().into_owned();
        let session_id = "encrypted-writer-sidecar-session";
        let root_turn = "root-turn-sidecar";
        let contract = json!({
            "id": "encrypted_writer",
            "why": "implementation",
            "visual": false,
            "root": workspace,
            "read": [],
            "write": ["backend/src"],
            "capabilities": ["files.read", "workspace.write"],
            "checks": [{ "id": "tests", "cmd": "cargo test -p codey --lib" }]
        });
        let sidecar = json!({
            "task_name": "encrypted_writer",
            "agent_type": "codey_worker",
            "preparation_id": "prep-encrypted-writer",
            "contract": contract
        });

        let mut reader_spawn = input("PreToolUse", session_id);
        reader_spawn.turn_id = Some(root_turn.to_string());
        reader_spawn.cwd = Some(workspace.clone());
        reader_spawn.tool_name = Some("agents.spawn_agent".to_string());
        reader_spawn.tool_input = Some(json!({
            "task_name": "active_reader",
            "agent_type": "codey_deep_research",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "active_reader",
                "why": "independent_review",
                "visual": false,
                "root": workspace,
                "read": ["docs"],
                "write": [],
                "checks": []
            }))
        }));
        assert_eq!(handle_hook(&reader_spawn, state_root).unwrap(), json!({}));
        let mut reader_spawned = input("PostToolUse", session_id);
        reader_spawned.turn_id = Some(root_turn.to_string());
        reader_spawned.tool_name = reader_spawn.tool_name.clone();
        reader_spawned.tool_input = reader_spawn.tool_input.clone();
        reader_spawned.tool_response = Some(json!({ "task_name": "/root/active_reader" }));
        assert_eq!(handle_hook(&reader_spawned, state_root).unwrap(), json!({}));
        let mut reader_started = input("SubagentStart", session_id);
        reader_started.agent_id = Some("/root/active_reader".to_string());
        assert_eq!(handle_hook(&reader_started, state_root).unwrap(), json!({}));

        let mut prepare = input("PreToolUse", session_id);
        prepare.turn_id = Some(root_turn.to_string());
        prepare.cwd = Some(workspace.clone());
        prepare.tool_name =
            Some(crate::subagent_control_mcp::PREPARE_DELEGATION_QUALIFIED_TOOL_NAME.to_string());
        prepare.tool_input = Some(sidecar.clone());
        assert_eq!(handle_hook(&prepare, state_root).unwrap(), json!({}));

        let mut receipt = sidecar.clone();
        receipt
            .as_object_mut()
            .unwrap()
            .insert("accepted".to_string(), json!(true));
        let mut prepared = input("PostToolUse", session_id);
        prepared.turn_id = Some(root_turn.to_string());
        prepared.cwd = Some(workspace.clone());
        prepared.tool_name = prepare.tool_name.clone();
        prepared.tool_input = Some(sidecar);
        prepared.tool_response = Some(json!({
            "structuredContent": receipt,
            "isError": false
        }));
        assert_eq!(handle_hook(&prepared, state_root).unwrap(), json!({}));

        let encrypted_spawn_input = json!({
            "task_name": "encrypted_writer",
            "agent_type": "codey_worker",
            "fork_turns": "none",
            "message": format!("gAAAAA{}", "A".repeat(160))
        });
        let mut wrong_turn_spawn = input("PreToolUse", session_id);
        wrong_turn_spawn.turn_id = Some("root-turn-other".to_string());
        wrong_turn_spawn.cwd = Some(workspace.clone());
        wrong_turn_spawn.tool_name = Some("agents.spawn_agent".to_string());
        wrong_turn_spawn.tool_input = Some(encrypted_spawn_input.clone());
        let denied = handle_hook(&wrong_turn_spawn, state_root).unwrap();
        assert_eq!(
            denied["hookSpecificOutput"]["permissionDecision"].as_str(),
            Some("deny")
        );
        assert!(
            denied["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("turn_id"))
        );

        let mut spawn = wrong_turn_spawn;
        spawn.turn_id = Some(root_turn.to_string());
        assert_eq!(handle_hook(&spawn, state_root).unwrap(), json!({}));

        let mut spawned = input("PostToolUse", session_id);
        spawned.turn_id = Some(root_turn.to_string());
        spawned.tool_name = spawn.tool_name.clone();
        spawned.tool_input = spawn.tool_input.clone();
        spawned.tool_response = Some(json!({ "task_name": "/root/encrypted_writer" }));
        assert_eq!(handle_hook(&spawned, state_root).unwrap(), json!({}));

        let mut started = input("SubagentStart", session_id);
        started.agent_id = Some("/root/encrypted_writer".to_string());
        assert_eq!(handle_hook(&started, state_root).unwrap(), json!({}));

        let mut write = input("PreToolUse", session_id);
        write.agent_id = Some("/root/encrypted_writer".to_string());
        write.cwd = Some(workspace);
        write.tool_name = Some("apply_patch".to_string());
        write.tool_input = Some(json!({
            "patch": "*** Begin Patch\n*** Update File: backend/src/lib.rs\n*** End Patch"
        }));
        assert_eq!(handle_hook(&write, state_root).unwrap(), json!({}));
    }

    #[test]
    fn root_control_sidecar_requires_a_root_turn_id() {
        let temp = tempfile::tempdir().unwrap();
        let mut prepare = input("PreToolUse", "missing-sidecar-turn-session");
        prepare.tool_name =
            Some(crate::subagent_control_mcp::PREPARE_DELEGATION_QUALIFIED_TOOL_NAME.to_string());
        prepare.tool_input = Some(json!({
            "task_name": "encrypted_writer",
            "agent_type": "codey_worker",
            "preparation_id": "prep-missing-turn",
            "contract": {
                "id": "encrypted_writer",
                "why": "implementation",
                "visual": false,
                "root": "/repo",
                "read": [],
                "write": ["backend/src"],
                "capabilities": ["files.read", "workspace.write"],
                "checks": [{ "id": "tests", "cmd": "cargo test" }]
            }
        }));
        let denied = handle_hook(&prepare, temp.path()).unwrap();
        assert_eq!(
            denied["hookSpecificOutput"]["permissionDecision"].as_str(),
            Some("deny")
        );
        assert!(
            denied["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("turn_id"))
        );
    }

    #[test]
    fn runtime_gate_allows_small_delegations_with_a_valid_role_contract() {
        let temp = tempfile::tempdir().unwrap();
        let mut spawn = input("PreToolUse", "small-session");
        spawn.tool_name = Some("agents.spawn_agent".to_string());
        spawn.tool_input = Some(json!({
            "task_name": "tiny_scan",
            "agent_type": "codey_deep_research",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "tiny_scan",
                "why": "breadth",
                "visual": false,
                "read": [],
                "write": [],
                "checks": []
            }))
        }));
        assert_eq!(handle_hook(&spawn, temp.path()).unwrap(), json!({}));
    }

    #[test]
    fn anonymous_actor_with_active_subagent_is_limited_to_reconciliation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut start = input("SubagentStart", "session-a");
        start.agent_id = Some("agent-a".to_string());
        handle_hook(&start, root).unwrap();

        let mut root_bash = input("PreToolUse", "session-a");
        root_bash.tool_name = Some("Bash".to_string());
        let denied = handle_hook(&root_bash, root).unwrap();
        assert_eq!(
            denied["hookSpecificOutput"]["permissionDecision"].as_str(),
            Some("deny")
        );

        let mut root_read = input("PreToolUse", "session-a");
        root_read.tool_name = Some("mcp__codey_fastctx__grep".to_string());
        assert_eq!(
            handle_hook(&root_read, root).unwrap()["hookSpecificOutput"]["permissionDecision"]
                .as_str(),
            Some("deny")
        );

        let mut root_network = input("PreToolUse", "session-a");
        root_network.tool_name = Some("web_search".to_string());
        assert_eq!(
            handle_hook(&root_network, root).unwrap()["hookSpecificOutput"]["permissionDecision"]
                .as_str(),
            Some("deny")
        );

        let mut child_bash = input("PreToolUse", "session-a");
        child_bash.agent_id = Some("agent-a".to_string());
        child_bash.tool_name = Some("Bash".to_string());
        let child_denied = handle_hook(&child_bash, root).unwrap();
        assert_eq!(
            child_denied["hookSpecificOutput"]["permissionDecision"].as_str(),
            Some("deny")
        );
        assert!(
            child_denied["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("有效 attempt"))
        );

        for tool in [
            "agents.wait_agent",
            "functions.wait",
            "functions/wait",
            "functions:wait",
            "functions__wait",
            "functions_wait",
        ] {
            let mut wait = input("PreToolUse", "session-a");
            wait.tool_name = Some(tool.to_string());
            assert_eq!(handle_hook(&wait, root).unwrap(), json!({}), "{tool}");
        }

        let mut full_list = input("PreToolUse", "session-a");
        full_list.tool_name = Some("agents.list_agents".to_string());
        full_list.tool_input = Some(json!({}));
        assert_eq!(handle_hook(&full_list, root).unwrap(), json!({}));

        for (tool, tool_input) in [
            ("agents.spawn_agent", json!({})),
            ("agents.followup_task", json!({ "target": "/root/agent-a" })),
            (
                "agents.interrupt_agent",
                json!({ "target": "/root/agent-a" }),
            ),
            ("agents.send_message", json!({ "target": "/root/agent-a" })),
            (
                "agents.list_agents",
                json!({ "path_prefix": "/root/agent-a" }),
            ),
        ] {
            let mut orchestration = input("PreToolUse", "session-a");
            orchestration.tool_name = Some(tool.to_string());
            orchestration.tool_input = Some(tool_input);
            let denied = handle_hook(&orchestration, root).unwrap();
            assert_eq!(
                denied["hookSpecificOutput"]["permissionDecision"].as_str(),
                Some("deny"),
                "{tool}"
            );
            assert!(
                denied["hookSpecificOutput"]["permissionDecisionReason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("主体身份")),
                "{tool}"
            );
        }

        let mut functions_exec = input("PreToolUse", "session-a");
        functions_exec.tool_name = Some("functions.exec".to_string());
        assert_eq!(
            handle_hook(&functions_exec, root).unwrap()["hookSpecificOutput"]["permissionDecision"]
                .as_str(),
            Some("deny")
        );
    }

    #[test]
    fn bound_root_turn_can_finish_batch_dispatch_while_other_turns_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let spawn_input = |task: &str, turn: &str| {
            let mut spawn = input("PreToolUse", "turn-bound-session");
            spawn.turn_id = Some(turn.to_string());
            spawn.tool_name = Some("agents.spawn_agent".to_string());
            spawn.tool_input = Some(json!({
                "task_name": task,
                "agent_type": "codey_deep_research",
                "fork_turns": "none",
                "message": delegation_message(json!({
                    "id": task,
                    "why": "breadth",
                    "visual": false,
                    "read": [],
                    "write": [],
                    "checks": []
                }))
            }));
            spawn
        };

        let first = spawn_input("research_first", "root-turn-a");
        assert_eq!(handle_hook(&first, root).unwrap(), json!({}));
        let second = spawn_input("research_second", "root-turn-a");
        assert_eq!(handle_hook(&second, root).unwrap(), json!({}));

        let wrong_turn = spawn_input("research_third", "child-turn-b");
        let denied = handle_hook(&wrong_turn, root).unwrap();
        assert_eq!(
            denied["hookSpecificOutput"]["permissionDecision"].as_str(),
            Some("deny")
        );
        assert!(
            denied["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("turn_id"))
        );

        let mut root_message = input("PreToolUse", "turn-bound-session");
        root_message.turn_id = Some("root-turn-a".to_string());
        root_message.tool_name = Some("agents.send_message".to_string());
        root_message.tool_input = Some(json!({
            "target": "/root/research_first",
            "message": "status?"
        }));
        assert_eq!(handle_hook(&root_message, root).unwrap(), json!({}));
    }

    #[test]
    fn user_prompt_submit_rebinds_the_trusted_root_turn_without_blanket_cancellation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let session_id = "user-steering-session";

        let mut spawn = input("PreToolUse", session_id);
        spawn.turn_id = Some("root-turn-a".to_string());
        spawn.tool_name = Some("agents.spawn_agent".to_string());
        spawn.tool_input = Some(json!({
            "task_name": "steer_reader",
            "agent_type": "codey_deep_research",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "steer_reader",
                "why": "independent_review",
                "visual": false,
                "read": [],
                "write": [],
                "checks": []
            }))
        }));
        assert_eq!(
            handle_hook_for_runtime_at(&spawn, root, runtime_id, 10).unwrap(),
            json!({})
        );

        let mut interrupt = input("PreToolUse", session_id);
        interrupt.turn_id = Some("root-turn-b".to_string());
        interrupt.tool_name = Some("agents.interrupt_agent".to_string());
        interrupt.tool_input = Some(json!({ "target": "/root/steer_reader" }));
        assert_eq!(
            handle_hook_for_runtime_at(&interrupt, root, runtime_id, 20).unwrap()
                ["hookSpecificOutput"]["permissionDecision"]
                .as_str(),
            Some("deny")
        );

        let mut child_prompt = input("UserPromptSubmit", session_id);
        child_prompt.agent_id = Some("child-agent".to_string());
        child_prompt.turn_id = Some("child-turn".to_string());
        child_prompt.prompt = Some("continue".to_string());
        assert_eq!(
            handle_hook_for_runtime_at(&child_prompt, root, runtime_id, 25).unwrap(),
            json!({})
        );
        interrupt.turn_id = Some("child-turn".to_string());
        assert_eq!(
            handle_hook_for_runtime_at(&interrupt, root, runtime_id, 26).unwrap()
                ["hookSpecificOutput"]["permissionDecision"]
                .as_str(),
            Some("deny")
        );

        let mut user_prompt = input("UserPromptSubmit", session_id);
        user_prompt.turn_id = Some("root-turn-b".to_string());
        user_prompt.prompt = Some("取消这个读取代理，保留其他任务".to_string());
        let steering = handle_hook_for_runtime_at(&user_prompt, root, runtime_id, 30).unwrap();
        let context = steering["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(context.contains("当前用户输入优先"));
        assert!(context.contains("只中断仍非终态且被明确取消的 target"));
        assert!(context.contains("不得被解释为取消全部代理"));
        interrupt.turn_id = Some("root-turn-b".to_string());
        assert_eq!(
            handle_hook_for_runtime_at(&interrupt, root, runtime_id, 40).unwrap(),
            json!({})
        );

        let idle = input("UserPromptSubmit", "idle-session");
        assert_eq!(
            handle_hook_for_runtime_at(&idle, root, runtime_id, 50).unwrap(),
            json!({})
        );
    }

    #[test]
    fn combined_hook_keeps_fastctx_active_and_prioritizes_the_subagent_gate() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let mut root_bash = input("PreToolUse", "session-a");
        root_bash.tool_name = Some("Bash".to_string());
        root_bash.tool_input = Some(json!({ "command": "rg -n needle src" }));

        let routed = combined_hook_output_for_runtime(&root_bash, root, runtime_id, false).unwrap();
        assert!(
            routed["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("Codey FastCtx"))
        );

        let mut start = input("SubagentStart", "session-a");
        start.agent_id = Some("agent-a".to_string());
        handle_hook_for_runtime(&start, root, runtime_id).unwrap();
        let gated = combined_hook_output_for_runtime(&root_bash, root, runtime_id, true).unwrap();
        assert!(
            gated["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("子代理门禁"))
        );

        let mut child_bash = root_bash;
        child_bash.agent_id = Some("agent-a".to_string());
        let child_routed =
            combined_hook_output_for_runtime(&child_bash, root, runtime_id, true).unwrap();
        assert!(
            child_routed["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("有效 attempt"))
        );
    }

    #[test]
    fn child_cannot_spawn_nested_subagents_through_any_supported_alias() {
        let temp = tempfile::tempdir().unwrap();
        for tool in [
            "Agent",
            "agents.Agent",
            "spawn_agent",
            "agents.spawn_agent",
            "agents__spawn_agent",
            "agentsspawn_agent",
        ] {
            let mut child_spawn = input("PreToolUse", "session-a");
            child_spawn.agent_id = Some("agent-a".to_string());
            child_spawn.tool_name = Some(tool.to_string());

            let denied = handle_hook(&child_spawn, temp.path()).unwrap();
            assert_eq!(
                denied["hookSpecificOutput"]["permissionDecision"].as_str(),
                Some("deny"),
                "{tool}"
            );
            assert!(
                denied["hookSpecificOutput"]["permissionDecisionReason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("子代理不能继续派生子代理")),
                "{tool}"
            );
        }
    }

    #[test]
    fn child_can_only_send_collaboration_reports_to_root() {
        let temp = tempfile::tempdir().unwrap();
        for tool in [
            "agents.wait_agent",
            "agents.list_agents",
            "agents.interrupt_agent",
            "agents.followup_task",
        ] {
            let mut child_tool = input("PreToolUse", "session-a");
            child_tool.agent_id = Some("agent-a".to_string());
            child_tool.tool_name = Some(tool.to_string());
            let denied = handle_hook(&child_tool, temp.path()).unwrap();
            assert_eq!(
                denied["hookSpecificOutput"]["permissionDecision"].as_str(),
                Some("deny"),
                "{tool}"
            );
        }

        let mut sibling_message = input("PreToolUse", "session-a");
        sibling_message.agent_id = Some("agent-a".to_string());
        sibling_message.tool_name = Some("agents.send_message".to_string());
        sibling_message.tool_input = Some(json!({ "target": "/root/sibling", "message": "x" }));
        assert_eq!(
            handle_hook(&sibling_message, temp.path()).unwrap()
                ["hookSpecificOutput"]["permissionDecision"]
                .as_str(),
            Some("deny")
        );

        let mut root_message = sibling_message;
        root_message.tool_input = Some(json!({ "target": "/root", "message": "status" }));
        assert_eq!(handle_hook(&root_message, temp.path()).unwrap(), json!({}));
    }

    #[test]
    fn typed_missing_id_context_keeps_all_anonymous_dispatch_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let session_id = "typed-missing-id-session";

        let mut start = input("SubagentStart", session_id);
        start.agent_type = Some("codey_quick_scan".to_string());
        handle_hook_for_runtime_at(&start, root, runtime_id, 1_000).unwrap();
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            1
        );

        let mut child_spawn = input("PreToolUse", session_id);
        child_spawn.agent_type = Some("codey_quick_scan".to_string());
        child_spawn.tool_name = Some("agents.spawn_agent".to_string());
        let denied = handle_hook_for_runtime_at(&child_spawn, root, runtime_id, 1_500).unwrap();
        assert!(
            denied["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("缺少 agent_id"))
        );

        let mut child_command = input("PreToolUse", session_id);
        child_command.agent_type = Some("codey_quick_scan".to_string());
        child_command.tool_name = Some("Bash".to_string());
        let denied = handle_hook_for_runtime_at(&child_command, root, runtime_id, 1_600).unwrap();
        assert!(
            denied["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("缺少 agent_id"))
        );

        let mut child_read = input("PreToolUse", session_id);
        child_read.agent_type = Some("codey_quick_scan".to_string());
        child_read.tool_name = Some("mcp__codey_fastctx__grep".to_string());
        assert_eq!(
            handle_hook_for_runtime_at(&child_read, root, runtime_id, 1_700).unwrap()
                ["hookSpecificOutput"]["permissionDecision"]
                .as_str(),
            Some("deny")
        );

        let mut child_stop = input("Stop", session_id);
        child_stop.agent_type = Some("codey_quick_scan".to_string());
        assert_eq!(
            handle_hook_for_runtime_at(&child_stop, root, runtime_id, 1_800).unwrap(),
            json!({})
        );

        let mut root_spawn = input("PreToolUse", session_id);
        root_spawn.tool_name = Some("agents.spawn_agent".to_string());
        root_spawn.tool_input = Some(json!({
            "task_name": "second_scan",
            "agent_type": "codey_quick_scan",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "second_scan",
                "why": "multi_lookup",
                "visual": false,
                "read": [],
                "write": [],
                "checks": []
            }))
        }));
        let root_denied = handle_hook_for_runtime_at(&root_spawn, root, runtime_id, 2_000).unwrap();
        assert!(
            root_denied["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("主体身份"))
        );

        let mut unknown_wait = input("PostToolUse", session_id);
        unknown_wait.tool_name = Some("agents.wait_agent".to_string());
        unknown_wait.tool_response = Some(json!({ "unexpected": "payload" }));
        let blocked = handle_hook_for_runtime_at(&unknown_wait, root, runtime_id, 2_100).unwrap();
        assert!(
            blocked["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("响应结构无法识别"))
        );

        let mut third_root_spawn = input("PreToolUse", session_id);
        third_root_spawn.tool_name = Some("agents.spawn_agent".to_string());
        let denied =
            handle_hook_for_runtime_at(&third_root_spawn, root, runtime_id, 2_200).unwrap();
        assert!(
            denied["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("主体身份"))
        );
    }

    #[test]
    fn missing_agent_id_enters_visible_fail_safe_mode_and_list_reconciles_it() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let session_id = "missing-id-session";

        handle_hook_for_runtime_at(&input("SubagentStart", session_id), root, runtime_id, 1_000)
            .unwrap();
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            1
        );

        let mut root_patch = input("PreToolUse", session_id);
        root_patch.tool_name = Some("apply_patch".to_string());
        let denied = handle_hook_for_runtime_at(&root_patch, root, runtime_id, 2_000).unwrap();
        let reason = denied["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.contains("缺少 agent_id"));
        assert!(reason.contains("兼容性"));

        let mut ambiguous_spawn = input("PreToolUse", session_id);
        ambiguous_spawn.tool_name = Some("agents.spawn_agent".to_string());
        ambiguous_spawn.tool_input = Some(json!({}));
        let denied = handle_hook_for_runtime_at(&ambiguous_spawn, root, runtime_id, 2_500).unwrap();
        assert!(
            denied["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("主体身份")
        );

        let mut list = input("PostToolUse", session_id);
        list.tool_name = Some("agents.list_agents".to_string());
        list.tool_input = Some(json!({}));
        list.tool_response = Some(json!({
            "children": [
                { "name": "/root", "status": "running" },
                { "name": "/root/agent-a", "status": "completed" }
            ]
        }));
        assert_eq!(
            handle_hook_for_runtime_at(&list, root, runtime_id, 3_000).unwrap(),
            json!({})
        );
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            0
        );
    }

    #[test]
    fn missing_id_stop_settles_only_a_unique_active_ledger_candidate() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";

        let spawn_agent = |session_id: &str, task_id: &str, now_ms: u64| {
            let mut spawn = input("PreToolUse", session_id);
            spawn.turn_id = Some("root-turn-a".to_string());
            spawn.tool_name = Some("agents.spawn_agent".to_string());
            spawn.tool_input = Some(json!({
                "task_name": task_id,
                "agent_type": "codey_deep_research",
                "fork_turns": "none",
                "message": delegation_message(json!({
                    "id": task_id,
                    "why": "independent_review",
                    "visual": false,
                    "read": [],
                    "write": [],
                    "checks": []
                }))
            }));
            handle_hook_for_runtime_at(&spawn, root, runtime_id, now_ms).unwrap();
            let mut spawned = input("PostToolUse", session_id);
            spawned.tool_name = spawn.tool_name.clone();
            spawned.tool_input = spawn.tool_input.clone();
            spawned.tool_response = Some(json!({
                "agent_id": format!("agent-{task_id}")
            }));
            handle_hook_for_runtime_at(&spawned, root, runtime_id, now_ms + 1).unwrap();
        };

        let unique_session = "unique-anonymous-stop";
        spawn_agent(unique_session, "only_reader", 10);
        let mut anonymous_start = input("SubagentStart", unique_session);
        anonymous_start.agent_type = Some("codey_deep_research".to_string());
        handle_hook_for_runtime_at(&anonymous_start, root, runtime_id, 12).unwrap();
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, unique_session).unwrap(),
            1
        );
        let mut anonymous_stop = input("SubagentStop", unique_session);
        anonymous_stop.agent_type = Some("codey_deep_research".to_string());
        assert_eq!(
            handle_hook_for_runtime_at(&anonymous_stop, root, runtime_id, 13).unwrap(),
            json!({})
        );
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, unique_session).unwrap(),
            0
        );

        let ambiguous_session = "ambiguous-anonymous-stop";
        spawn_agent(ambiguous_session, "reader_a", 20);
        spawn_agent(ambiguous_session, "reader_b", 30);
        let mut anonymous_start = input("SubagentStart", ambiguous_session);
        anonymous_start.agent_type = Some("codey_deep_research".to_string());
        handle_hook_for_runtime_at(&anonymous_start, root, runtime_id, 32).unwrap();
        let mut anonymous_stop = input("SubagentStop", ambiguous_session);
        anonymous_stop.agent_type = Some("codey_deep_research".to_string());
        handle_hook_for_runtime_at(&anonymous_stop, root, runtime_id, 33).unwrap();
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, ambiguous_session).unwrap(),
            2
        );
        assert!(
            agent_marker_path(
                &session_state_dir(root, ambiguous_session),
                runtime_id,
                MISSING_AGENT_ID_MARKER,
            )
            .exists()
        );
    }

    #[test]
    fn unknown_wait_shape_is_reported_then_cleared_by_a_known_shape() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let session_id = "protocol-session";
        let mut start = input("SubagentStart", session_id);
        start.agent_id = Some("agent-a".to_string());
        handle_hook_for_runtime_at(&start, root, runtime_id, 1_000).unwrap();

        let mut unknown_wait = input("PostToolUse", session_id);
        unknown_wait.tool_name = Some("agents.wait_agent".to_string());
        unknown_wait.tool_response = Some(json!({ "unexpected": "payload" }));
        let blocked = handle_hook_for_runtime_at(&unknown_wait, root, runtime_id, 2_000).unwrap();
        assert!(
            blocked["reason"]
                .as_str()
                .unwrap()
                .contains("响应结构无法识别")
        );

        let mut known_wait = input("PostToolUse", session_id);
        known_wait.tool_name = Some("agents.wait_agent".to_string());
        known_wait.tool_response = Some(json!({
            "timedOut": true,
            "message": "still running"
        }));
        let blocked = handle_hook_for_runtime_at(&known_wait, root, runtime_id, 3_000).unwrap();
        assert!(!blocked["reason"].as_str().unwrap().contains("兼容性诊断"));
    }

    #[test]
    fn subagent_stop_releases_root_and_stop_hook_cannot_finish_early() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut start = input("SubagentStart", "session-a");
        start.agent_id = Some("agent-a".to_string());
        handle_hook(&start, root).unwrap();

        let blocked = handle_hook(&input("Stop", "session-a"), root).unwrap();
        assert_eq!(blocked["decision"].as_str(), Some("block"));

        let mut stop = input("SubagentStop", "session-a");
        stop.agent_id = Some("agent-a".to_string());
        handle_hook(&stop, root).unwrap();
        assert_eq!(
            handle_hook(&input("Stop", "session-a"), root).unwrap(),
            json!({})
        );
    }

    #[test]
    fn verified_read_only_batch_allows_only_trusted_root_fastctx_reads() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-read-window";
        let root_turn = "root-turn-read-window";
        let base = current_timestamp_millis();
        let spawn_agent = |session_id: &str,
                           task_id: &str,
                           role: &str,
                           agent_id: &str,
                           contract: Value,
                           now_ms: u64| {
            let mut spawn = input("PreToolUse", session_id);
            spawn.turn_id = Some(root_turn.to_string());
            spawn.tool_name = Some("agents.spawn_agent".to_string());
            spawn.tool_input = Some(json!({
                "task_name": task_id,
                "agent_type": role,
                "fork_turns": "none",
                "message": delegation_message(contract),
            }));
            assert_eq!(
                handle_hook_for_runtime_at(&spawn, root, runtime_id, now_ms).unwrap(),
                json!({})
            );

            let mut spawned = input("PostToolUse", session_id);
            spawned.turn_id = Some(root_turn.to_string());
            spawned.tool_name = spawn.tool_name.clone();
            spawned.tool_input = spawn.tool_input.clone();
            spawned.tool_response = Some(json!({ "agent_id": agent_id }));
            assert_eq!(
                handle_hook_for_runtime_at(&spawned, root, runtime_id, now_ms + 1).unwrap(),
                json!({})
            );

            let mut started = input("SubagentStart", session_id);
            started.agent_id = Some(agent_id.to_string());
            started.agent_type = Some(role.to_string());
            assert_eq!(
                handle_hook_for_runtime_at(&started, root, runtime_id, now_ms + 2).unwrap(),
                json!({})
            );
        };
        let read_contract = |task_id: &str| {
            json!({
                "id": task_id,
                "why": "breadth",
                "visual": false,
                "read": [],
                "write": [],
                "capabilities": ["files.read"],
                "checks": [],
            })
        };
        let root_tool = |session_id: &str, turn_id: &str, tool_name: &str| {
            let mut tool = input("PreToolUse", session_id);
            tool.turn_id = Some(turn_id.to_string());
            tool.tool_name = Some(tool_name.to_string());
            tool
        };
        let permission = |output: &Value| {
            output["hookSpecificOutput"]["permissionDecision"]
                .as_str()
                .map(str::to_string)
        };

        let read_session = "verified-read-session";
        spawn_agent(
            read_session,
            "reader_a",
            "codey_deep_research",
            "agent-reader-a",
            read_contract("reader_a"),
            base + 10,
        );
        spawn_agent(
            read_session,
            "reader_b",
            "codey_quick_scan",
            "agent-reader-b",
            read_contract("reader_b"),
            base + 20,
        );

        for tool_name in [
            "mcp__codey_fastctx__inspect_local_file",
            "mcp__codey_fastctx__grep",
            "mcp__codey_fastctx__glob",
        ] {
            assert_eq!(
                handle_hook_for_runtime_at(
                    &root_tool(read_session, root_turn, tool_name),
                    root,
                    runtime_id,
                    base + 30,
                )
                .unwrap(),
                json!({}),
                "{tool_name}"
            );
        }
        for tool_name in [
            "mcp__codey_fastctx__replace",
            "functions.apply_patch",
            "functions.exec",
            "web.run",
            "tool_search",
            "functions.view_image",
        ] {
            let denied = handle_hook_for_runtime_at(
                &root_tool(read_session, root_turn, tool_name),
                root,
                runtime_id,
                base + 31,
            )
            .unwrap();
            assert_eq!(permission(&denied).as_deref(), Some("deny"), "{tool_name}");
        }
        let untrusted = handle_hook_for_runtime_at(
            &root_tool(read_session, "different-turn", "mcp__codey_fastctx__grep"),
            root,
            runtime_id,
            base + 32,
        )
        .unwrap();
        assert_eq!(permission(&untrusted).as_deref(), Some("deny"));
        assert_eq!(
            handle_hook_for_runtime_at(&input("Stop", read_session), root, runtime_id, base + 33,)
                .unwrap()["decision"]
                .as_str(),
            Some("block")
        );

        let mut partial_wait = input("PostToolUse", read_session);
        partial_wait.turn_id = Some(root_turn.to_string());
        partial_wait.tool_name = Some("agents.wait_agent".to_string());
        partial_wait.tool_response = Some(json!({
            "updates": [
                { "task_name": "/root/reader_a", "status": "completed", "message": "evidence" },
                { "agent_id": "agent-reader-b", "status": "running" }
            ],
            "timed_out": false
        }));
        let continuation =
            handle_hook_for_runtime_at(&partial_wait, root, runtime_id, base + 40).unwrap();
        assert_eq!(continuation["decision"].as_str(), Some("block"));
        let reason = continuation["reason"].as_str().unwrap();
        assert!(reason.contains("仍有 1 个子代理"));
        assert!(reason.contains("mcp__codey_fastctx__inspect_local_file"));
        assert!(reason.contains("写入、命令、网络"));
        assert!(
            !agent_marker_path(
                &session_state_dir(root, read_session),
                runtime_id,
                "agent-reader-a",
            )
            .exists()
        );
        assert!(
            agent_marker_path(
                &session_state_dir(root, read_session),
                runtime_id,
                "agent-reader-b",
            )
            .exists()
        );
        assert_eq!(
            handle_hook_for_runtime_at(
                &root_tool(read_session, root_turn, "mcp__codey_fastctx__grep"),
                root,
                runtime_id,
                base + 41,
            )
            .unwrap(),
            json!({})
        );

        remove_active_marker(root, runtime_id, read_session, "agent-reader-b").unwrap();
        create_active_marker(root, runtime_id, read_session, "untracked-agent").unwrap();
        let marker_mismatch = handle_hook_for_runtime_at(
            &root_tool(read_session, root_turn, "mcp__codey_fastctx__grep"),
            root,
            runtime_id,
            base + 42,
        )
        .unwrap();
        assert_eq!(permission(&marker_mismatch).as_deref(), Some("deny"));

        let command_session = "command-capable-read-session";
        spawn_agent(
            command_session,
            "command_reader",
            "codey_deep_research",
            "agent-command-reader",
            json!({
                "id": "command_reader",
                "why": "breadth",
                "visual": false,
                "read": [],
                "write": [],
                "capabilities": ["files.read", "command.execute"],
                "checks": [],
            }),
            base + 50,
        );
        let command_capable = handle_hook_for_runtime_at(
            &root_tool(command_session, root_turn, "mcp__codey_fastctx__grep"),
            root,
            runtime_id,
            base + 53,
        )
        .unwrap();
        assert_eq!(permission(&command_capable).as_deref(), Some("deny"));

        let write_session = "writer-session";
        spawn_agent(
            write_session,
            "writer_a",
            "codey_worker",
            "agent-writer-a",
            json!({
                "id": "writer_a",
                "why": "independent_work",
                "visual": false,
                "read": [],
                "write": ["backend/src"],
                "capabilities": ["files.read", "workspace.write"],
                "checks": [{ "id": "tests", "cmd": "cargo test -p codey --lib" }],
            }),
            base + 60,
        );
        let writer_active = handle_hook_for_runtime_at(
            &root_tool(write_session, root_turn, "mcp__codey_fastctx__grep"),
            root,
            runtime_id,
            base + 63,
        )
        .unwrap();
        assert_eq!(permission(&writer_active).as_deref(), Some("deny"));
    }

    #[test]
    fn partial_wait_updates_keep_root_blocked_until_every_subagent_stops() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for agent_id in ["agent-a", "agent-b", "agent-c"] {
            let mut start = input("SubagentStart", "session-a");
            start.agent_id = Some(agent_id.to_string());
            handle_hook(&start, root).unwrap();
        }

        let mut stop_a = input("SubagentStop", "session-a");
        stop_a.agent_id = Some("agent-a".to_string());
        handle_hook(&stop_a, root).unwrap();

        let mut first_wait = input("PostToolUse", "session-a");
        first_wait.tool_name = Some("agents.wait_agent".to_string());
        first_wait.tool_response = Some(json!({
            "status": "FINAL_ANSWER",
            "agent_id": "agent-a",
            "message": "first result"
        }));
        let blocked_after_first = handle_hook(&first_wait, root).unwrap();
        assert_eq!(blocked_after_first["decision"].as_str(), Some("block"));
        let first_reason = blocked_after_first["reason"].as_str().unwrap();
        assert!(first_reason.contains("仍有 2 个子代理"));
        assert!(first_reason.contains("first result"));
        assert!(first_reason.contains("可继续使用 agents.wait_agent"));
        assert!(first_reason.contains("不得恢复非协作本地工作"));

        let mut root_steer = input("PreToolUse", "session-a");
        root_steer.tool_name = Some("agents.send_message".to_string());
        assert_eq!(
            handle_hook(&root_steer, root).unwrap()["hookSpecificOutput"]["permissionDecision"]
                .as_str(),
            Some("deny")
        );

        let mut root_patch = input("PreToolUse", "session-a");
        root_patch.tool_name = Some("apply_patch".to_string());
        assert_eq!(
            handle_hook(&root_patch, root).unwrap()["hookSpecificOutput"]["permissionDecision"]
                .as_str(),
            Some("deny")
        );
        assert_eq!(
            handle_hook(&input("Stop", "session-a"), root).unwrap()["decision"].as_str(),
            Some("block")
        );

        let mut stop_b = input("SubagentStop", "session-a");
        stop_b.agent_id = Some("agent-b".to_string());
        handle_hook(&stop_b, root).unwrap();
        let blocked_after_second = handle_hook(&first_wait, root).unwrap();
        assert!(
            blocked_after_second["reason"]
                .as_str()
                .unwrap()
                .contains("仍有 1 个子代理")
        );

        let mut stop_c = input("SubagentStop", "session-a");
        stop_c.agent_id = Some("agent-c".to_string());
        handle_hook(&stop_c, root).unwrap();
        assert_eq!(handle_hook(&first_wait, root).unwrap(), json!({}));
        assert_eq!(handle_hook(&root_patch, root).unwrap(), json!({}));
        assert_eq!(
            handle_hook(&input("Stop", "session-a"), root).unwrap(),
            json!({})
        );
    }

    #[test]
    fn completed_wait_response_releases_matching_active_markers() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for agent_id in ["agent-a", "agent-b"] {
            let mut start = input("SubagentStart", "session-a");
            start.agent_id = Some(agent_id.to_string());
            handle_hook(&start, root).unwrap();
        }

        let mut completed_wait = input("PostToolUse", "session-a");
        completed_wait.tool_name = Some("functions.wait".to_string());
        completed_wait.tool_response = Some(json!({
            "updates": [
                {
                    "agentId": "agent-a",
                    "status": "FINAL_ANSWER",
                    "message": "done"
                },
                {
                    "agent_id": "agent-b",
                    "kind": "task-complete"
                }
            ]
        }));

        assert_eq!(handle_hook(&completed_wait, root).unwrap(), json!({}));
        assert_eq!(active_agent_count(root, "session-a").unwrap(), 0);

        for agent_id in ["agent-a", "agent-b"] {
            let mut late_stop = input("SubagentStop", "session-a");
            late_stop.agent_id = Some(agent_id.to_string());
            assert_eq!(handle_hook(&late_stop, root).unwrap(), json!({}));
        }
        assert_eq!(active_agent_count(root, "session-a").unwrap(), 0);
    }

    #[test]
    fn errored_and_other_terminal_wait_statuses_release_markers() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for agent_id in ["agent-a", "agent-b", "agent-c", "agent-d"] {
            let mut start = input("SubagentStart", "session-a");
            start.agent_id = Some(agent_id.to_string());
            handle_hook(&start, root).unwrap();
        }

        let mut terminal_wait = input("PostToolUse", "session-a");
        terminal_wait.tool_name = Some("agents.wait_agent".to_string());
        terminal_wait.tool_response = Some(json!({
            "updates": [
                { "agent_id": "agent-a", "status": "completed" },
                { "agent_id": "agent-b", "state": "errored" },
                { "agent_id": "agent-c", "agent_status": { "errored": "429 Too Many Requests" } },
                { "agent_id": "agent-d", "status": "shutdown" }
            ]
        }));

        assert_eq!(handle_hook(&terminal_wait, root).unwrap(), json!({}));
        assert_eq!(active_agent_count(root, "session-a").unwrap(), 0);
    }

    #[test]
    fn full_agent_list_snapshot_reconciles_terminal_children() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for agent_id in ["agent-a", "agent-b"] {
            let mut start = input("SubagentStart", "session-a");
            start.agent_id = Some(agent_id.to_string());
            handle_hook(&start, root).unwrap();
        }

        let mut list = input("PostToolUse", "session-a");
        list.tool_name = Some("agents.list_agents".to_string());
        list.tool_input = Some(json!({}));
        list.tool_response = Some(json!({
            "agents": [
                { "agent_name": "/root", "agent_status": "running" },
                { "agent_name": "/root/agent-a", "agent_status": { "completed": "done" } },
                { "agent_name": "/root/agent-b", "agent_status": { "errored": "503 Service Unavailable" } }
            ]
        }));

        assert_eq!(handle_hook(&list, root).unwrap(), json!({}));
        assert_eq!(active_agent_count(root, "session-a").unwrap(), 0);
    }

    #[test]
    fn mixed_full_list_settles_only_the_completed_ledger_marker() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let session_id = "mixed-ledger-list";
        let spawn = |task_id: &str, agent_id: &str, now_ms: u64| {
            let mut request = input("PreToolUse", session_id);
            request.turn_id = Some("root-turn-a".to_string());
            request.cwd = Some("/repo".to_string());
            request.tool_name = Some("agents.spawn_agent".to_string());
            request.tool_input = Some(json!({
                "task_name": task_id,
                "agent_type": "codey_deep_research",
                "fork_turns": "none",
                "message": delegation_message(json!({
                    "id": task_id,
                    "why": "independent_review",
                    "visual": false,
                    "root": "/repo",
                    "read": [],
                    "write": [],
                    "capabilities": ["files.read"],
                    "checks": []
                }))
            }));
            assert_eq!(
                handle_hook_for_runtime_at(&request, root, runtime_id, now_ms).unwrap(),
                json!({})
            );

            let mut response = input("PostToolUse", session_id);
            response.turn_id = request.turn_id.clone();
            response.tool_name = request.tool_name.clone();
            response.tool_input = request.tool_input.clone();
            response.tool_response = Some(json!({ "agent_id": agent_id }));
            assert_eq!(
                handle_hook_for_runtime_at(&response, root, runtime_id, now_ms + 1).unwrap(),
                json!({})
            );

            let mut started = input("SubagentStart", session_id);
            started.agent_id = Some(agent_id.to_string());
            started.agent_type = Some("codey_deep_research".to_string());
            assert_eq!(
                handle_hook_for_runtime_at(&started, root, runtime_id, now_ms + 2).unwrap(),
                json!({})
            );
        };

        spawn("list_reader_a", "agent-list-a", 10);
        spawn("list_reader_b", "agent-list-b", 20);

        let marker_a = agent_marker_path(
            &session_state_dir(root, session_id),
            runtime_id,
            "agent-list-a",
        );
        let marker_b = agent_marker_path(
            &session_state_dir(root, session_id),
            runtime_id,
            "agent-list-b",
        );
        assert!(marker_a.exists());
        assert!(marker_b.exists());

        let mut list = input("PostToolUse", session_id);
        list.turn_id = Some("root-turn-a".to_string());
        list.tool_name = Some("agents.list_agents".to_string());
        list.tool_input = Some(json!({}));
        list.tool_response = Some(json!({
            "agents": [
                { "agent_name": "/root", "agent_status": "running" },
                { "agent_name": "/root/list_reader_a", "agent_status": { "completed": "done" } },
                { "agent_name": "/root/list_reader_b", "agent_status": "running" }
            ]
        }));

        let blocked = handle_hook_for_runtime_at(&list, root, runtime_id, 30).unwrap();
        assert_eq!(blocked["decision"].as_str(), Some("block"));
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            1
        );
        assert!(!marker_a.exists());
        assert!(marker_b.exists());
    }

    #[test]
    fn filtered_or_mixed_agent_lists_do_not_clear_live_markers() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for agent_id in ["agent-a", "agent-b"] {
            let mut start = input("SubagentStart", "session-a");
            start.agent_id = Some(agent_id.to_string());
            handle_hook(&start, root).unwrap();
        }

        let mut filtered = input("PostToolUse", "session-a");
        filtered.tool_name = Some("agents.list_agents".to_string());
        filtered.tool_input = Some(json!({ "path_prefix": "/root/agent-a" }));
        filtered.tool_response = Some(json!({
            "agents": [{
                "agent_name": "/root/agent-a",
                "agent_status": { "errored": "429 Too Many Requests" }
            }]
        }));
        assert_eq!(
            handle_hook(&filtered, root).unwrap()["decision"].as_str(),
            Some("block")
        );
        assert_eq!(active_agent_count(root, "session-a").unwrap(), 2);

        filtered.tool_input = Some(json!({
            "path_prefix": "",
            "future_filter": "terminal_only"
        }));
        assert_eq!(
            handle_hook(&filtered, root).unwrap()["decision"].as_str(),
            Some("block")
        );
        assert_eq!(active_agent_count(root, "session-a").unwrap(), 2);

        let mut mixed = input("PostToolUse", "session-a");
        mixed.tool_name = Some("agents.list_agents".to_string());
        mixed.tool_input = Some(json!({}));
        mixed.tool_response = Some(json!({
            "agents": [
                { "agent_name": "/root", "agent_status": "running" },
                { "agent_name": "/root/agent-a", "agent_status": { "errored": "429 Too Many Requests" } },
                { "agent_name": "/root/agent-b", "agent_status": "running" }
            ]
        }));
        assert_eq!(
            handle_hook(&mixed, root).unwrap()["decision"].as_str(),
            Some("block")
        );
        assert_eq!(active_agent_count(root, "session-a").unwrap(), 2);
    }

    #[test]
    fn stale_pending_init_and_unusable_collaboration_paths_release_after_grace() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";

        let mut start = input("SubagentStart", "pending-session");
        start.agent_id = Some("agent-a".to_string());
        handle_hook_for_runtime_at(&start, root, runtime_id, 1_000).unwrap();
        let mut list = input("PostToolUse", "pending-session");
        list.tool_name = Some("agents.list_agents".to_string());
        list.tool_input = Some(json!({}));
        list.tool_response = Some(json!({
            "agents": [
                { "agent_name": "/root", "agent_status": "running" },
                { "agent_name": "/root/agent-a", "agent_status": "pending_init" }
            ]
        }));
        assert_eq!(
            handle_hook_for_runtime_at(&list, root, runtime_id, 1_000).unwrap()["decision"]
                .as_str(),
            Some("block")
        );
        assert_eq!(
            handle_hook_for_runtime_at(
                &input("Stop", "pending-session"),
                root,
                runtime_id,
                1_000 + PENDING_INIT_GRACE_MILLIS - 1,
            )
            .unwrap()["decision"]
                .as_str(),
            Some("block")
        );
        assert_eq!(
            handle_hook_for_runtime_at(
                &input("Stop", "pending-session"),
                root,
                runtime_id,
                1_000 + PENDING_INIT_GRACE_MILLIS,
            )
            .unwrap(),
            json!({})
        );

        let mut stalled_start = input("SubagentStart", "stalled-session");
        stalled_start.agent_id = Some("agent-b".to_string());
        handle_hook_for_runtime_at(&stalled_start, root, runtime_id, 2_000).unwrap();
        assert_eq!(
            handle_hook_for_runtime_at(&input("Stop", "stalled-session"), root, runtime_id, 2_000,)
                .unwrap()["decision"]
                .as_str(),
            Some("block")
        );
        let mut unavailable_wait = input("PostToolUse", "stalled-session");
        unavailable_wait.tool_name = Some("agents.wait_agent".to_string());
        unavailable_wait.tool_response = Some(Value::String(
            "该工具未在当前线程注册，无法执行 agents.wait_agent".to_string(),
        ));
        assert_eq!(
            handle_hook_for_runtime_at(
                &unavailable_wait,
                root,
                runtime_id,
                2_000 + STOP_STALL_GRACE_MILLIS - 1,
            )
            .unwrap()["decision"]
                .as_str(),
            Some("block")
        );
        assert_eq!(
            handle_hook_for_runtime_at(
                &input("Stop", "stalled-session"),
                root,
                runtime_id,
                2_000 + STOP_STALL_GRACE_MILLIS,
            )
            .unwrap(),
            json!({})
        );
    }

    #[test]
    fn mixed_pending_init_and_live_agents_use_independent_recovery_timers() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let session_id = "mixed-pending-live-session";
        let root_turn = "root-turn-a";
        let pending_target = "/root/pending_reader";
        let live_target = "/root/live_reader";

        let spawn_agent = |task_id: &str, target: &str, now_ms: u64| {
            let mut spawn = input("PreToolUse", session_id);
            spawn.turn_id = Some(root_turn.to_string());
            spawn.cwd = Some("/repo".to_string());
            spawn.tool_name = Some("agents.spawn_agent".to_string());
            spawn.tool_input = Some(json!({
                "task_name": task_id,
                "agent_type": "codey_deep_research",
                "fork_turns": "none",
                "message": delegation_message(json!({
                    "id": task_id,
                    "why": "independent_review",
                    "visual": false,
                    "root": "/repo",
                    "read": [],
                    "write": [],
                    "checks": []
                }))
            }));
            handle_hook_for_runtime_at(&spawn, root, runtime_id, now_ms).unwrap();
            let mut spawned = input("PostToolUse", session_id);
            spawned.tool_name = spawn.tool_name.clone();
            spawned.tool_input = spawn.tool_input.clone();
            spawned.tool_response = Some(json!({ "agent_id": target }));
            handle_hook_for_runtime_at(&spawned, root, runtime_id, now_ms + 1).unwrap();
            let mut started = input("SubagentStart", session_id);
            started.agent_id = Some(target.to_string());
            handle_hook_for_runtime_at(&started, root, runtime_id, now_ms + 2).unwrap();
        };
        spawn_agent("pending_reader", pending_target, 10);
        spawn_agent("live_reader", live_target, 20);

        let pending_marker = agent_marker_path(
            &session_state_dir(root, session_id),
            runtime_id,
            pending_target,
        );
        let live_marker = agent_marker_path(
            &session_state_dir(root, session_id),
            runtime_id,
            live_target,
        );
        assert!(pending_marker.exists());
        assert!(live_marker.exists());

        let first_seen = 1_000;
        let mut mixed = input("PostToolUse", session_id);
        mixed.turn_id = Some(root_turn.to_string());
        mixed.tool_name = Some("agents.list_agents".to_string());
        mixed.tool_input = Some(json!({}));
        mixed.tool_response = Some(json!({
            "agents": [
                { "agent_name": "/root", "status": "running" },
                { "agent_name": pending_target, "status": "pending_init" },
                { "agent_name": live_target, "status": "running" }
            ]
        }));
        assert_eq!(
            handle_hook_for_runtime_at(&mixed, root, runtime_id, first_seen).unwrap()["decision"]
                .as_str(),
            Some("block")
        );
        // Repeating the same mixed snapshot must not restart the pending
        // reservation's clock merely because its sibling is live.
        assert_eq!(
            handle_hook_for_runtime_at(
                &mixed,
                root,
                runtime_id,
                first_seen + PENDING_INIT_GRACE_MILLIS / 2,
            )
            .unwrap()["decision"]
                .as_str(),
            Some("block")
        );

        let before_deadline = handle_hook_for_runtime_at(
            &input("Stop", session_id),
            root,
            runtime_id,
            first_seen + PENDING_INIT_GRACE_MILLIS - 1,
        )
        .unwrap();
        assert_eq!(before_deadline["decision"].as_str(), Some("block"));
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            2
        );

        let recovered = handle_hook_for_runtime_at(
            &input("Stop", session_id),
            root,
            runtime_id,
            first_seen + PENDING_INIT_GRACE_MILLIS,
        )
        .unwrap();
        assert_eq!(recovered["decision"].as_str(), Some("block"));
        assert!(
            recovered["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("仍有 1 个子代理"))
        );
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            1
        );
        assert!(!pending_marker.exists());
        assert!(live_marker.exists());

        let trace = std::fs::read_to_string(crate::subagent::telemetry::trace_file(root)).unwrap();
        assert!(trace.contains("pending_init_grace_elapsed"));

        // A lagging provider snapshot cannot restart or resurrect the recovered
        // reservation while the healthy sibling continues.
        assert_eq!(
            handle_hook_for_runtime_at(
                &mixed,
                root,
                runtime_id,
                first_seen + PENDING_INIT_GRACE_MILLIS + 1,
            )
            .unwrap()["decision"]
                .as_str(),
            Some("block")
        );
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            1
        );
        assert!(!pending_marker.exists());
        assert!(live_marker.exists());
    }

    #[test]
    fn live_observation_clears_only_that_reservations_pending_init_timer() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let session_id = "pending-becomes-live-session";
        let target = "/root/reader_a";

        let mut spawn = input("PreToolUse", session_id);
        spawn.turn_id = Some("root-turn-a".to_string());
        spawn.tool_name = Some("agents.spawn_agent".to_string());
        spawn.tool_input = Some(json!({
            "task_name": "reader_a",
            "agent_type": "codey_deep_research",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "reader_a",
                "why": "independent_review",
                "visual": false,
                "read": [],
                "write": [],
                "checks": []
            }))
        }));
        handle_hook_for_runtime_at(&spawn, root, runtime_id, 10).unwrap();
        let mut spawned = input("PostToolUse", session_id);
        spawned.tool_name = spawn.tool_name.clone();
        spawned.tool_input = spawn.tool_input.clone();
        spawned.tool_response = Some(json!({ "agent_id": target }));
        handle_hook_for_runtime_at(&spawned, root, runtime_id, 20).unwrap();
        let mut started = input("SubagentStart", session_id);
        started.agent_id = Some(target.to_string());
        handle_hook_for_runtime_at(&started, root, runtime_id, 30).unwrap();

        let mut list = input("PostToolUse", session_id);
        list.tool_name = Some("agents.list_agents".to_string());
        list.tool_input = Some(json!({}));
        list.tool_response = Some(json!({
            "agents": [
                { "agent_name": "/root", "status": "running" },
                { "agent_name": target, "status": "pending_init" }
            ]
        }));
        handle_hook_for_runtime_at(&list, root, runtime_id, 1_000).unwrap();
        list.tool_response = Some(json!({
            "agents": [
                { "agent_name": "/root", "status": "running" },
                { "agent_name": target, "status": "running" }
            ]
        }));
        handle_hook_for_runtime_at(&list, root, runtime_id, 2_000).unwrap();

        let stopped = handle_hook_for_runtime_at(
            &input("Stop", session_id),
            root,
            runtime_id,
            1_000 + PENDING_INIT_GRACE_MILLIS,
        )
        .unwrap();
        assert_eq!(stopped["decision"].as_str(), Some("block"));
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            1
        );
        assert!(
            agent_marker_path(&session_state_dir(root, session_id), runtime_id, target).exists()
        );
        let trace = std::fs::read_to_string(crate::subagent::telemetry::trace_file(root)).unwrap();
        assert!(!trace.contains("pending_init_grace_elapsed"));
    }

    #[test]
    fn ledger_backed_stale_attempt_is_fenced_before_stop_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let session_id = "ledger-stale-session";
        let mut spawn = input("PreToolUse", session_id);
        spawn.cwd = Some("/repo".to_string());
        spawn.tool_name = Some("agents.spawn_agent".to_string());
        spawn.tool_input = Some(json!({
            "task_name": "stale_reader",
            "agent_type": "codey_deep_research",
            "fork_turns": "none",
            "message": delegation_message(json!({
                "id": "stale_reader",
                "why": "breadth",
                "visual": false,
                "root": "/repo",
                "read": ["backend/src"],
                "write": [],
                "checks": []
            }))
        }));
        assert_eq!(
            handle_hook_for_runtime_at(&spawn, root, runtime_id, 1_000).unwrap(),
            json!({})
        );
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            1
        );
        assert_eq!(
            handle_hook_for_runtime_at(&input("Stop", session_id), root, runtime_id, 2_000,)
                .unwrap()["decision"]
                .as_str(),
            Some("block")
        );

        let recovered = handle_hook_for_runtime_at(
            &input("Stop", session_id),
            root,
            runtime_id,
            2_000 + STOP_STALL_GRACE_MILLIS,
        )
        .unwrap();
        assert_eq!(recovered["decision"].as_str(), Some("block"));
        assert!(
            recovered["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("resolve_batch"))
        );
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            0
        );
    }

    #[test]
    fn collaboration_tool_output_is_bounded_on_unicode_boundaries() {
        let payload = "界".repeat(MAX_RENDERED_TOOL_RESULT_CHARS + 32);
        let rendered = render_tool_result(Some(&Value::String(payload)), "wait_agent");
        let (body, suffix) = rendered.split_once('\n').unwrap();

        assert_eq!(body.chars().count(), MAX_RENDERED_TOOL_RESULT_CHARS);
        assert!(body.chars().all(|character| character == '界'));
        assert!(suffix.contains("协作工具返回内容已截断"));
        assert!(suffix.contains("agents.list_agents"));
    }

    #[test]
    fn corrupted_active_state_fails_closed_then_recovers_after_grace() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let session_id = "corrupt-session";
        let mut start = input("SubagentStart", session_id);
        start.agent_id = Some("agent-a".to_string());
        handle_hook_for_runtime_at(&start, root, runtime_id, 1_000).unwrap();

        let session_dir = session_state_dir(root, session_id);
        let marker = agent_marker_path(&session_dir, runtime_id, "agent-a");
        fs::write(&marker, b"{").unwrap();

        let first_error =
            handle_hook_for_runtime_at(&input("Stop", session_id), root, runtime_id, 2_000)
                .unwrap_err();
        assert!(format!("{first_error:#}").contains("解析 Codex 子代理门禁状态失败"));

        let observed = session_auxiliary_path(&session_dir, runtime_id, STATE_ERROR_SINCE_FILE);
        assert_eq!(fs::read_to_string(&observed).unwrap(), "2000\n");
        assert!(marker.exists());

        let recovered = handle_hook_for_runtime_at(
            &input("Stop", session_id),
            root,
            runtime_id,
            2_000 + STOP_STALL_GRACE_MILLIS,
        )
        .unwrap();
        assert_eq!(recovered, json!({}));
        assert!(!marker.exists());
        assert!(!observed.exists());
    }

    #[test]
    fn healthy_active_state_clears_a_stale_corruption_observation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let session_id = "healthy-session";
        let mut start = input("SubagentStart", session_id);
        start.agent_id = Some("agent-a".to_string());
        handle_hook_for_runtime_at(&start, root, runtime_id, 1_000).unwrap();

        let session_dir = session_state_dir(root, session_id);
        let observed = session_auxiliary_path(&session_dir, runtime_id, STATE_ERROR_SINCE_FILE);
        write_observation_timestamp(&session_dir, &observed, 1_000).unwrap();

        let blocked = handle_hook_for_runtime_at(
            &input("Stop", session_id),
            root,
            runtime_id,
            1_000 + STOP_STALL_GRACE_MILLIS,
        )
        .unwrap();
        assert_eq!(blocked["decision"].as_str(), Some("block"));
        assert!(!observed.exists());
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            1
        );
    }

    #[test]
    fn repeated_interrupted_snapshots_do_not_extend_the_stall_grace() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let session_id = "unchanged-interrupt-session";
        let mut start = input("SubagentStart", session_id);
        start.agent_id = Some("agent-a".to_string());
        handle_hook_for_runtime_at(&start, root, runtime_id, 1_000).unwrap();

        assert_eq!(
            handle_hook_for_runtime_at(&input("Stop", session_id), root, runtime_id, 2_000)
                .unwrap()["decision"]
                .as_str(),
            Some("block")
        );
        let session_dir = session_state_dir(root, session_id);
        let stalled = session_auxiliary_path(&session_dir, runtime_id, STOP_BLOCKED_SINCE_FILE);
        assert_eq!(fs::read_to_string(&stalled).unwrap(), "2000\n");

        let mut wait = input("PostToolUse", session_id);
        wait.tool_name = Some("agents.wait_agent".to_string());
        wait.tool_response = Some(json!({
            "updates": [{ "agent_id": "agent-a", "status": "interrupted" }]
        }));
        handle_hook_for_runtime_at(&wait, root, runtime_id, 3_000).unwrap();
        assert!(!stalled.exists());

        handle_hook_for_runtime_at(&input("Stop", session_id), root, runtime_id, 4_000).unwrap();
        assert_eq!(fs::read_to_string(&stalled).unwrap(), "4000\n");
        handle_hook_for_runtime_at(&wait, root, runtime_id, 5_000).unwrap();
        assert_eq!(fs::read_to_string(&stalled).unwrap(), "4000\n");

        wait.tool_response = Some(json!({
            "updates": [{ "agent_id": "agent-a", "status": "running" }]
        }));
        handle_hook_for_runtime_at(&wait, root, runtime_id, 6_000).unwrap();
        assert!(!stalled.exists());
        handle_hook_for_runtime_at(&input("Stop", session_id), root, runtime_id, 7_000).unwrap();
        handle_hook_for_runtime_at(&wait, root, runtime_id, 8_000).unwrap();
        assert_eq!(fs::read_to_string(&stalled).unwrap(), "7000\n");

        assert_eq!(
            handle_hook_for_runtime_at(
                &input("Stop", session_id),
                root,
                runtime_id,
                7_000 + STOP_STALL_GRACE_MILLIS,
            )
            .unwrap(),
            json!({})
        );
    }

    #[test]
    fn stop_absolute_release_still_allows_later_stall_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let session_id = "absolute-stop-session";
        let mut start = input("SubagentStart", session_id);
        start.agent_id = Some("agent-a".to_string());
        handle_hook_for_runtime_at(&start, root, runtime_id, 1_000).unwrap();

        let first_blocked =
            handle_hook_for_runtime_at(&input("Stop", session_id), root, runtime_id, 2_000)
                .unwrap();
        assert_eq!(first_blocked["decision"].as_str(), Some("block"));
        let session_dir = session_state_dir(root, session_id);
        let absolute = session_auxiliary_path(&session_dir, runtime_id, STOP_ABSOLUTE_SINCE_FILE);
        assert_eq!(fs::read_to_string(&absolute).unwrap(), "2000\n");

        // 结构有效的 wait 响应只重置 10 分钟停滞计时，绝对计时保持不变。
        let mut wait = input("PostToolUse", session_id);
        wait.tool_name = Some("agents.wait_agent".to_string());
        wait.tool_response = Some(json!({ "timedout": true, "message": "still running" }));
        handle_hook_for_runtime_at(&wait, root, runtime_id, 3_000).unwrap();
        assert_eq!(fs::read_to_string(&absolute).unwrap(), "2000\n");

        // 持续到绝对上限前仍有有效等待结果，避免 10 分钟停滞窗口提前回收。
        let absolute_deadline = 2_000 + STOP_ABSOLUTE_GRACE_MILLIS;
        handle_hook_for_runtime_at(&wait, root, runtime_id, absolute_deadline - 1).unwrap();

        let released = handle_hook_for_runtime_at(
            &input("Stop", session_id),
            root,
            runtime_id,
            absolute_deadline,
        )
        .unwrap();
        assert_eq!(released, json!({}));
        // 绝对放行先在账本中 fence 活动 attempt，再清理旧 marker，避免
        // ledger-backed active count 在后续 Stop 中反复复活。
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            0
        );
        let issue = protocol_issue_reason(root, runtime_id, session_id)
            .unwrap()
            .unwrap();
        assert!(issue.contains("绝对上限"), "{issue}");

        // 后续 Stop 保持幂等，不会重新建立停滞窗口或恢复旧 attempt。
        let recovered = handle_hook_for_runtime_at(
            &input("Stop", session_id),
            root,
            runtime_id,
            absolute_deadline + STOP_STALL_GRACE_MILLIS,
        )
        .unwrap();
        assert_eq!(recovered, json!({}));
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, session_id).unwrap(),
            0
        );
        assert!(!absolute.exists());
    }

    #[test]
    fn fail_closed_denies_child_data_and_orchestration_but_allows_root_reporting() {
        let error = anyhow::anyhow!("账本损坏");

        for tool in ["apply_patch", "mcp__codey_fastctx__replace", "Bash"] {
            let mut child_write = input("PreToolUse", "session-a");
            child_write.agent_id = Some("agent-a".to_string());
            child_write.tool_name = Some(tool.to_string());
            let denied = fail_closed_output(&child_write, &error);
            assert_eq!(
                denied["hookSpecificOutput"]["permissionDecision"].as_str(),
                Some("deny"),
                "{tool}"
            );
        }

        for tool in ["read_file", "mcp__codey_fastctx__inspect_local_file"] {
            let mut child_read = input("PreToolUse", "session-a");
            child_read.agent_id = Some("agent-a".to_string());
            child_read.tool_name = Some(tool.to_string());
            assert_eq!(
                fail_closed_output(&child_read, &error)["hookSpecificOutput"]["permissionDecision"]
                    .as_str(),
                Some("deny"),
                "{tool}"
            );
        }

        for tool in ["agents.wait_agent", "agents.list_agents"] {
            let mut child_collaboration = input("PreToolUse", "session-a");
            child_collaboration.agent_id = Some("agent-a".to_string());
            child_collaboration.tool_name = Some(tool.to_string());
            assert_eq!(
                fail_closed_output(&child_collaboration, &error)["hookSpecificOutput"]
                    ["permissionDecision"]
                    .as_str(),
                Some("deny"),
                "{tool}"
            );
        }

        let mut report = input("PreToolUse", "session-a");
        report.agent_id = Some("agent-a".to_string());
        report.tool_name = Some("agents.send_message".to_string());
        report.tool_input = Some(json!({ "target": "/root", "message": "ledger error" }));
        assert_eq!(fail_closed_output(&report, &error), json!({}));

        for tool in ["agents.wait_agent", "agents.list_agents"] {
            let mut root_recovery = input("PreToolUse", "session-a");
            root_recovery.tool_name = Some(tool.to_string());
            assert_eq!(
                fail_closed_output(&root_recovery, &error),
                json!({}),
                "{tool}"
            );
        }
        for tool in [
            "agents.spawn_agent",
            "agents.followup_task",
            "agents.interrupt_agent",
            "agents.send_message",
            "read_file",
        ] {
            let mut root_denied = input("PreToolUse", "session-a");
            root_denied.tool_name = Some(tool.to_string());
            assert_eq!(
                fail_closed_output(&root_denied, &error)["hookSpecificOutput"]
                    ["permissionDecision"]
                    .as_str(),
                Some("deny"),
                "{tool}"
            );
        }

        let mut root_bash = input("PreToolUse", "session-a");
        root_bash.tool_name = Some("Bash".to_string());
        assert_eq!(
            fail_closed_output(&root_bash, &error)["hookSpecificOutput"]["permissionDecision"]
                .as_str(),
            Some("deny")
        );
    }

    #[test]
    fn unparsable_or_oversized_hook_input_is_denied_in_both_output_shapes() {
        let oversized = vec![b' '; (MAX_HOOK_INPUT_BYTES + 1) as usize];
        let denied = parse_hook_input(&oversized).unwrap_err();
        assert_eq!(
            denied["hookSpecificOutput"]["permissionDecision"].as_str(),
            Some("deny")
        );
        assert_eq!(denied["decision"].as_str(), Some("block"));
        assert!(denied["reason"].as_str().unwrap().contains("1 MiB"));
        assert_eq!(
            denied["hookSpecificOutput"]["permissionDecisionReason"].as_str(),
            denied["reason"].as_str()
        );

        let denied = parse_hook_input(b"{not-json").unwrap_err();
        assert_eq!(
            denied["hookSpecificOutput"]["permissionDecision"].as_str(),
            Some("deny")
        );
        assert_eq!(denied["decision"].as_str(), Some("block"));
        assert!(denied["reason"].as_str().unwrap().contains("JSON 解析失败"));

        let parsed = parse_hook_input(br#"{"hookEventName":"Stop","sessionId":"s"}"#).unwrap();
        assert_eq!(parsed.hook_event_name, "Stop");
        assert_eq!(parsed.session_id, "s");
    }

    #[test]
    fn non_terminal_or_unattributed_wait_updates_do_not_release_markers() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for (index, tool_response) in [
            json!({ "agent_id": "agent-a", "status": "partial" }),
            json!({ "agentId": "agent-a", "type": "MESSAGE" }),
            json!({ "status": "FINAL_ANSWER", "message": "done" }),
            json!({ "agent_id": "agent-a", "message": "FINAL_ANSWER" }),
        ]
        .into_iter()
        .enumerate()
        {
            let session_id = format!("session-{index}");
            let mut start = input("SubagentStart", &session_id);
            start.agent_id = Some("agent-a".to_string());
            handle_hook(&start, root).unwrap();

            let mut wait = input("PostToolUse", &session_id);
            wait.tool_name = Some("agents.wait_agent".to_string());
            wait.tool_response = Some(tool_response);
            let blocked = handle_hook(&wait, root).unwrap();

            assert_eq!(blocked["decision"].as_str(), Some("block"));
            assert_eq!(active_agent_count(root, &session_id).unwrap(), 1);
        }
    }

    #[test]
    fn interrupted_root_wait_preserves_live_session_gate_state() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        let mut child_wait = input("PostToolUse", "child-session");
        child_wait.agent_id = Some("agent-a".to_string());
        child_wait.tool_name = Some("agentswait_agent".to_string());
        assert_eq!(handle_hook(&child_wait, root).unwrap(), json!({}));

        for (index, tool_response) in [
            json!({ "output": "Wait interrupted by new input" }),
            json!({ "output": "Wait cancelled by user" }),
            json!({ "message": "Wait manually stopped" }),
            json!({ "kind": "steered_input" }),
            json!({ "interrupted_by_user_input": true }),
            json!({ "canceled_by_user": true }),
            json!({ "result": { "interrupted_by_user": true } }),
        ]
        .into_iter()
        .enumerate()
        {
            let session_id = format!("interrupted-session-{index}");
            for agent_id in ["agent-a", "agent-b"] {
                let mut start = input("SubagentStart", &session_id);
                start.agent_id = Some(agent_id.to_string());
                handle_hook(&start, root).unwrap();
            }

            let mut interrupted_wait = input("PostToolUse", &session_id);
            interrupted_wait.tool_name = Some("agents__wait_agent".to_string());
            interrupted_wait.tool_response = Some(tool_response);
            assert_eq!(
                handle_hook(&interrupted_wait, root).unwrap()["decision"].as_str(),
                Some("block")
            );
            assert_eq!(active_agent_count(root, &session_id).unwrap(), 2);

            let mut root_patch = input("PreToolUse", &session_id);
            root_patch.tool_name = Some("apply_patch".to_string());
            assert_eq!(
                handle_hook(&root_patch, root).unwrap()["hookSpecificOutput"]["permissionDecision"]
                    .as_str(),
                Some("deny")
            );
            assert_eq!(
                handle_hook(&input("Stop", &session_id), root).unwrap()["decision"].as_str(),
                Some("block")
            );

            for agent_id in ["agent-a", "agent-b"] {
                let mut stop = input("SubagentStop", &session_id);
                stop.agent_id = Some(agent_id.to_string());
                handle_hook(&stop, root).unwrap();
            }
            assert_eq!(active_agent_count(root, &session_id).unwrap(), 0);
            assert_eq!(handle_hook(&root_patch, root).unwrap(), json!({}));
            assert_eq!(
                handle_hook(&input("Stop", &session_id), root).unwrap(),
                json!({})
            );
        }

        let mut start = input("SubagentStart", "active-session");
        start.agent_id = Some("agent-a".to_string());
        handle_hook(&start, root).unwrap();
        let mut completed_wait = input("PostToolUse", "active-session");
        completed_wait.tool_name = Some("agents__wait_agent".to_string());
        completed_wait.tool_response = Some(json!({
            "message": "Wait completed after an agent update"
        }));
        assert_eq!(
            handle_hook(&completed_wait, root).unwrap()["decision"].as_str(),
            Some("block")
        );
    }

    #[test]
    fn ordinary_agent_messages_cannot_clear_the_session_gate() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let mut start = input("SubagentStart", "session-a");
        start.agent_id = Some("agent-a".to_string());
        handle_hook_for_runtime(&start, root, runtime_id).unwrap();

        let mut wait = input("PostToolUse", "session-a");
        wait.tool_name = Some("agents.wait_agent".to_string());
        wait.tool_response = Some(json!({
            "updates": [{
                "agent_id": "agent-a",
                "type": "MESSAGE",
                "message": "Document the manual stop procedure before continuing",
                "details": { "interrupted_by_user_input": true }
            }]
        }));

        let blocked = handle_hook_for_runtime(&wait, root, runtime_id).unwrap();
        assert_eq!(blocked["decision"].as_str(), Some("block"));
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, "session-a").unwrap(),
            1
        );
    }

    #[test]
    fn task_body_decryption_message_requests_one_active_restatement() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let mut start = input("SubagentStart", "session-a");
        start.agent_id = Some("visual-a".to_string());
        handle_hook_for_runtime(&start, root, runtime_id).unwrap();

        let mut wait = input("PostToolUse", "session-a");
        wait.tool_name = Some("agents.wait_agent".to_string());
        wait.tool_response = Some(json!({
            "updates": [{
                "agent_id": "visual-a",
                "status": "MESSAGE",
                "message": "任务正文未能解密，无法开始视觉核验。"
            }]
        }));

        let blocked = handle_hook_for_runtime(&wait, root, runtime_id).unwrap();
        assert_eq!(blocked["decision"].as_str(), Some("block"));
        let reason = blocked["reason"].as_str().unwrap();
        assert!(reason.contains("`agents.send_message`"));
        assert!(reason.contains("只重述一次"));
        assert!(reason.contains("不要中断该代理"));
        assert!(reason.contains("禁止循环重试"));
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, "session-a").unwrap(),
            1
        );
    }

    #[test]
    fn business_payloads_cannot_impersonate_list_or_wait_protocol_envelopes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runtime_id = "runtime-a";
        let mut start = input("SubagentStart", "session-a");
        start.agent_id = Some("agent-a".to_string());
        handle_hook_for_runtime(&start, root, runtime_id).unwrap();

        let mut list = input("PostToolUse", "session-a");
        list.tool_name = Some("agents.list_agents".to_string());
        list.tool_input = Some(json!({}));
        list.tool_response = Some(json!({
            "output": {
                "agents": [
                    { "agent_name": "/root", "status": "running" },
                    { "agent_name": "/root/agent-a", "status": "completed" }
                ]
            }
        }));
        assert_eq!(
            handle_hook_for_runtime(&list, root, runtime_id).unwrap()["decision"].as_str(),
            Some("block")
        );
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, "session-a").unwrap(),
            1
        );

        let mut wait = input("PostToolUse", "session-a");
        wait.tool_name = Some("agents.wait_agent".to_string());
        wait.tool_response = Some(json!({
            "updates": [{
                "agent_id": "agent-a",
                "type": "MESSAGE",
                "payload": { "status": "completed" }
            }]
        }));
        assert_eq!(
            handle_hook_for_runtime(&wait, root, runtime_id).unwrap()["decision"].as_str(),
            Some("block")
        );
        assert_eq!(
            active_agent_count_for_runtime(root, runtime_id, "session-a").unwrap(),
            1
        );
    }

    #[test]
    fn runtime_generations_fence_stale_markers_and_late_events() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut start = input("SubagentStart", "session-a");
        start.agent_id = Some("agent-a".to_string());

        handle_hook_for_runtime(&start, root, "runtime-old").unwrap();
        assert_eq!(
            active_agent_count_for_runtime(root, "runtime-old", "session-a").unwrap(),
            1
        );
        assert_eq!(
            active_agent_count_for_runtime(root, "runtime-new", "session-a").unwrap(),
            0
        );

        handle_hook_for_runtime(&start, root, "runtime-new").unwrap();
        let session_dir = session_state_dir(root, "session-a");
        let marker_path = agent_marker_path(&session_dir, "runtime-new", "agent-a");
        let marker: ActiveMarker =
            serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
        assert_eq!(marker.schema_version, ACTIVE_MARKER_SCHEMA_VERSION);
        assert_eq!(marker.runtime_id_hash, hash_component("runtime-new"));
        assert!(marker.started_at_ms > 0);

        let mut late_old_stop = input("SubagentStop", "session-a");
        late_old_stop.agent_id = Some("agent-a".to_string());
        handle_hook_for_runtime(&late_old_stop, root, "runtime-old").unwrap();
        assert_eq!(
            active_agent_count_for_runtime(root, "runtime-new", "session-a").unwrap(),
            1
        );

        handle_hook_for_runtime(&input("SessionEnd", "session-a"), root, "runtime-old").unwrap();
        assert_eq!(
            active_agent_count_for_runtime(root, "runtime-new", "session-a").unwrap(),
            1
        );

        let mut root_patch = input("PreToolUse", "session-a");
        root_patch.tool_name = Some("apply_patch".to_string());
        assert_eq!(
            handle_hook_for_runtime(&root_patch, root, "runtime-new").unwrap()
                ["hookSpecificOutput"]["permissionDecision"]
                .as_str(),
            Some("deny")
        );
    }

    #[test]
    fn unverifiable_legacy_markers_do_not_block_a_versioned_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let session_dir = session_state_dir(root, "session-a");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(
            session_dir.join(format!("{}.active", hash_component("agent-a"))),
            b"active\n",
        )
        .unwrap();

        assert_eq!(
            active_agent_count_for_runtime(root, "runtime-new", "session-a").unwrap(),
            0
        );
    }

    #[test]
    fn late_subagent_stop_after_interrupted_wait_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut start = input("SubagentStart", "session-a");
        start.agent_id = Some("agent-a".to_string());
        handle_hook(&start, root).unwrap();

        let mut interrupted_wait = input("PostToolUse", "session-a");
        interrupted_wait.tool_name = Some("agents.wait_agent".to_string());
        interrupted_wait.tool_response = Some(json!({
            "output": "Wait interrupted by new user input"
        }));
        assert_eq!(
            handle_hook(&interrupted_wait, root).unwrap()["decision"].as_str(),
            Some("block")
        );
        assert_eq!(active_agent_count(root, "session-a").unwrap(), 1);

        let mut late_stop = input("SubagentStop", "session-a");
        late_stop.agent_id = Some("agent-a".to_string());
        assert_eq!(handle_hook(&late_stop, root).unwrap(), json!({}));
        assert_eq!(active_agent_count(root, "session-a").unwrap(), 0);
    }

    #[test]
    fn gate_state_is_isolated_by_session() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut start = input("SubagentStart", "session-a");
        start.agent_id = Some("agent-a".to_string());
        handle_hook(&start, root).unwrap();

        let mut other = input("PreToolUse", "session-b");
        other.tool_name = Some("apply_patch".to_string());
        assert_eq!(handle_hook(&other, root).unwrap(), json!({}));
    }

    #[test]
    fn collaboration_tool_aliases_are_allowed() {
        for tool in [
            "Agent",
            "spawn_agent",
            "agents__wait_agent",
            "agentswait_agent",
            "agents.list_agents",
            "agents/interrupt_agent",
            "agents::send_message",
            "followup_task",
            "functions.wait",
            "functions/wait",
            "functions:wait",
            "functions__wait",
            "functions_wait",
        ] {
            assert!(is_collaboration_tool(tool), "{tool}");
        }
        assert!(!is_collaboration_tool("functions.exec"));
        assert!(!is_collaboration_tool("update_plan"));
        assert!(is_wait_agent_tool("functions.wait"));
        assert!(is_wait_agent_tool("functions__wait"));
        assert!(!is_wait_agent_tool("functions.exec"));
        for tool in [
            "Agent",
            "agents.spawn_agent",
            "agents__spawn_agent",
            "agentsspawn_agent",
        ] {
            assert_eq!(
                crate::subagent::rules::classify_tool(tool),
                crate::subagent::rules::ToolClass::Spawn,
                "{tool}"
            );
        }
        assert_ne!(
            crate::subagent::rules::classify_tool("agents.wait_agent"),
            crate::subagent::rules::ToolClass::Spawn
        );
    }

    #[test]
    fn gate_only_activates_for_a_codey_runtime() {
        assert!(!runtime_gate_is_active(None));
        assert!(!runtime_gate_is_active(Some(OsStr::new("0"))));
        assert!(!runtime_gate_is_active(Some(OsStr::new("true"))));
        assert!(runtime_gate_is_active(Some(OsStr::new("1"))));
    }

    #[test]
    fn windows_hook_executable_paths_translate_to_wsl_mounts() {
        assert_eq!(
            windows_path_to_wsl(r"C:\Program Files\Codey\codey.exe").as_deref(),
            Some("/mnt/c/Program Files/Codey/codey.exe")
        );
        assert_eq!(
            windows_path_to_wsl(r"\\?\D:\Apps\Codey.exe").as_deref(),
            Some("/mnt/d/Apps/Codey.exe")
        );
        assert_eq!(windows_path_to_wsl("/Applications/Codey"), None);
    }

    #[test]
    fn windows_hook_executable_paths_are_powershell_invocations() {
        assert_eq!(
            powershell_executable_invocation(Path::new(r"C:\Program Files\Codey\codey.exe")),
            r#"& 'C:\Program Files\Codey\codey.exe'"#
        );
        assert_eq!(
            powershell_executable_invocation(Path::new(
                r"C:\Users\O'Brien\$Codey` Preview\codey.exe"
            )),
            r#"& 'C:\Users\O''Brien\$Codey` Preview\codey.exe'"#
        );
    }

    #[test]
    fn trust_hash_is_canonical_and_definition_sensitive() {
        let command = "'/tmp/codey' --codey-subagent-gate-hook";
        let first = hook_trust_hash("pre_tool_use", Some("*"), command, 5);
        let same = hook_trust_hash("pre_tool_use", Some("*"), command, 5);
        let changed = hook_trust_hash("stop", None, "codey --gate", 5);

        assert_eq!(first, same);
        assert_eq!(
            first,
            "sha256:55551dee38305185b5687a38eac9f0301b5e77da84abe693bc6c905fcfd767a5"
        );
        assert!(first.starts_with("sha256:"));
        assert_eq!(first.len(), "sha256:".len() + 64);
        assert_ne!(first, changed);
    }
}
