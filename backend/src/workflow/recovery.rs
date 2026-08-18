use std::sync::Arc;

use async_trait::async_trait;

use super::app_server::{AppServerAdapter, ReconcileOutcome};
use super::domain::{NodeCas, NodeRecord, NodeStatus, RunStatus, WorkflowResult};
use super::engine::run_blocking;
use super::journal::{EventDraft, Journal, payload_hash};

#[async_trait]
pub trait ShutdownHooks: Send + Sync {
    async fn pause_admission(&self) -> WorkflowResult<()>;
    async fn interrupt(&self, node: &NodeRecord, idempotency_key: &str) -> WorkflowResult<()>;
}

#[derive(Clone)]
pub struct RecoveryManager {
    journal: Journal,
    adapter: Arc<dyn AppServerAdapter>,
}

impl RecoveryManager {
    pub fn new(journal: Journal, adapter: Arc<dyn AppServerAdapter>) -> Self {
        Self { journal, adapter }
    }

    /// Marks every unfinished run as recovering before consulting app-server.
    /// A write whose completion cannot be proven is left `UnknownOutcome` and
    /// moves the run to `NeedsAttention`; it is never replayed automatically.
    pub async fn recover(&self) -> WorkflowResult<Vec<String>> {
        let journal = self.journal.clone();
        let runs = run_blocking(move || journal.unfinished_runs()).await?;
        let mut recovered = Vec::with_capacity(runs.len());
        for original in runs {
            let original_status = original.status;
            let run_id = original.id.clone();
            if original.status != RunStatus::Recovering {
                let journal = self.journal.clone();
                let transitioning_id = run_id.clone();
                run_blocking(move || {
                    journal.transition_run(
                        &transitioning_id,
                        original.revision,
                        original.generation,
                        RunStatus::Recovering,
                        &EventDraft::new(
                            "run.recovering",
                            serde_json::json!({ "previousStatus": original_status }),
                        ),
                    )
                })
                .await?;
            }

            let journal = self.journal.clone();
            let lookup_id = run_id.clone();
            let nodes = run_blocking(move || journal.nodes(&lookup_id)).await?;
            let mut needs_attention = false;
            let mut had_active = false;
            for node in nodes {
                if !matches!(
                    node.status,
                    NodeStatus::Leased | NodeStatus::Running | NodeStatus::WaitingApproval
                ) {
                    continue;
                }
                had_active = true;
                let outcome = match node.thread_id.as_deref() {
                    Some(thread_id) => self
                        .adapter
                        .reconcile_thread(&run_id, &node.id, thread_id)
                        .await
                        .unwrap_or(ReconcileOutcome::Unknown),
                    None if node.status == NodeStatus::Leased => ReconcileOutcome::NotFound,
                    None => ReconcileOutcome::Unknown,
                };
                if self.reconcile_node(&node, outcome).await? {
                    needs_attention = true;
                }
            }

            let journal = self.journal.clone();
            let lookup_id = run_id.clone();
            let current = run_blocking(move || journal.get_run(&lookup_id)).await?;
            let journal = self.journal.clone();
            let lookup_id = run_id.clone();
            let has_active = run_blocking(move || journal.nodes(&lookup_id))
                .await?
                .iter()
                .any(|node| {
                    matches!(
                        node.status,
                        NodeStatus::Leased | NodeStatus::Running | NodeStatus::WaitingApproval
                    )
                });
            let target = if needs_attention || original_status == RunStatus::NeedsAttention {
                RunStatus::NeedsAttention
            } else if matches!(original_status, RunStatus::Paused | RunStatus::Pausing) {
                RunStatus::Paused
            } else if original_status == RunStatus::Canceling {
                if has_active {
                    // A cancellation whose active turn cannot be proven stopped
                    // must be surfaced, not left forever in a non-runnable state.
                    RunStatus::NeedsAttention
                } else {
                    RunStatus::Canceled
                }
            } else if had_active || has_active {
                RunStatus::Running
            } else {
                RunStatus::Queued
            };
            let journal = self.journal.clone();
            let transition_id = run_id.clone();
            run_blocking(move || {
                journal.transition_run(
                    &transition_id,
                    current.revision,
                    current.generation,
                    target,
                    &EventDraft::new("run.recovered", serde_json::json!({ "status": target })),
                )
            })
            .await?;
            recovered.push(run_id);
        }
        Ok(recovered)
    }

    async fn reconcile_node(
        &self,
        node: &NodeRecord,
        outcome: ReconcileOutcome,
    ) -> WorkflowResult<bool> {
        let recovered_result = match &outcome {
            ReconcileOutcome::Succeeded(result) => Some(result.clone()),
            _ => None,
        };
        let target = match outcome {
            ReconcileOutcome::Running => {
                if node.status == NodeStatus::Leased {
                    NodeStatus::Running
                } else {
                    return Ok(false);
                }
            }
            ReconcileOutcome::WaitingApproval => {
                if node.status == NodeStatus::Running {
                    NodeStatus::WaitingApproval
                } else {
                    return Ok(false);
                }
            }
            ReconcileOutcome::Succeeded(_) => NodeStatus::Succeeded,
            ReconcileOutcome::Failed if node.repo_writer => NodeStatus::UnknownOutcome,
            ReconcileOutcome::Failed => NodeStatus::Failed,
            ReconcileOutcome::NotFound if node.status == NodeStatus::Leased => NodeStatus::Ready,
            ReconcileOutcome::NotFound | ReconcileOutcome::Unknown => NodeStatus::UnknownOutcome,
        };
        let cas = node_cas(node);
        let journal = self.journal.clone();
        let run_id = node.run_id.clone();
        let node_id = node.id.clone();
        run_blocking(move || {
            journal.transition_node(
                &run_id,
                &node_id,
                &cas,
                target,
                recovered_result.as_ref(),
                &EventDraft::new(
                    "node.reconciled",
                    serde_json::json!({ "nodeId": node_id, "status": target }),
                ),
            )
        })
        .await?;

        if !matches!(target, NodeStatus::UnknownOutcome | NodeStatus::Failed) {
            return Ok(false);
        }
        if node.repo_writer {
            return Ok(true);
        }
        let journal = self.journal.clone();
        let run_id = node.run_id.clone();
        let node_id = node.id.clone();
        let refreshed = {
            let journal = journal.clone();
            let lookup_run = run_id.clone();
            let lookup_node = node_id.clone();
            run_blocking(move || journal.get_node(&lookup_run, &lookup_node)).await?
        };
        let next_hash = payload_hash(&serde_json::json!({
            "recoveredFrom": refreshed.payload_hash,
            "generation": refreshed.generation + 1,
        }))?;
        run_blocking(move || {
            journal.reset_node_for_retry(
                &run_id,
                &node_id,
                &node_cas(&refreshed),
                &next_hash,
                &EventDraft::new(
                    "node.recovery_retry_scheduled",
                    serde_json::json!({ "nodeId": node_id }),
                ),
            )
        })
        .await?;
        Ok(false)
    }

    pub async fn shutdown(&self, hooks: &dyn ShutdownHooks) -> WorkflowResult<()> {
        hooks.pause_admission().await?;
        let journal = self.journal.clone();
        let runs = run_blocking(move || journal.unfinished_runs()).await?;
        for run in runs {
            if run.status == RunStatus::Paused {
                continue;
            }
            if run.status == RunStatus::Canceling {
                continue;
            }
            let run_id = run.id.clone();
            let journal = self.journal.clone();
            run_blocking(move || {
                journal.transition_run(
                    &run_id,
                    run.revision,
                    run.generation,
                    RunStatus::Pausing,
                    &EventDraft::new("run.shutdown_pausing", serde_json::json!({})),
                )
            })
            .await?;
            let journal = self.journal.clone();
            let lookup_id = run.id.clone();
            let nodes = run_blocking(move || journal.nodes(&lookup_id)).await?;
            for node in nodes.iter().filter(|node| {
                matches!(
                    node.status,
                    NodeStatus::Leased | NodeStatus::Running | NodeStatus::WaitingApproval
                )
            }) {
                hooks
                    .interrupt(node, &format!("shutdown:{}:{}", run.id, node.id))
                    .await?;
            }
            let journal = self.journal.clone();
            let lookup_id = run.id.clone();
            let pausing = run_blocking(move || journal.get_run(&lookup_id)).await?;
            let journal = self.journal.clone();
            let transition_id = run.id;
            run_blocking(move || {
                journal.transition_run(
                    &transition_id,
                    pausing.revision,
                    pausing.generation,
                    RunStatus::Paused,
                    &EventDraft::new("run.shutdown_paused", serde_json::json!({})),
                )
            })
            .await?;
        }
        Ok(())
    }
}

fn node_cas(node: &NodeRecord) -> NodeCas {
    NodeCas {
        expected_revision: node.revision,
        expected_generation: node.generation,
        expected_lease_epoch: node.lease_epoch,
        expected_payload_hash: node.payload_hash.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::app_server::FakeAppServerAdapter;
    use crate::workflow::artifacts::ArtifactStore;
    use crate::workflow::domain::{PermissionSet, RouteMode, WorkspaceRisk};
    use crate::workflow::engine::{StartWorkflowRequest, WorkflowService};
    use crate::workflow::policy::WorkflowPolicy;
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::time::Duration;

    #[tokio::test]
    async fn recovery_reconciles_a_completed_read_worker_without_replaying_it() {
        let temp = tempfile::tempdir().unwrap();
        let journal = Journal::open(temp.path().join("workflow.sqlite")).unwrap();
        let artifacts = ArtifactStore::open(temp.path().join("artifacts")).unwrap();
        let adapter = Arc::new(FakeAppServerAdapter::default());
        let service = WorkflowService::new(
            journal.clone(),
            artifacts,
            adapter.clone(),
            WorkflowPolicy::default(),
        );
        service
            .start(StartWorkflowRequest {
                command_id: "start-recovery".to_string(),
                run_id: Some("run-recovery".to_string()),
                route: RouteMode::Direct,
                input: serde_json::json!({
                    "originalRequest": "inspect",
                    "cwd": "/tmp",
                    "permissionSnapshot": {
                        "approvalPolicy": "never",
                        "sandboxMode": "read-only"
                    }
                }),
                permissions: PermissionSet {
                    read_paths: BTreeSet::from(["/tmp".to_string()]),
                    ..PermissionSet::default()
                },
                workspace_risk: WorkspaceRisk::default(),
                isolated_workspace_available: false,
            })
            .await
            .unwrap();
        let run = service.get("run-recovery").await.unwrap().run;
        journal
            .transition_run(
                &run.id,
                run.revision,
                run.generation,
                RunStatus::Running,
                &EventDraft::new("run.running", serde_json::json!({})),
            )
            .unwrap();
        let (started, _) = service
            .dispatch_node("run-recovery", "builder", "worker", Duration::from_secs(30))
            .await
            .unwrap();
        adapter.queue_reconcile(
            &started.thread_id,
            ReconcileOutcome::Succeeded(serde_json::json!({ "text": "recovered" })),
        );

        let recovered = RecoveryManager::new(journal.clone(), adapter)
            .recover()
            .await
            .unwrap();

        assert_eq!(recovered, ["run-recovery"]);
        assert_eq!(
            journal.get_node("run-recovery", "builder").unwrap().status,
            NodeStatus::Succeeded
        );
        assert_eq!(
            journal.get_run("run-recovery").unwrap().status,
            RunStatus::Running
        );
    }

    #[tokio::test]
    async fn recovery_requeues_a_failed_read_worker() {
        let temp = tempfile::tempdir().unwrap();
        let journal = Journal::open(temp.path().join("workflow.sqlite")).unwrap();
        let artifacts = ArtifactStore::open(temp.path().join("artifacts")).unwrap();
        let adapter = Arc::new(FakeAppServerAdapter::default());
        let service = WorkflowService::new(
            journal.clone(),
            artifacts,
            adapter.clone(),
            WorkflowPolicy::default(),
        );
        service
            .start(StartWorkflowRequest {
                command_id: "start-retry-recovery".to_string(),
                run_id: Some("run-retry-recovery".to_string()),
                route: RouteMode::Direct,
                input: serde_json::json!({
                    "originalRequest": "inspect",
                    "cwd": "/tmp",
                    "permissionSnapshot": {
                        "approvalPolicy": "never",
                        "sandboxMode": "read-only"
                    }
                }),
                permissions: PermissionSet {
                    read_paths: BTreeSet::from(["/tmp".to_string()]),
                    ..PermissionSet::default()
                },
                workspace_risk: WorkspaceRisk::default(),
                isolated_workspace_available: false,
            })
            .await
            .unwrap();
        let run = service.get("run-retry-recovery").await.unwrap().run;
        journal
            .transition_run(
                &run.id,
                run.revision,
                run.generation,
                RunStatus::Running,
                &EventDraft::new("run.running", serde_json::json!({})),
            )
            .unwrap();
        let (started, _) = service
            .dispatch_node(
                "run-retry-recovery",
                "builder",
                "worker",
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        adapter.queue_reconcile(&started.thread_id, ReconcileOutcome::Failed);

        RecoveryManager::new(journal.clone(), adapter)
            .recover()
            .await
            .unwrap();

        let node = journal.get_node("run-retry-recovery", "builder").unwrap();
        assert_eq!(node.status, NodeStatus::Ready);
        assert_eq!(node.generation, 1);
    }

    async fn start_idle_run(
        journal: &Journal,
        artifact_root: &Path,
        adapter: Arc<FakeAppServerAdapter>,
        run_id: &str,
    ) -> WorkflowService {
        let artifacts = ArtifactStore::open(artifact_root).unwrap();
        let service = WorkflowService::new(
            journal.clone(),
            artifacts,
            adapter,
            WorkflowPolicy::default(),
        );
        service
            .start(StartWorkflowRequest {
                command_id: format!("start-{run_id}"),
                run_id: Some(run_id.to_string()),
                route: RouteMode::Direct,
                input: serde_json::json!({
                    "originalRequest": "inspect",
                    "cwd": "/tmp",
                    "permissionSnapshot": {
                        "approvalPolicy": "never",
                        "sandboxMode": "read-only"
                    }
                }),
                permissions: PermissionSet {
                    read_paths: BTreeSet::from(["/tmp".to_string()]),
                    ..PermissionSet::default()
                },
                workspace_risk: WorkspaceRisk::default(),
                isolated_workspace_available: false,
            })
            .await
            .unwrap();
        service
    }

    #[tokio::test]
    async fn recovery_preserves_needs_attention_without_automatic_replay() {
        let temp = tempfile::tempdir().unwrap();
        let journal = Journal::open(temp.path().join("workflow.sqlite")).unwrap();
        let adapter = Arc::new(FakeAppServerAdapter::default());
        let service = start_idle_run(
            &journal,
            &temp.path().join("attention-artifacts"),
            adapter.clone(),
            "run-attention",
        )
        .await;
        let run = service.get("run-attention").await.unwrap().run;
        journal
            .transition_run(
                &run.id,
                run.revision,
                run.generation,
                RunStatus::NeedsAttention,
                &EventDraft::new("run.needs_attention", serde_json::json!({})),
            )
            .unwrap();

        RecoveryManager::new(journal.clone(), adapter)
            .recover()
            .await
            .unwrap();

        assert_eq!(
            journal.get_run("run-attention").unwrap().status,
            RunStatus::NeedsAttention
        );
    }

    #[tokio::test]
    async fn recovery_settles_an_idle_canceling_run() {
        let temp = tempfile::tempdir().unwrap();
        let journal = Journal::open(temp.path().join("workflow.sqlite")).unwrap();
        let adapter = Arc::new(FakeAppServerAdapter::default());
        let service = start_idle_run(
            &journal,
            &temp.path().join("cancel-artifacts"),
            adapter.clone(),
            "run-canceling",
        )
        .await;
        let run = service.get("run-canceling").await.unwrap().run;
        journal
            .transition_run(
                &run.id,
                run.revision,
                run.generation,
                RunStatus::Canceling,
                &EventDraft::new("run.canceling", serde_json::json!({})),
            )
            .unwrap();

        RecoveryManager::new(journal.clone(), adapter)
            .recover()
            .await
            .unwrap();

        assert_eq!(
            journal.get_run("run-canceling").unwrap().status,
            RunStatus::Canceled
        );
    }
}
