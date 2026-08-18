#[cfg(test)]
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::app_server::{
    AppServerAdapter, FinalDeliveryRequest, StartNodeRequest, StartedNode, SteerThreadRequest,
};
use super::artifacts::ArtifactStore;
use super::domain::{
    ApprovalStatus, ArtifactMetadata, ArtifactPage, CommandEnvelope, DurableAck, Lease, NodeCas,
    NodeRecord, NodeRole, NodeStatus, PermissionSet, RouteMode, RunRecord, RunStatus,
    WorkflowError, WorkflowEvent, WorkflowResult, WorkspaceRisk,
};
use super::journal::{CommandOutcome, EventDraft, Journal, now_ms, payload_hash};
use super::policy::{WorkflowPolicy, WorkspaceDecision};
use super::scheduler::PlanCompiler;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartWorkflowRequest {
    pub command_id: String,
    #[serde(default)]
    pub run_id: Option<String>,
    pub route: RouteMode,
    pub input: Value,
    #[serde(default)]
    pub permissions: PermissionSet,
    #[serde(default)]
    pub workspace_risk: WorkspaceRisk,
    #[serde(default)]
    pub isolated_workspace_available: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SteerWorkflowRequest {
    pub command_id: String,
    pub run_id: String,
    #[serde(default)]
    pub node_id: Option<String>,
    pub message: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCommandRequest {
    pub command_id: String,
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryNodeRequest {
    pub command_id: String,
    pub run_id: String,
    pub node_id: String,
    #[serde(default)]
    pub replacement_payload: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalReplyRequest {
    pub command_id: String,
    pub run_id: String,
    pub approval_id: String,
    pub approved: bool,
    pub response: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListRunsRequest {
    #[serde(default)]
    pub status: Option<RunStatus>,
    #[serde(default)]
    pub after_updated_at_ms: Option<i64>,
    #[serde(default = "default_page_limit")]
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunDetails {
    pub run: RunRecord,
    pub nodes: Vec<NodeRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactResponse {
    pub metadata: ArtifactMetadata,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowCapabilities {
    pub available: bool,
    pub unavailable_reason: Option<String>,
    pub schema_version: Option<i64>,
    pub routes: Vec<RouteMode>,
    pub durable_commands: bool,
    pub event_replay: bool,
    pub artifact_paging: bool,
    pub approvals: bool,
    pub recovery: bool,
    pub exactly_once_delivery: bool,
}

#[derive(Clone)]
struct AvailableService {
    journal: Journal,
    artifacts: ArtifactStore,
    adapter: Arc<dyn AppServerAdapter>,
    policy: Arc<RwLock<WorkflowPolicy>>,
}

#[derive(Clone)]
enum ServiceState {
    Available(AvailableService),
    Unavailable(String),
}

/// Async API surface used by UI/command integrations.
///
/// Database calls are always completed on Tokio's blocking pool before an ACK
/// is returned. Adapter calls happen after database handles have been dropped.
#[derive(Clone)]
pub struct WorkflowService {
    state: Arc<ServiceState>,
}

impl WorkflowService {
    #[cfg(test)]
    pub fn open(
        journal_path: impl AsRef<Path>,
        artifact_root: impl AsRef<Path>,
        adapter: Arc<dyn AppServerAdapter>,
        policy: WorkflowPolicy,
    ) -> WorkflowResult<Self> {
        let journal = Journal::open(journal_path)?;
        let artifacts = ArtifactStore::open(artifact_root)?;
        Ok(Self::new(journal, artifacts, adapter, policy))
    }

    pub fn new(
        journal: Journal,
        artifacts: ArtifactStore,
        adapter: Arc<dyn AppServerAdapter>,
        policy: WorkflowPolicy,
    ) -> Self {
        Self {
            state: Arc::new(ServiceState::Available(AvailableService {
                journal,
                artifacts,
                adapter,
                policy: Arc::new(RwLock::new(policy)),
            })),
        }
    }

    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            state: Arc::new(ServiceState::Unavailable(reason.into())),
        }
    }

    fn available(&self) -> WorkflowResult<AvailableService> {
        match self.state.as_ref() {
            ServiceState::Available(available) => Ok(available.clone()),
            ServiceState::Unavailable(reason) => Err(WorkflowError::Unavailable(reason.clone())),
        }
    }

    pub fn update_policy(&self, policy: WorkflowPolicy) -> WorkflowResult<()> {
        policy.validate()?;
        let available = self.available()?;
        *available
            .policy
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = policy;
        Ok(())
    }

    pub async fn start(&self, request: StartWorkflowRequest) -> WorkflowResult<DurableAck> {
        validate_identifier("command id", &request.command_id)?;
        let available = self.available()?;
        let policy = available
            .policy
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let snapshot = policy.snapshot(&request.permissions, request.workspace_risk.clone())?;
        let plan = PlanCompiler::new(snapshot.clone()).compile(request.route)?;
        for node in &plan.nodes {
            match policy.workspace_decision(
                &snapshot.workspace_risk,
                node.repo_writer,
                request.isolated_workspace_available,
            ) {
                WorkspaceDecision::InPlace | WorkspaceDecision::Isolate => {}
                WorkspaceDecision::RequireApproval => {
                    return Err(WorkflowError::InvalidRequest(
                        "a dirty workspace requires isolation or explicit approval before writing"
                            .to_string(),
                    ));
                }
                WorkspaceDecision::Reject => {
                    return Err(WorkflowError::InvalidRequest(
                        "a high-risk workspace cannot be modified without isolation".to_string(),
                    ));
                }
            }
        }
        let request_hash = payload_hash(&request)?;
        let run_id = request
            .run_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        validate_identifier("run id", &run_id)?;
        let created_at = now_ms();
        let run = RunRecord {
            id: run_id.clone(),
            status: RunStatus::Queued,
            route: request.route,
            revision: 0,
            generation: 0,
            input: request.input.clone(),
            policy: snapshot,
            final_delivery_committed: false,
            created_at_ms: created_at,
            updated_at_ms: created_at,
            error: None,
        };
        let command = CommandEnvelope {
            command_id: request.command_id,
            run_id: run_id.clone(),
            kind: "start".to_string(),
            payload_hash: request_hash,
        };
        let event = EventDraft::new(
            "run.queued",
            serde_json::json!({ "route": request.route, "input": request.input }),
        );
        let journal = available.journal;
        let outcome =
            run_blocking(move || journal.create_run(&run, &plan, &command, &event)).await?;
        Ok(ack_from_outcome(outcome))
    }

    pub async fn steer(&self, request: SteerWorkflowRequest) -> WorkflowResult<DurableAck> {
        validate_identifier("command id", &request.command_id)?;
        let available = self.available()?;
        let run_id = request.run_id.clone();
        let journal = available.journal.clone();
        let run = run_blocking(move || journal.get_run(&run_id)).await?;
        if run.status.is_terminal() || matches!(run.status, RunStatus::Canceling) {
            return Err(WorkflowError::Conflict(format!(
                "run {} does not accept steering while {}",
                run.id, run.status
            )));
        }
        let command = CommandEnvelope {
            command_id: request.command_id.clone(),
            run_id: request.run_id.clone(),
            kind: "steer".to_string(),
            payload_hash: payload_hash(&request)?,
        };
        let event = EventDraft::new(
            "run.steered",
            serde_json::json!({
                "commandId": request.command_id,
                "nodeId": request.node_id,
                "message": request.message,
            }),
        );
        let journal = available.journal.clone();
        let outcome = run_blocking(move || journal.append_command_event(&command, &event)).await?;
        let ack = ack_from_outcome(outcome);

        let run_id = request.run_id.clone();
        let journal = available.journal.clone();
        let nodes = run_blocking(move || journal.nodes(&run_id)).await?;
        if let Some((node_id, thread_id)) = select_thread(&nodes, request.node_id.as_deref())? {
            available
                .adapter
                .steer_thread(SteerThreadRequest {
                    run_id: request.run_id.clone(),
                    thread_id,
                    message: request.message.clone(),
                    idempotency_key: request.command_id.clone(),
                })
                .await?;
            let journal = available.journal;
            let run_id = request.run_id;
            run_blocking(move || {
                journal.append_event(
                    &run_id,
                    &EventDraft::new(
                        "run.steer_forwarded",
                        serde_json::json!({
                            "commandId": request.command_id,
                            "nodeId": node_id,
                        }),
                    ),
                )
            })
            .await?;
        }
        Ok(ack)
    }

    pub async fn pause(&self, request: RunCommandRequest) -> WorkflowResult<DurableAck> {
        let available = self.available()?;
        let ack = self
            .transition_command(
                &available,
                &request,
                "pause",
                RunStatus::Pausing,
                "run.pausing",
            )
            .await?;
        if !ack.duplicate {
            self.interrupt_active(&available, &request.run_id, &request.command_id)
                .await?;
            let run_id = request.run_id.clone();
            let journal = available.journal.clone();
            let run = run_blocking(move || journal.get_run(&run_id)).await?;
            if run.status == RunStatus::Pausing {
                let journal = available.journal.clone();
                let run_id = run.id.clone();
                run_blocking(move || {
                    journal.transition_run(
                        &run_id,
                        run.revision,
                        run.generation,
                        RunStatus::Paused,
                        &EventDraft::new("run.paused", serde_json::json!({})),
                    )
                })
                .await?;
            }
        }
        Ok(ack)
    }

    pub async fn resume(&self, request: RunCommandRequest) -> WorkflowResult<DurableAck> {
        let available = self.available()?;
        self.transition_command(
            &available,
            &request,
            "resume",
            RunStatus::Queued,
            "run.resumed",
        )
        .await
    }

    pub async fn cancel(&self, request: RunCommandRequest) -> WorkflowResult<DurableAck> {
        let available = self.available()?;
        let ack = self
            .transition_command(
                &available,
                &request,
                "cancel",
                RunStatus::Canceling,
                "run.canceling",
            )
            .await?;
        if !ack.duplicate {
            self.interrupt_active(&available, &request.run_id, &request.command_id)
                .await?;
            let run_id = request.run_id.clone();
            let journal = available.journal.clone();
            let run = run_blocking(move || journal.get_run(&run_id)).await?;
            if run.status == RunStatus::Canceling {
                let journal = available.journal.clone();
                let run_id = run.id.clone();
                run_blocking(move || {
                    journal.transition_run(
                        &run_id,
                        run.revision,
                        run.generation,
                        RunStatus::Canceled,
                        &EventDraft::new("run.canceled", serde_json::json!({})),
                    )
                })
                .await?;
            }
        }
        Ok(ack)
    }

    pub async fn retry(&self, request: RetryNodeRequest) -> WorkflowResult<DurableAck> {
        validate_identifier("command id", &request.command_id)?;
        let available = self.available()?;
        let run_id = request.run_id.clone();
        let node_id = request.node_id.clone();
        let journal = available.journal.clone();
        let node = run_blocking(move || journal.get_node(&run_id, &node_id)).await?;
        if node.repo_writer && node.status == super::domain::NodeStatus::UnknownOutcome {
            return Err(WorkflowError::Conflict(
                "a writer with unknown outcome requires reconciliation or compensation".to_string(),
            ));
        }
        let replacement = request.replacement_payload.as_ref().unwrap_or(&Value::Null);
        let new_payload_hash = payload_hash(&serde_json::json!({
            "previous": node.payload_hash,
            "replacement": replacement,
            "nextGeneration": node.generation + 1,
        }))?;
        let request_hash = payload_hash(&request)?;
        let command = CommandEnvelope {
            command_id: request.command_id,
            run_id: request.run_id.clone(),
            kind: "retry".to_string(),
            payload_hash: request_hash,
        };
        let cas = super::domain::NodeCas {
            expected_revision: node.revision,
            expected_generation: node.generation,
            expected_lease_epoch: node.lease_epoch,
            expected_payload_hash: node.payload_hash.clone(),
        };
        let event = EventDraft::new(
            "node.retry_scheduled",
            serde_json::json!({ "nodeId": node.id, "generation": node.generation + 1 }),
        );
        let journal = available.journal;
        let run_id = request.run_id;
        let node_id = request.node_id;
        let outcome = run_blocking(move || {
            journal.reset_node_for_retry_command(
                &command,
                &run_id,
                &node_id,
                &cas,
                &new_payload_hash,
                &event,
            )
        })
        .await?;
        Ok(ack_from_outcome(outcome))
    }

    pub async fn reply(&self, request: ApprovalReplyRequest) -> WorkflowResult<DurableAck> {
        validate_identifier("command id", &request.command_id)?;
        let available = self.available()?;
        let decision = if request.approved {
            ApprovalStatus::Approved
        } else {
            ApprovalStatus::Rejected
        };
        let request_hash = payload_hash(&request)?;
        let command = CommandEnvelope {
            command_id: request.command_id,
            run_id: request.run_id.clone(),
            kind: "approval_reply".to_string(),
            payload_hash: request_hash,
        };
        let event = EventDraft::new(
            "approval.resolved",
            serde_json::json!({
                "approvalId": request.approval_id,
                "decision": decision,
                "response": request.response,
            }),
        );
        let journal = available.journal;
        let approval_id = request.approval_id;
        let response = request.response;
        let outcome = run_blocking(move || {
            journal.resolve_approval_command(&command, &approval_id, decision, &response, &event)
        })
        .await?;
        Ok(ack_from_outcome(outcome))
    }

    pub async fn list(&self, request: ListRunsRequest) -> WorkflowResult<Vec<RunRecord>> {
        let available = self.available()?;
        let journal = available.journal;
        run_blocking(move || {
            journal.list_runs(request.status, request.after_updated_at_ms, request.limit)
        })
        .await
    }

    pub async fn get(&self, run_id: impl Into<String>) -> WorkflowResult<RunDetails> {
        let available = self.available()?;
        let run_id = run_id.into();
        let journal = available.journal;
        run_blocking(move || {
            let run = journal.get_run(&run_id)?;
            let nodes = journal.nodes(&run_id)?;
            Ok(RunDetails { run, nodes })
        })
        .await
    }

    pub async fn events(
        &self,
        run_id: impl Into<String>,
        after_sequence: u64,
        limit: usize,
    ) -> WorkflowResult<Vec<WorkflowEvent>> {
        let available = self.available()?;
        let run_id = run_id.into();
        let journal = available.journal;
        run_blocking(move || journal.events_after(&run_id, after_sequence, limit)).await
    }

    pub async fn artifact(
        &self,
        run_id: impl Into<String>,
        artifact_id: impl Into<String>,
    ) -> WorkflowResult<ArtifactResponse> {
        let available = self.available()?;
        let run_id = run_id.into();
        let artifact_id = artifact_id.into();
        let journal = available.journal;
        let artifacts = available.artifacts;
        run_blocking(move || {
            let metadata = journal.get_artifact(&run_id, &artifact_id)?;
            let bytes = artifacts.read(&metadata)?;
            Ok(ArtifactResponse { metadata, bytes })
        })
        .await
    }

    pub async fn store_node_result(
        &self,
        run_id: impl Into<String>,
        node_id: impl Into<String>,
        role: NodeRole,
        result: Value,
    ) -> WorkflowResult<ArtifactMetadata> {
        let available = self.available()?;
        let run_id = run_id.into();
        let node_id = node_id.into();
        let bytes = serde_json::to_vec_pretty(&result)?;
        let journal = available.journal;
        let artifacts = available.artifacts;
        run_blocking(move || {
            artifacts.put(
                &journal,
                &run_id,
                Some(&node_id),
                format!("{} result", role.as_str()),
                "application/json",
                &bytes,
                serde_json::json!({
                    "kind": "node_result",
                    "role": role,
                    "sensitive": true,
                }),
            )
        })
        .await
    }

    pub async fn artifacts(
        &self,
        run_id: impl Into<String>,
        after_id: Option<String>,
        limit: usize,
    ) -> WorkflowResult<ArtifactPage> {
        let available = self.available()?;
        let run_id = run_id.into();
        let journal = available.journal;
        run_blocking(move || journal.artifacts_page(&run_id, after_id.as_deref(), limit)).await
    }

    pub async fn capabilities(&self) -> WorkflowCapabilities {
        match self.state.as_ref() {
            ServiceState::Unavailable(reason) => WorkflowCapabilities {
                available: false,
                unavailable_reason: Some(reason.clone()),
                schema_version: None,
                routes: vec![],
                durable_commands: false,
                event_replay: false,
                artifact_paging: false,
                approvals: false,
                recovery: false,
                exactly_once_delivery: false,
            },
            ServiceState::Available(available) => {
                let journal = available.journal.clone();
                let schema_version = run_blocking(move || journal.schema_version()).await.ok();
                WorkflowCapabilities {
                    available: schema_version.is_some(),
                    unavailable_reason: schema_version
                        .is_none()
                        .then(|| "workflow journal is temporarily unavailable".to_string()),
                    schema_version,
                    routes: vec![
                        RouteMode::Direct,
                        RouteMode::Guarded,
                        RouteMode::Parallel,
                        RouteMode::Expert,
                    ],
                    durable_commands: true,
                    event_replay: true,
                    artifact_paging: true,
                    // Server-initiated approval requests are fail-closed until
                    // the proxy can durably park and later resume them.
                    approvals: false,
                    recovery: true,
                    exactly_once_delivery: false,
                }
            }
        }
    }

    pub async fn dispatch_node(
        &self,
        run_id: impl Into<String>,
        node_id: impl Into<String>,
        worker_id: impl Into<String>,
        lease_ttl: Duration,
    ) -> WorkflowResult<(StartedNode, Lease)> {
        let available = self.available()?;
        let run_id = run_id.into();
        let node_id = node_id.into();
        let worker_id = worker_id.into();
        let journal = available.journal.clone();
        let lookup_run = run_id.clone();
        let lookup_node = node_id.clone();
        let node = run_blocking(move || journal.get_node(&lookup_run, &lookup_node)).await?;
        let journal = available.journal.clone();
        let prompt_run_id = run_id.clone();
        let run = run_blocking(move || journal.get_run(&prompt_run_id)).await?;
        let journal = available.journal.clone();
        let dependency_run_id = run_id.clone();
        let all_nodes = run_blocking(move || journal.nodes(&dependency_run_id)).await?;
        let journal = available.journal.clone();
        let steering_run_id = run_id.clone();
        let run_messages =
            run_blocking(move || journal.events_after(&steering_run_id, 0, 500)).await?;
        let steering_messages = run_messages
            .iter()
            .filter(|event| event.kind == "run.steered")
            .filter_map(|event| event.payload.get("message").cloned())
            .collect::<Vec<_>>();
        let repair_messages = run_messages
            .iter()
            .filter(|event| event.kind == "run.repair_requested")
            .map(|event| event.payload.clone())
            .collect::<Vec<_>>();
        let dependency_results = node
            .dependencies
            .iter()
            .filter_map(|dependency| {
                all_nodes
                    .iter()
                    .find(|candidate| &candidate.id == dependency)
                    .and_then(|candidate| {
                        candidate.result.clone().map(|result| (dependency, result))
                    })
            })
            .map(|(dependency, result)| {
                serde_json::json!({
                    "nodeId": dependency,
                    "result": result,
                })
            })
            .collect::<Vec<_>>();
        let cwd = run
            .input
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let permission_snapshot = run
            .input
            .get("permissionSnapshot")
            .cloned()
            .unwrap_or(Value::Null);
        let approval_policy = run
            .input
            .get("permissionRuntime")
            .and_then(|runtime| runtime.get("approvalPolicy"))
            .cloned()
            .or_else(|| permission_snapshot.get("approvalPolicy").cloned())
            .unwrap_or_else(|| Value::String("never".to_string()));
        let sandbox_mode = permission_snapshot
            .get("sandboxMode")
            .and_then(Value::as_str)
            .unwrap_or(if node.repo_writer {
                "workspace-write"
            } else {
                "read-only"
            })
            .to_string();
        let role_key = node.role.as_str();
        let role_selection = run
            .input
            .get("roleModels")
            .and_then(|roles| roles.get(role_key));
        let model = role_selection
            .and_then(|selection| selection.get("model"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let reasoning_effort = role_selection
            .and_then(|selection| selection.get("reasoningEffort"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let prompt = serde_json::json!({
            "workflow": {
                "runId": run_id.clone(),
                "nodeId": node_id.clone(),
                "route": run.route,
                "role": node.role,
            },
            "objective": run.input.get("originalRequest").cloned().unwrap_or_else(|| run.input.clone()),
            "instructions": role_instructions(node.role),
            "dependencyArtifacts": dependency_results,
            "steeringMessages": steering_messages,
            "repairMessages": repair_messages,
            "constraints": {
                "doNotBroadenPermissions": true,
                "doNotRevertUnrelatedUserChanges": true,
                "finalDeliveryIsOwnedByTheEngine": true,
            },
        });
        let existing_thread_id = (run.route == RouteMode::Direct && node.role == NodeRole::Builder)
            .then(|| {
                run.input
                    .get("originThreadId")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .flatten();
        let journal = available.journal.clone();
        let lease_run = run_id.clone();
        let lease_node = node_id.clone();
        let lease = run_blocking(move || {
            journal.acquire_lease(
                &lease_run,
                &lease_node,
                &worker_id,
                node.generation,
                node.lease_epoch,
                lease_ttl,
            )
        })
        .await?;
        let journal = available.journal.clone();
        let attempt_lease = lease.clone();
        let attempt_id =
            run_blocking(move || journal.start_attempt(&attempt_lease, node.repo_writer)).await?;
        let started = available
            .adapter
            .start_node(StartNodeRequest {
                run_id: run_id.clone(),
                node_id: node_id.clone(),
                role: node.role,
                prompt,
                existing_thread_id,
                cwd,
                approval_policy,
                sandbox_mode,
                model,
                reasoning_effort,
                permissions: node.permissions,
                idempotency_key: attempt_id,
                repo_writer: node.repo_writer,
            })
            .await?;
        let journal = available.journal.clone();
        let linked_run = lease.run_id.clone();
        let linked_node = lease.node_id.clone();
        let thread_id = started.thread_id.clone();
        if let Err(link_error) =
            run_blocking(move || journal.record_thread_link(&linked_run, &linked_node, &thread_id))
                .await
        {
            let cleanup = available
                .adapter
                .interrupt_thread(
                    &lease.run_id,
                    &started.thread_id,
                    &format!("dispatch-link-failed:{}:{}", lease.run_id, lease.node_id),
                )
                .await;
            return Err(match cleanup {
                Ok(()) => link_error,
                Err(cleanup_error) => WorkflowError::Adapter(format!(
                    "{link_error}; failed to interrupt unlinked worker: {cleanup_error}"
                )),
            });
        }
        Ok((started, lease))
    }

    pub async fn finalize(
        &self,
        run_id: impl Into<String>,
        content: Value,
    ) -> WorkflowResult<WorkflowEvent> {
        let available = self.available()?;
        let run_id = run_id.into();
        let journal = available.journal.clone();
        let lookup_id = run_id.clone();
        let run = run_blocking(move || journal.get_run(&lookup_id)).await?;
        if run.final_delivery_committed || run.status == RunStatus::Succeeded {
            return Err(WorkflowError::Conflict(
                "run is already finalized".to_string(),
            ));
        }
        if run.status != RunStatus::Running {
            return Err(WorkflowError::Conflict(format!(
                "run {} cannot finalize while {}",
                run.id, run.status
            )));
        }
        let origin_thread_id = run
            .input
            .get("originThreadId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                WorkflowError::InvalidRequest(
                    "final delivery requires a bound origin thread".to_string(),
                )
            })?
            .to_string();
        let journal = available.journal.clone();
        let node_run_id = run_id.clone();
        let nodes = run_blocking(move || journal.nodes(&node_run_id)).await?;
        let final_node = nodes
            .into_iter()
            .find(|node| node.role == NodeRole::FinalDelivery)
            .ok_or_else(|| WorkflowError::NotFound {
                entity: "final delivery node",
                id: run_id.clone(),
            })?;
        if final_node.status != NodeStatus::Ready {
            return Err(WorkflowError::Conflict(
                "final delivery is not ready because acceptance has not passed".to_string(),
            ));
        }
        available
            .adapter
            .deliver_final(FinalDeliveryRequest {
                run_id: run_id.clone(),
                origin_thread_id,
                content: content.clone(),
                idempotency_key: format!("final:{}:{}", run.id, run.generation),
            })
            .await?;
        let final_node_id = final_node.id.clone();
        let final_cas = NodeCas {
            expected_revision: final_node.revision,
            expected_generation: final_node.generation,
            expected_lease_epoch: final_node.lease_epoch,
            expected_payload_hash: final_node.payload_hash.clone(),
        };
        let journal = available.journal;
        run_blocking(move || {
            journal.commit_succeeded_after_delivery(
                &run_id,
                &final_node_id,
                run.revision,
                run.generation,
                &final_cas,
                &content,
                &EventDraft::new(
                    "run.succeeded",
                    serde_json::json!({ "finalDeliveryCommitted": true }),
                ),
            )
        })
        .await
    }

    async fn transition_command(
        &self,
        available: &AvailableService,
        request: &RunCommandRequest,
        kind: &str,
        next: RunStatus,
        event_kind: &str,
    ) -> WorkflowResult<DurableAck> {
        validate_identifier("command id", &request.command_id)?;
        let run_id = request.run_id.clone();
        let journal = available.journal.clone();
        let run = run_blocking(move || journal.get_run(&run_id)).await?;
        let command = CommandEnvelope {
            command_id: request.command_id.clone(),
            run_id: request.run_id.clone(),
            kind: kind.to_string(),
            payload_hash: payload_hash(request)?,
        };
        let event = EventDraft::new(event_kind, serde_json::json!({}));
        let journal = available.journal.clone();
        let outcome = run_blocking(move || {
            journal.transition_run_command(&command, run.revision, run.generation, next, &event)
        })
        .await?;
        Ok(ack_from_outcome(outcome))
    }

    async fn interrupt_active(
        &self,
        available: &AvailableService,
        run_id: &str,
        command_id: &str,
    ) -> WorkflowResult<()> {
        let lookup_id = run_id.to_string();
        let journal = available.journal.clone();
        let nodes = run_blocking(move || journal.nodes(&lookup_id)).await?;
        for node in nodes {
            if let Some(thread_id) = node.thread_id {
                available
                    .adapter
                    .interrupt_thread(run_id, &thread_id, &format!("{command_id}:{}", node.id))
                    .await?;
            }
        }
        Ok(())
    }
}

fn role_instructions(role: NodeRole) -> &'static str {
    match role {
        NodeRole::Planner | NodeRole::Preflight => {
            "Perform a concise preflight: restate the objective, inspect relevant evidence, identify risks and produce an executable plan. Do not modify files."
        }
        NodeRole::Researcher | NodeRole::Scout => {
            "Investigate the assigned slice independently. Return only verified findings, exact file or symbol locations, uncertainty and risks. Do not modify files."
        }
        NodeRole::Builder => {
            "Implement the requested change within the frozen permission and workspace boundary. Preserve unrelated user changes, validate proportionally, and report the exact changes and evidence."
        }
        NodeRole::Validator => {
            "Validate the ChangeSet independently with focused and then broad checks. Do not modify implementation files. Report commands, outcomes, regressions and any inconclusive evidence."
        }
        NodeRole::Reviewer => {
            "Review the OriginalRequest, Preflight risks, ChangeSet and ValidationReport independently. Fail closed on missing evidence. The first non-empty line must be exactly PASS, CHANGES_REQUIRED, or INCONCLUSIVE. After PASS, provide a concise user-ready final delivery. After any other verdict, list specific evidence and actionable defects."
        }
        NodeRole::Expert | NodeRole::Integrator => {
            "Resolve high-risk or conflicting evidence. Make assumptions explicit, prefer verified primary evidence, and return a conservative recommendation suitable for independent review."
        }
        NodeRole::FinalDelivery => "Final delivery is virtual and must not start an agent turn.",
    }
}

fn select_thread(
    nodes: &[NodeRecord],
    requested_node: Option<&str>,
) -> WorkflowResult<Option<(String, String)>> {
    let is_active = |node: &NodeRecord| {
        matches!(
            node.status,
            NodeStatus::Leased | NodeStatus::Running | NodeStatus::WaitingApproval
        )
    };
    if let Some(node_id) = requested_node {
        let node = nodes
            .iter()
            .find(|node| node.id == node_id)
            .ok_or_else(|| WorkflowError::NotFound {
                entity: "node",
                id: node_id.to_string(),
            })?;
        return Ok(is_active(node)
            .then(|| {
                node.thread_id
                    .clone()
                    .map(|thread| (node.id.clone(), thread))
            })
            .flatten());
    }
    Ok(nodes
        .iter()
        .find(|node| node.role == NodeRole::Builder && is_active(node))
        .or_else(|| nodes.iter().find(|node| is_active(node)))
        .and_then(|node| {
            node.thread_id
                .clone()
                .map(|thread| (node.id.clone(), thread))
        }))
}

fn ack_from_outcome(outcome: CommandOutcome<DurableAck>) -> DurableAck {
    let duplicate = outcome.is_duplicate();
    let mut ack = outcome.into_inner();
    ack.duplicate = duplicate;
    ack
}

fn validate_identifier(name: &str, value: &str) -> WorkflowResult<()> {
    if value.trim().is_empty() || value.len() > 256 {
        Err(WorkflowError::InvalidRequest(format!(
            "{name} must contain between 1 and 256 characters"
        )))
    } else {
        Ok(())
    }
}

fn default_page_limit() -> usize {
    50
}

pub(crate) async fn run_blocking<T, F>(operation: F) -> WorkflowResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> WorkflowResult<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| WorkflowError::Join(error.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::app_server::{FakeAppServerAdapter, FakeCall, StartedNode};

    fn service() -> (
        tempfile::TempDir,
        WorkflowService,
        Arc<FakeAppServerAdapter>,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let adapter = Arc::new(FakeAppServerAdapter::default());
        let service = WorkflowService::open(
            temp.path().join("workflow.sqlite"),
            temp.path().join("artifacts"),
            adapter.clone(),
            WorkflowPolicy {
                permissions: PermissionSet {
                    read_paths: ["repo"].into_iter().map(String::from).collect(),
                    write_paths: ["repo"].into_iter().map(String::from).collect(),
                    allowed_commands: Default::default(),
                    network_hosts: Default::default(),
                    can_request_approval: true,
                },
                ..WorkflowPolicy::default()
            },
        )
        .unwrap();
        (temp, service, adapter)
    }

    fn start_request(command_id: &str) -> StartWorkflowRequest {
        StartWorkflowRequest {
            command_id: command_id.to_string(),
            run_id: Some("run-service".to_string()),
            route: RouteMode::Direct,
            input: serde_json::json!({
                "originalRequest": "test",
                "originThreadId": "origin-thread",
                "cwd": "repo",
                "permissionSnapshot": {
                    "approvalPolicy": "never",
                    "sandboxMode": "workspace-write"
                }
            }),
            permissions: PermissionSet {
                read_paths: ["repo"].into_iter().map(String::from).collect(),
                write_paths: ["repo"].into_iter().map(String::from).collect(),
                allowed_commands: Default::default(),
                network_hosts: Default::default(),
                can_request_approval: true,
            },
            workspace_risk: WorkspaceRisk::default(),
            isolated_workspace_available: false,
        }
    }

    #[tokio::test]
    async fn start_ack_is_durable_and_replayable() {
        let (_temp, service, _adapter) = service();
        let first = service.start(start_request("start-1")).await.unwrap();
        let duplicate = service.start(start_request("start-1")).await.unwrap();
        assert!(!first.duplicate);
        assert!(duplicate.duplicate);
        assert_eq!(first.workflow_sequence, duplicate.workflow_sequence);
    }

    #[tokio::test]
    async fn final_delivery_failure_cannot_succeed_the_run() {
        let (_temp, service, adapter) = service();
        service.start(start_request("start-1")).await.unwrap();
        let details = service.get("run-service").await.unwrap();
        let available = service.available().unwrap();
        let journal = available.journal.clone();
        run_blocking(move || {
            journal.transition_run(
                "run-service",
                details.run.revision,
                details.run.generation,
                RunStatus::Running,
                &EventDraft::new("run.running", serde_json::json!({})),
            )
        })
        .await
        .unwrap();
        service
            .dispatch_node("run-service", "builder", "worker", Duration::from_secs(30))
            .await
            .unwrap();
        let builder = service
            .get("run-service")
            .await
            .unwrap()
            .nodes
            .into_iter()
            .find(|node| node.id == "builder")
            .unwrap();
        let journal = available.journal.clone();
        run_blocking(move || {
            journal.finish_attempt_and_node(
                "run-service",
                "builder",
                &NodeCas {
                    expected_revision: builder.revision,
                    expected_generation: builder.generation,
                    expected_lease_epoch: builder.lease_epoch,
                    expected_payload_hash: builder.payload_hash,
                },
                NodeStatus::Succeeded,
                Some(&serde_json::json!({ "text": "done" })),
                None,
                &EventDraft::new("node.completed", serde_json::json!({})),
            )?;
            journal.promote_ready_nodes("run-service")?;
            Ok(())
        })
        .await
        .unwrap();
        adapter.fail_next("delivery failed");
        assert!(
            service
                .finalize("run-service", serde_json::json!({"answer":1}))
                .await
                .is_err()
        );
        let details = service.get("run-service").await.unwrap();
        assert_eq!(details.run.status, RunStatus::Running);
        assert!(!details.run.final_delivery_committed);
        assert_eq!(
            details
                .nodes
                .iter()
                .find(|node| node.role == NodeRole::FinalDelivery)
                .unwrap()
                .status,
            NodeStatus::Ready
        );
        assert!(
            !adapter
                .calls()
                .iter()
                .any(|call| matches!(call, FakeCall::Final(_)))
        );
    }

    #[tokio::test]
    async fn dispatch_interrupts_worker_when_thread_link_cannot_be_persisted() {
        let (_temp, service, adapter) = service();
        service.start(start_request("start-1")).await.unwrap();
        let details = service.get("run-service").await.unwrap();
        let available = service.available().unwrap();
        let journal = available.journal.clone();
        run_blocking(move || {
            journal.transition_run(
                "run-service",
                details.run.revision,
                details.run.generation,
                RunStatus::Running,
                &EventDraft::new("run.running", serde_json::json!({})),
            )
        })
        .await
        .unwrap();

        let journal = available.journal.clone();
        run_blocking(move || {
            journal.record_thread_link("run-service", "final_delivery", "thread-link-collision")
        })
        .await
        .unwrap();
        adapter.queue_started(StartedNode {
            thread_id: "thread-link-collision".to_string(),
            turn_id: Some("turn-builder".to_string()),
        });

        assert!(
            service
                .dispatch_node("run-service", "builder", "worker", Duration::from_secs(30))
                .await
                .is_err()
        );
        assert!(adapter.calls().iter().any(|call| matches!(
            call,
            FakeCall::Interrupt {
                run_id,
                thread_id,
                idempotency_key,
            } if run_id == "run-service"
                && thread_id == "thread-link-collision"
                && idempotency_key == "dispatch-link-failed:run-service:builder"
        )));
        assert!(
            service
                .get("run-service")
                .await
                .unwrap()
                .nodes
                .into_iter()
                .find(|node| node.id == "builder")
                .unwrap()
                .thread_id
                .is_none()
        );
    }

    #[tokio::test]
    async fn unavailable_service_reports_capabilities_and_rejects_calls() {
        let service = WorkflowService::unavailable("database locked");
        let capabilities = service.capabilities().await;
        assert!(!capabilities.available);
        assert_eq!(
            capabilities.unavailable_reason.as_deref(),
            Some("database locked")
        );
        assert!(matches!(
            service.start(start_request("start-1")).await,
            Err(WorkflowError::Unavailable(_))
        ));
    }
}
