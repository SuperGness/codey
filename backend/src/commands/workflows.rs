use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use super::AppState;
use crate::workflow::domain::{
    ApprovalStatus, NodeRecord, NodeRole, NodeStatus, ReviewerVerdict, RouteMode, RunRecord,
    RunStatus, parse_reviewer_verdict,
};
use crate::workflow::engine::{ApprovalReplyRequest, RetryNodeRequest, run_blocking};
use crate::workflow::host::{BoundStartRequest, PermissionSnapshot, StartResult};
use crate::workflow::journal::RunSummaryStats;

const PROTOCOL_VERSION: u64 = 1;
const DEFAULT_PAGE_LIMIT: usize = 50;
const MAX_ARTIFACT_PAGE_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CapabilitiesArgs {
    #[serde(default)]
    source: String,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    cwd_hint: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartArgs {
    schema_version: u64,
    protocol_version: u64,
    command_id: String,
    #[serde(default)]
    thread_id: Option<String>,
    cwd: String,
    permission_snapshot: PermissionSnapshot,
    text: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    route: Option<RouteMode>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SteerArgs {
    schema_version: u64,
    protocol_version: u64,
    command_id: String,
    run_id: String,
    expected_revision: u64,
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunMutationArgs {
    command_id: String,
    run_id: String,
    expected_revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetryArgs {
    command_id: String,
    run_id: String,
    node_id: String,
    expected_revision: u64,
    #[serde(default)]
    replacement_payload: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReplyArgs {
    command_id: String,
    run_id: String,
    expected_revision: u64,
    #[serde(alias = "interactionId")]
    approval_id: String,
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    reply: Value,
}

pub async fn capabilities(state: &Arc<AppState>, args: Value) -> Result<Value, String> {
    let args: CapabilitiesArgs = serde_json::from_value(args).unwrap_or(CapabilitiesArgs {
        source: String::new(),
        thread_id: None,
        cwd_hint: None,
    });
    let config = state.config.read().await.clone();
    let service_capabilities = state.workflow.service().capabilities().await;
    let health = state.workflow.proxy_health().await;
    let enabled = config.workflow.enabled && config.workflow.global_mode;
    let engine_epoch = health
        .engine_epoch
        .clone()
        .unwrap_or_else(|| "offline".to_string());
    if args.source != "codex_composer" {
        let available = enabled && service_capabilities.available && health.accepting;
        return Ok(json!({
            "enabled": config.workflow.enabled,
            "available": available,
            "readOnly": false,
            "schemaVersion": service_capabilities.schema_version.unwrap_or(1),
            "engineEpoch": engine_epoch,
            "unavailableReason": if available {
                Value::Null
            } else {
                json!(health.reason.or(service_capabilities.unavailable_reason))
            },
            "actions": {
                "artifact": service_capabilities.artifact_paging,
                "pause": available,
                "resume": available,
                "cancel": available,
                "retryNode": available,
                "replyInteraction": available && service_capabilities.approvals,
            },
            "protocol": {
                "canCancel": true,
                "canCreateContext": true,
                "canQueryRun": true,
                "canReplyToApproval": service_capabilities.approvals,
                "canResumeContext": true,
                "canStreamStableEvents": true,
            },
        }));
    }

    if !enabled || !service_capabilities.available || !health.accepting {
        return Ok(json!({
            "status": "unavailable",
            "enabled": enabled,
            "bridgeHealthy": true,
            "proxyHealthy": health.accepting,
            "schemaVersion": PROTOCOL_VERSION,
            "protocolVersion": PROTOCOL_VERSION,
            "reason": health
                .reason
                .or(service_capabilities.unavailable_reason)
                .unwrap_or_else(|| "workflow mode is unavailable".to_string()),
            "capabilities": {
                "composerTakeover": false,
                "start": false,
                "steer": false,
                "bypassAudit": true,
            },
        }));
    }

    match state
        .workflow
        .resolve_composer_context(args.thread_id.as_deref(), args.cwd_hint.as_deref())
        .await
    {
        Ok(context) => {
            let active = context.active_run.as_ref().map(|run| {
                json!({
                    "active": true,
                    "runId": run.id,
                    "generation": run.generation,
                    "revision": run.revision,
                    "originTurnId": context
                        .latest_turn_id
                        .clone()
                        .unwrap_or_else(|| format!("workflow:{}", run.id)),
                })
            });
            Ok(json!({
                "status": "ok",
                "enabled": true,
                "bridgeHealthy": true,
                "proxyHealthy": true,
                "schemaVersion": PROTOCOL_VERSION,
                "protocolVersion": PROTOCOL_VERSION,
                "engineEpoch": engine_epoch,
                "cwd": context.cwd,
                "permissionSnapshot": context.permission_snapshot,
                "activeWorkflow": active,
                "capabilities": {
                    "composerTakeover": true,
                    "start": true,
                    "steer": true,
                    "bypassAudit": true,
                },
            }))
        }
        Err(error) => Ok(json!({
            "status": "unavailable",
            "enabled": true,
            "bridgeHealthy": true,
            "proxyHealthy": true,
            "schemaVersion": PROTOCOL_VERSION,
            "protocolVersion": PROTOCOL_VERSION,
            "reason": error.to_string(),
            "capabilities": {
                "composerTakeover": false,
                "start": false,
                "steer": false,
                "bypassAudit": true,
            },
        })),
    }
}

pub async fn start(state: &Arc<AppState>, args: Value) -> Result<Value, String> {
    let args: StartArgs = parse(args)?;
    validate_protocol(args.schema_version, args.protocol_version)?;
    if args.text.trim().is_empty() {
        return Err("工作流请求正文不能为空".to_string());
    }
    if !args.permission_snapshot.resolved
        || args.permission_snapshot.snapshot_id.trim().is_empty()
        || args.permission_snapshot.hash.trim().is_empty()
    {
        return Err("工作流权限快照无效".to_string());
    }

    if let Some(result) = state
        .workflow
        .replay_start(
            &args.command_id,
            &args.text,
            &args.cwd,
            &args.permission_snapshot.hash,
        )
        .await
        .map_err(|error| error.to_string())?
    {
        let origin_thread_id = result
            .run
            .input
            .get("originThreadId")
            .and_then(Value::as_str)
            .ok_or_else(|| "已接纳的工作流缺少 origin task".to_string())?;
        let binding = match args
            .thread_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            Some(thread_id) if thread_id != origin_thread_id => {
                return Err("重复命令绑定到了不同的 Codex 任务".to_string());
            }
            Some(_) => None,
            None => {
                if !bind_origin_thread(state, origin_thread_id).await? {
                    return Err("工作流已接纳，但无法重新确认当前 Codex 任务绑定".to_string());
                }
                Some(json!({ "confirmed": true, "threadId": origin_thread_id }))
            }
        };
        return start_response(state, result, binding).await;
    }

    let mut binding = None;
    let origin_thread_id = if let Some(thread_id) = args
        .thread_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        thread_id.to_string()
    } else {
        let origin = state
            .workflow
            .create_origin_for_snapshot(&args.permission_snapshot.snapshot_id, &args.cwd)
            .await
            .map_err(|error| error.to_string())?;
        match bind_origin_thread(state, &origin.thread_id).await {
            Ok(true) => {
                binding = Some(json!({
                    "confirmed": true,
                    "threadId": origin.thread_id,
                }));
                origin.thread_id
            }
            Ok(false) | Err(_) => {
                state.workflow.discard_origin(&origin.thread_id).await;
                return Ok(json!({
                    "accepted": false,
                    "safeToSendNative": true,
                    "reason": "origin task binding could not be confirmed",
                }));
            }
        }
    };

    let result = match state
        .workflow
        .start_bound(BoundStartRequest {
            command_id: args.command_id,
            text: args.text,
            origin_thread_id: origin_thread_id.clone(),
            cwd: args.cwd,
            permission_snapshot_id: args.permission_snapshot.snapshot_id,
            source: args.source,
            requested_route: args.route,
        })
        .await
    {
        Ok(result) => result,
        Err(error) => {
            let accepted = state
                .workflow
                .active_run_for_origin(&origin_thread_id)
                .await
                .map_err(|lookup_error| lookup_error.to_string())?
                .is_some();
            if !accepted {
                return Ok(json!({
                    "accepted": false,
                    "safeToSendNative": true,
                    "reason": error.to_string(),
                }));
            }
            return Err(error.to_string());
        }
    };
    start_response(state, result, binding).await
}

async fn start_response(
    state: &Arc<AppState>,
    result: StartResult,
    binding: Option<Value>,
) -> Result<Value, String> {
    let health = state.workflow.proxy_health().await;
    let run = result.run;
    Ok(json!({
        "status": "ok",
        "accepted": true,
        "durableAck": true,
        "commandId": result.ack.command_id,
        "runId": result.ack.run_id,
        "generation": run.generation,
        "revision": run.revision,
        "sequence": result.ack.workflow_sequence,
        "duplicate": result.ack.duplicate,
        "engineEpoch": health.engine_epoch.unwrap_or_else(|| "offline".to_string()),
        "binding": binding,
    }))
}

pub async fn steer(state: &Arc<AppState>, args: Value) -> Result<Value, String> {
    let args: SteerArgs = parse(args)?;
    validate_protocol(args.schema_version, args.protocol_version)?;
    let ack = state
        .workflow
        .steer(
            args.command_id,
            args.run_id.clone(),
            args.expected_revision,
            args.text,
        )
        .await
        .map_err(|error| error.to_string())?;
    mutation_response(state, &args.run_id, ack).await
}

pub async fn list(state: &Arc<AppState>, args: Value) -> Result<Value, String> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(DEFAULT_PAGE_LIMIT)
        .clamp(1, 200);
    let thread_id = args
        .get("threadId")
        .or_else(|| args.get("originThreadId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let runs = match thread_id {
        Some(thread_id) => {
            state
                .workflow
                .list_runs_for_origin_with_stats(thread_id, limit)
                .await
        }
        None => state.workflow.list_runs_with_stats(limit).await,
    }
    .map_err(|error| error.to_string())?;
    let summaries = runs
        .iter()
        .map(|(run, stats)| run_summary(run, stats))
        .collect::<Vec<_>>();
    let health = state.workflow.proxy_health().await;
    Ok(json!({
        "engineEpoch": health.engine_epoch.unwrap_or_else(|| "offline".to_string()),
        "runs": summaries,
        "threadId": thread_id,
        "nextCursor": Value::Null,
    }))
}

pub async fn get(state: &Arc<AppState>, args: Value) -> Result<Value, String> {
    let run_id = required_string(&args, "runId")?;
    snapshot(state, &run_id).await
}

pub async fn events(state: &Arc<AppState>, args: Value) -> Result<Value, String> {
    let run_id = required_string(&args, "runId")?;
    let after_sequence = args
        .get("afterSequence")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(200)
        .clamp(1, 500);
    let details = state
        .workflow
        .service()
        .get(&run_id)
        .await
        .map_err(|error| error.to_string())?;
    let events = state
        .workflow
        .service()
        .events(&run_id, after_sequence, limit)
        .await
        .map_err(|error| error.to_string())?;
    let journal = state
        .workflow
        .journal()
        .ok_or_else(|| "工作流 Journal 不可用".to_string())?
        .clone();
    let latest_run_id = run_id.clone();
    let latest = run_blocking(move || journal.latest_sequence(&latest_run_id))
        .await
        .map_err(|error| error.to_string())?;
    let health = state.workflow.proxy_health().await;
    let wire_events = events
        .iter()
        .map(|event| {
            json!({
                "eventId": event.event_id,
                "runId": event.run_id,
                "generation": details.run.generation,
                "revision": details.run.revision,
                "sequence": event.workflow_sequence,
                "type": event.kind,
                "severity": event_severity(&event.kind),
                "nodeId": event.payload.get("nodeId"),
                "attemptId": event.payload.get("attemptId"),
                "createdAt": event.created_at_ms,
                "summary": event_summary(event),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "runId": run_id,
        "engineEpoch": health.engine_epoch.unwrap_or_else(|| "offline".to_string()),
        "generation": details.run.generation,
        "revision": details.run.revision,
        "afterSequence": after_sequence,
        "latestSequence": latest,
        "events": wire_events,
        "hasMore": latest > wire_events.last().and_then(|event| event.get("sequence")).and_then(Value::as_u64).unwrap_or(after_sequence),
        "resetRequired": false,
    }))
}

pub async fn artifact(state: &Arc<AppState>, args: Value) -> Result<Value, String> {
    let run_id = required_string(&args, "runId")?;
    let artifact_id = required_string(&args, "artifactId")?;
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(16_384)
        .clamp(1, MAX_ARTIFACT_PAGE_BYTES);
    let artifact = state
        .workflow
        .service()
        .artifact(&run_id, &artifact_id)
        .await
        .map_err(|error| error.to_string())?;
    let raw_total = artifact.bytes.len();
    let raw_start = offset.min(raw_total);
    let raw_end = raw_start.saturating_add(limit).min(raw_total);
    let text_page = is_text_mime(&artifact.metadata.mime_type)
        .then(|| redacted_text_page(&artifact.bytes, offset, limit));
    let (page_offset, total, text, next_offset) = match text_page {
        Some(page) => (
            page.offset,
            page.total_bytes,
            Some(page.text),
            page.next_offset,
        ),
        None => (
            raw_start,
            raw_total,
            None,
            (raw_end < raw_total).then_some(raw_end),
        ),
    };
    Ok(json!({
        "artifact": artifact_wire(&artifact.metadata, 0),
        "offset": page_offset,
        "limit": limit,
        "totalBytes": total,
        "mimeType": artifact.metadata.mime_type,
        "text": text,
        "encoding": text.as_ref().map(|_| "utf-8"),
        "truncated": next_offset.is_some(),
        "nextOffset": next_offset,
    }))
}

pub async fn mutate(state: &Arc<AppState>, action: &str, args: Value) -> Result<Value, String> {
    let args: RunMutationArgs = parse(args)?;
    let ack = state
        .workflow
        .run_command(
            action,
            args.command_id,
            args.run_id.clone(),
            args.expected_revision,
        )
        .await
        .map_err(|error| error.to_string())?;
    mutation_response(state, &args.run_id, ack).await
}

pub async fn retry_node(state: &Arc<AppState>, args: Value) -> Result<Value, String> {
    let args: RetryArgs = parse(args)?;
    let run_id = args.run_id.clone();
    let ack = state
        .workflow
        .retry_node(
            RetryNodeRequest {
                command_id: args.command_id,
                run_id: args.run_id,
                node_id: args.node_id,
                replacement_payload: args.replacement_payload,
            },
            args.expected_revision,
        )
        .await
        .map_err(|error| error.to_string())?;
    mutation_response(state, &run_id, ack).await
}

pub async fn reply_interaction(state: &Arc<AppState>, args: Value) -> Result<Value, String> {
    let args: ReplyArgs = parse(args)?;
    let run_id = args.run_id.clone();
    let approved = args.decision.as_deref() == Some("approved")
        || args.reply.get("decision").and_then(Value::as_str) == Some("approved");
    let ack = state
        .workflow
        .reply_approval(
            ApprovalReplyRequest {
                command_id: args.command_id,
                run_id: args.run_id,
                approval_id: args.approval_id,
                approved,
                response: args.reply,
            },
            args.expected_revision,
        )
        .await
        .map_err(|error| error.to_string())?;
    mutation_response(state, &run_id, ack).await
}

pub async fn bypass_audit(_state: &Arc<AppState>, args: Value) -> Result<Value, String> {
    let metadata = json!({
        "reason": args.get("reason").and_then(Value::as_str).unwrap_or("unknown"),
        "source": args.get("source").and_then(Value::as_str).unwrap_or("unknown"),
        "threadIdPresent": args.get("threadId").is_some_and(|value| !value.is_null()),
        "hadAttachment": args.get("hadAttachment").and_then(Value::as_bool).unwrap_or(false),
        "hadVoice": args.get("hadVoice").and_then(Value::as_bool).unwrap_or(false),
        "wasSlashCommand": args.get("wasSlashCommand").and_then(Value::as_bool).unwrap_or(false),
    });
    crate::error_log::record_failure_with_metadata(
        "workflow_bypass",
        "composer_native_fallback",
        "request bypassed workflow admission",
        crate::error_log::FailureMetadata::default(),
        metadata,
    );
    Ok(json!({ "status": "ok" }))
}

async fn mutation_response(
    state: &Arc<AppState>,
    run_id: &str,
    ack: crate::workflow::domain::DurableAck,
) -> Result<Value, String> {
    let snapshot = snapshot(state, run_id).await?;
    let health = state.workflow.proxy_health().await;
    Ok(json!({
        "accepted": true,
        "durableAck": true,
        "commandId": ack.command_id,
        "runId": ack.run_id,
        "sequence": ack.workflow_sequence,
        "duplicate": ack.duplicate,
        "engineEpoch": health.engine_epoch.unwrap_or_else(|| "offline".to_string()),
        "generation": snapshot.get("generation"),
        "revision": snapshot.get("revision"),
        "state": snapshot.get("run").and_then(|run| run.get("state")),
        "snapshot": snapshot,
    }))
}

async fn snapshot(state: &Arc<AppState>, run_id: &str) -> Result<Value, String> {
    let details = state
        .workflow
        .service()
        .get(run_id)
        .await
        .map_err(|error| error.to_string())?;
    let journal = state
        .workflow
        .journal()
        .ok_or_else(|| "工作流 Journal 不可用".to_string())?
        .clone();
    let journal_run_id = run_id.to_string();
    let (attempts, approvals, sequence) = run_blocking(move || {
        Ok((
            journal.attempts(&journal_run_id)?,
            journal.approvals(&journal_run_id)?,
            journal.latest_sequence(&journal_run_id)?,
        ))
    })
    .await
    .map_err(|error| error.to_string())?;
    let artifacts = state
        .workflow
        .service()
        .artifacts(run_id, None, 200)
        .await
        .map_err(|error| error.to_string())?;
    let health = state.workflow.proxy_health().await;
    let stats = summary_stats_from_nodes(sequence, &details.nodes);
    let run_wire = run_summary(&details.run, &stats);
    let nodes = details
        .nodes
        .iter()
        .map(|node| {
            json!({
                "nodeId": node.id,
                "runId": node.run_id,
                "generation": node.generation,
                "revision": node.revision,
                "title": node.label,
                "kind": node.role,
                "role": node.role,
                "state": node.status,
                "readOnly": !node.repo_writer,
                "dependencies": node.dependencies,
                "attemptCount": node.attempt_count,
                "executor": node.thread_id,
            })
        })
        .collect::<Vec<_>>();
    let attempt_wire = attempts
        .iter()
        .map(|attempt| {
            json!({
                "attemptId": attempt.id,
                "runId": attempt.run_id,
                "nodeId": attempt.node_id,
                "generation": attempt.generation,
                "number": attempt.number,
                "state": attempt.status,
                "leaseEpoch": attempt.lease_epoch,
                "startedAt": attempt.started_at_ms,
                "completedAt": attempt.completed_at_ms,
                "errorMessage": attempt.error,
                "sideEffectState": if attempt.is_write { "started" } else { "none" },
            })
        })
        .collect::<Vec<_>>();
    let artifact_wire = artifacts
        .items
        .iter()
        .map(|artifact| artifact_wire(artifact, details.run.generation))
        .collect::<Vec<_>>();
    let approval_wire = approvals
        .iter()
        .map(|approval| approval_wire(approval, details.run.generation, details.run.revision))
        .collect::<Vec<_>>();
    let interactions = approvals
        .iter()
        .map(|approval| {
            json!({
                "interactionId": approval.id,
                "runId": approval.run_id,
                "generation": details.run.generation,
                "nodeId": approval.node_id,
                "requestRevision": details.run.revision,
                "kind": "approval",
                "state": if approval.status == ApprovalStatus::Pending { "pending" } else { "answered" },
                "title": "工作流审批",
                "prompt": approval.prompt,
                "createdAt": approval.requested_at_ms,
            })
        })
        .collect::<Vec<_>>();
    let acceptance = acceptance_gate(&details.run, &details.nodes);
    Ok(json!({
        "engineEpoch": health.engine_epoch.unwrap_or_else(|| "offline".to_string()),
        "generation": details.run.generation,
        "revision": details.run.revision,
        "sequence": sequence,
        "run": run_wire,
        "nodes": nodes,
        "attempts": attempt_wire,
        "artifacts": artifact_wire,
        "approvals": approval_wire,
        "interactions": interactions,
        "acceptanceGate": acceptance,
    }))
}

fn run_summary(run: &RunRecord, stats: &RunSummaryStats) -> Value {
    json!({
        "runId": run.id,
        "generation": run.generation,
        "revision": run.revision,
        "state": run.status,
        "title": run.input.get("title").and_then(Value::as_str).unwrap_or("未命名工作流"),
        "mode": run.route,
        "risk": match run.policy.workspace_risk.level {
            crate::workflow::domain::WorkspaceRiskLevel::Clean => "low",
            crate::workflow::domain::WorkspaceRiskLevel::Dirty => "medium",
            crate::workflow::domain::WorkspaceRiskLevel::HighRisk => "critical",
        },
        "profile": run.input.get("profile"),
        "objective": run.input.get("originalRequest"),
        "originThreadId": run.input.get("originThreadId"),
        "latestSequence": stats.latest_sequence,
        "blockedReason": run.error,
        "createdAt": run.created_at_ms,
        "updatedAt": run.updated_at_ms,
        "resumable": run_is_resumable(run.status, stats),
        "progress": {
            "completedNodes": stats.completed_nodes,
            "failedNodes": stats.failed_nodes,
            "runningNodes": stats.running_nodes,
            "totalNodes": stats.total_nodes,
            "waitingNodes": stats.total_nodes.saturating_sub(stats.completed_nodes + stats.running_nodes),
        },
    })
}

fn summary_stats_from_nodes(latest_sequence: u64, nodes: &[NodeRecord]) -> RunSummaryStats {
    RunSummaryStats {
        latest_sequence,
        total_nodes: nodes.len(),
        completed_nodes: nodes
            .iter()
            .filter(|node| node.status.is_terminal())
            .count(),
        failed_nodes: nodes
            .iter()
            .filter(|node| matches!(node.status, NodeStatus::Failed | NodeStatus::UnknownOutcome))
            .count(),
        running_nodes: nodes
            .iter()
            .filter(|node| matches!(node.status, NodeStatus::Running | NodeStatus::Leased))
            .count(),
        ready_non_final_nodes: nodes
            .iter()
            .filter(|node| node.status == NodeStatus::Ready && node.role != NodeRole::FinalDelivery)
            .count(),
        attention_blockers: nodes
            .iter()
            .filter(|node| {
                matches!(
                    node.status,
                    NodeStatus::Failed
                        | NodeStatus::UnknownOutcome
                        | NodeStatus::WaitingApproval
                        | NodeStatus::Running
                        | NodeStatus::Leased
                        | NodeStatus::Canceled
                )
            })
            .count(),
    }
}

fn run_is_resumable(status: RunStatus, stats: &RunSummaryStats) -> bool {
    status == RunStatus::Paused
        || (status == RunStatus::NeedsAttention
            && stats.attention_blockers == 0
            && stats.ready_non_final_nodes > 0)
}

fn artifact_wire(artifact: &crate::workflow::domain::ArtifactMetadata, generation: u64) -> Value {
    json!({
        "artifactId": artifact.id,
        "runId": artifact.run_id,
        "generation": generation,
        "nodeId": artifact.node_id,
        "kind": artifact.metadata.get("kind").and_then(Value::as_str).unwrap_or("evidence"),
        "title": artifact.name,
        "mimeType": artifact.mime_type,
        "byteLength": artifact.size,
        "hash": artifact.sha256,
        "sensitive": artifact.metadata.get("sensitive").and_then(Value::as_bool).unwrap_or(false),
        "createdAt": artifact.created_at_ms,
    })
}

fn approval_wire(
    approval: &crate::workflow::domain::ApprovalRecord,
    generation: u64,
    revision: u64,
) -> Value {
    json!({
        "approvalId": approval.id,
        "interactionId": approval.id,
        "runId": approval.run_id,
        "generation": generation,
        "nodeId": approval.node_id,
        "requestRevision": revision,
        "state": approval.status,
        "title": "工作流审批",
        "prompt": approval.prompt,
        "createdAt": approval.requested_at_ms,
        "respondedAt": approval.resolved_at_ms,
    })
}

fn acceptance_gate(run: &RunRecord, nodes: &[crate::workflow::domain::NodeRecord]) -> Value {
    let reviewers = nodes
        .iter()
        .filter(|node| node.role == NodeRole::Reviewer)
        .collect::<Vec<_>>();
    let explicit_verdicts = reviewers
        .iter()
        .filter(|reviewer| reviewer.status == NodeStatus::Succeeded)
        .filter_map(|reviewer| reviewer.result.as_ref())
        .map(parse_reviewer_verdict)
        .collect::<Vec<_>>();
    let direct = reviewers.is_empty() && run.route == RouteMode::Direct;
    let state = if direct {
        if run.status == crate::workflow::domain::RunStatus::Succeeded {
            "passed"
        } else {
            "pending"
        }
    } else if reviewers.is_empty() {
        "inconclusive"
    } else if reviewers.iter().any(|reviewer| {
        matches!(
            reviewer.status,
            NodeStatus::Failed | NodeStatus::UnknownOutcome
        )
    }) || explicit_verdicts
        .iter()
        .any(|verdict| matches!(verdict, ReviewerVerdict::ChangesRequired { .. }))
    {
        "failed"
    } else if explicit_verdicts
        .iter()
        .any(|verdict| matches!(verdict, ReviewerVerdict::Inconclusive { .. }))
    {
        "inconclusive"
    } else if explicit_verdicts.len() == reviewers.len()
        && explicit_verdicts
            .iter()
            .all(|verdict| matches!(verdict, ReviewerVerdict::Pass { .. }))
    {
        "passed"
    } else {
        "pending"
    };
    json!({
        "state": state,
        "reviewerCount": reviewers.len(),
        "summary": match state {
            "passed" if direct => "Direct 路径已完成，无需独立 Reviewer",
            "passed" => "所有必需的独立 Reviewer 已明确通过",
            "failed" => "Reviewer 未通过",
            "pending" => "等待独立 Reviewer",
            _ => "Reviewer 证据不足或结论不明确",
        },
    })
}

async fn bind_origin_thread(state: &Arc<AppState>, thread_id: &str) -> Result<bool, String> {
    let runtime = state
        .runtime
        .lock()
        .await
        .clone()
        .ok_or_else(|| "Codex 尚未运行".to_string())?;
    let websocket_url = runtime.renderer_websocket_url().await;
    let thread_id = serde_json::to_string(thread_id).map_err(|error| error.to_string())?;
    let script = format!(
        r#"(async () => {{
          if (typeof window.__codeyLoadSessionTools === "function") {{
            await window.__codeyLoadSessionTools();
          }}
          if (typeof window.__codeyBindWorkflowThread !== "function") return false;
          return await window.__codeyBindWorkflowThread({thread_id});
        }})()"#
    );
    let response = codey_runtime_core::bridge::evaluate_script_with_await_promise(
        &websocket_url,
        &script,
        true,
    )
    .await
    .map_err(|error| error.to_string())?;
    Ok(response
        .get("result")
        .and_then(|value| value.get("result"))
        .and_then(|value| value.get("value"))
        .and_then(Value::as_bool)
        == Some(true))
}

fn validate_protocol(schema: u64, protocol: u64) -> Result<(), String> {
    if schema == PROTOCOL_VERSION && protocol == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err("工作流协议版本不兼容".to_string())
    }
}

fn parse<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| format!("工作流参数无效：{error}"))
}

fn required_string(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("缺少参数：{key}"))
}

fn event_severity(kind: &str) -> &'static str {
    if kind.contains("failed") || kind.contains("unknown") {
        "error"
    } else if kind.contains("attention") || kind.contains("approval") {
        "warning"
    } else {
        "info"
    }
}

fn event_summary(event: &crate::workflow::domain::WorkflowEvent) -> String {
    let node = event
        .payload
        .get("nodeId")
        .and_then(Value::as_str)
        .map(|node| format!(" · {node}"))
        .unwrap_or_default();
    format!("{}{}", event.kind, node)
}

fn is_text_mime(mime: &str) -> bool {
    mime.starts_with("text/")
        || mime.contains("json")
        || mime.contains("xml")
        || mime.contains("yaml")
        || mime.contains("javascript")
}

#[derive(Debug, PartialEq, Eq)]
struct TextArtifactPage {
    offset: usize,
    total_bytes: usize,
    text: String,
    next_offset: Option<usize>,
}

fn redacted_text_page(bytes: &[u8], requested_offset: usize, limit: usize) -> TextArtifactPage {
    let decoded = String::from_utf8_lossy(bytes);
    let redacted = redact_text(&decoded);
    let total_bytes = redacted.len();
    let mut start = requested_offset.min(total_bytes);
    while start < total_bytes && !redacted.is_char_boundary(start) {
        start += 1;
    }

    let target_end = start.saturating_add(limit).min(total_bytes);
    let mut end = target_end;
    while end > start && !redacted.is_char_boundary(end) {
        end -= 1;
    }
    if end == start && start < total_bytes {
        end = start
            + redacted[start..]
                .chars()
                .next()
                .expect("non-empty text page has a character")
                .len_utf8();
    }

    TextArtifactPage {
        offset: start,
        total_bytes,
        text: redacted[start..end].to_string(),
        next_offset: (end < total_bytes).then_some(end),
    }
}

fn redact_text(value: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;

    static BEARER: OnceLock<Regex> = OnceLock::new();
    static PREFIXED_TOKEN: OnceLock<Regex> = OnceLock::new();
    static ASSIGNMENT: OnceLock<Regex> = OnceLock::new();

    let bearer = BEARER.get_or_init(|| {
        Regex::new(r"(?i)\bBearer[ \t]+[A-Za-z0-9._~+/-]{8,}")
            .expect("static bearer redaction regex")
    });
    let prefixed_token = PREFIXED_TOKEN.get_or_init(|| {
        Regex::new(r"(?i)\b(?:sk|xox[baprs])-[A-Za-z0-9_-]{8,}")
            .expect("static token redaction regex")
    });
    let assignment = ASSIGNMENT.get_or_init(|| {
        Regex::new(
            r"(?i)(?P<prefix>(?:api[_-]?key|access[_-]?token|refresh[_-]?token|secret|password|authorization)\s*[=:]\s*)[^\s,;]+",
        )
        .expect("static assignment redaction regex")
    });

    let redacted = bearer.replace_all(value, "Bearer [已隐藏]");
    let redacted = prefixed_token.replace_all(&redacted, "[已隐藏凭据]");
    assignment
        .replace_all(&redacted, "${prefix}[已隐藏]")
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::{redact_text, redacted_text_page, run_is_resumable};
    use crate::workflow::domain::RunStatus;
    use crate::workflow::journal::RunSummaryStats;

    #[test]
    fn workflow_artifact_redaction_preserves_layout_and_hides_common_secrets() {
        let source = "line one\nAuthorization: Bearer abcdefghijklmnop\napi_key=secret-value\nsk-1234567890abcdef";
        let redacted = redact_text(source);

        assert_eq!(redacted.lines().count(), source.lines().count());
        assert!(!redacted.contains("abcdefghijklmnop"));
        assert!(!redacted.contains("secret-value"));
        assert!(!redacted.contains("sk-1234567890abcdef"));
        assert!(redacted.contains("api_key=[已隐藏]"));
    }

    #[test]
    fn artifact_redaction_happens_before_pagination() {
        let source = "开头 Authorization: Bearer abcdefghijklmnop 结尾";
        let expected = redact_text(source);
        let mut collected = String::new();
        let mut offset = 0;

        loop {
            let page = redacted_text_page(source.as_bytes(), offset, 7);
            assert!(!page.text.contains("abcdefghijklmnop"));
            collected.push_str(&page.text);
            match page.next_offset {
                Some(next) => {
                    assert!(next > offset);
                    offset = next;
                }
                None => break,
            }
        }

        assert_eq!(collected, expected);
        assert!(!collected.contains("abcdefghijklmnop"));
    }

    #[test]
    fn artifact_text_pages_never_split_utf8_characters() {
        let source = "甲乙🙂api_key=secret-value丙丁";
        let expected = redact_text(source);
        let mut collected = String::new();
        let mut offset = 0;

        loop {
            let page = redacted_text_page(source.as_bytes(), offset, 5);
            assert!(!page.text.contains('\u{fffd}'));
            collected.push_str(&page.text);
            match page.next_offset {
                Some(next) => offset = next,
                None => break,
            }
        }

        assert_eq!(collected, expected);
        assert!(!collected.contains("secret-value"));
    }

    #[test]
    fn needs_attention_is_only_resumable_with_unblocked_non_final_work() {
        let runnable = RunSummaryStats {
            ready_non_final_nodes: 1,
            ..RunSummaryStats::default()
        };
        assert!(run_is_resumable(RunStatus::NeedsAttention, &runnable));
        assert!(run_is_resumable(
            RunStatus::Paused,
            &RunSummaryStats::default()
        ));

        let blocked = RunSummaryStats {
            ready_non_final_nodes: 1,
            attention_blockers: 1,
            ..RunSummaryStats::default()
        };
        assert!(!run_is_resumable(RunStatus::NeedsAttention, &blocked));
        assert!(!run_is_resumable(
            RunStatus::NeedsAttention,
            &RunSummaryStats::default()
        ));
    }
}
