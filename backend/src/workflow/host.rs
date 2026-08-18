use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::task::{JoinHandle, JoinSet};

use crate::codex_startup_patch::WorkflowProxyLaunchConfig;
use crate::config::{CodeyConfig, SubagentRoleConfig, WorkflowConfig};

use super::app_server::AppServerAdapter;
use super::artifacts::ArtifactStore;
use super::domain::{
    DurableAck, NodeCas, NodeRecord, NodeRole, NodeStatus, PermissionSet, ReviewerVerdict,
    RouteMode, RunRecord, RunStatus, WorkflowError, WorkflowEvent, WorkflowResult, WorkspaceRisk,
    WorkspaceRiskLevel, parse_reviewer_verdict,
};
use super::engine::{
    ApprovalReplyRequest, ListRunsRequest, RetryNodeRequest, RunCommandRequest,
    StartWorkflowRequest, SteerWorkflowRequest, WorkflowService, run_blocking,
};
use super::journal::{EventDraft, Journal, RunSummaryStats, payload_hash};
use super::policy::{RetryClass, RetryDecision, WorkflowPolicy};
use super::proxy_client::{
    ExecutionOutcome, ProxyAppServerAdapter, ProxyHealth, ThreadContext, WorkflowControlClient,
};
use super::recovery::{RecoveryManager, ShutdownHooks};
use super::scheduler::Scheduler;

const PERMISSION_SNAPSHOT_TTL_MS: i64 = 5 * 60 * 1000;
const LEASE_OWNER: &str = "codey-workflow-engine";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionSnapshot {
    pub resolved: bool,
    pub snapshot_id: String,
    pub hash: String,
    pub revision: u64,
    pub approval_policy: String,
    pub sandbox_mode: String,
}

#[derive(Debug, Clone)]
struct FrozenPermissionSnapshot {
    public: PermissionSnapshot,
    raw_approval_policy: Value,
    raw_sandbox: Value,
    cwd: String,
    thread_id: Option<String>,
    created_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct ResolvedComposerContext {
    pub cwd: String,
    pub permission_snapshot: PermissionSnapshot,
    pub active_run: Option<RunRecord>,
    pub latest_turn_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BoundStartRequest {
    pub command_id: String,
    pub text: String,
    pub origin_thread_id: String,
    pub cwd: String,
    pub permission_snapshot_id: String,
    pub source: String,
    pub requested_route: Option<RouteMode>,
}

#[derive(Debug, Clone)]
pub struct StartResult {
    pub ack: DurableAck,
    pub run: RunRecord,
}

pub struct WorkflowHost {
    enabled: bool,
    global_mode: AtomicBool,
    unavailable_reason: Option<String>,
    launch_config: Option<WorkflowProxyLaunchConfig>,
    client: Option<Arc<WorkflowControlClient>>,
    adapter: Option<Arc<ProxyAppServerAdapter>>,
    service: WorkflowService,
    journal: Option<Journal>,
    workflow_config: StdRwLock<WorkflowConfig>,
    permission_snapshots: Mutex<HashMap<String, FrozenPermissionSnapshot>>,
    tasks: Mutex<HashMap<String, JoinHandle<()>>>,
    run_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    scheduler: Arc<StdMutex<Scheduler>>,
    accepting: AtomicBool,
    recovery_started: AtomicBool,
    offline_epoch: String,
}

struct SchedulerReservation {
    scheduler: Arc<StdMutex<Scheduler>>,
    node: NodeRecord,
}

impl Drop for SchedulerReservation {
    fn drop(&mut self) {
        self.scheduler
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .release(&self.node);
    }
}

impl WorkflowHost {
    pub fn from_config(config: &CodeyConfig, config_path: &Path) -> Arc<Self> {
        let workflow_config = config.workflow.clone();
        let offline_epoch = format!("offline-{}", uuid::Uuid::new_v4());
        if !workflow_config.enabled {
            return Arc::new(Self {
                enabled: false,
                global_mode: AtomicBool::new(workflow_config.global_mode),
                unavailable_reason: Some("workflow mode is disabled".to_string()),
                launch_config: None,
                client: None,
                adapter: None,
                service: WorkflowService::unavailable("workflow mode is disabled"),
                journal: None,
                workflow_config: StdRwLock::new(workflow_config),
                permission_snapshots: Mutex::new(HashMap::new()),
                tasks: Mutex::new(HashMap::new()),
                run_locks: Mutex::new(HashMap::new()),
                scheduler: Arc::new(StdMutex::new(Scheduler::default())),
                accepting: AtomicBool::new(false),
                recovery_started: AtomicBool::new(false),
                offline_epoch,
            });
        }

        let base = config_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(std::env::temp_dir);
        match Self::build_enabled(workflow_config.clone(), &base) {
            Ok(host) => Arc::new(host),
            Err(error) => Arc::new(Self {
                enabled: true,
                global_mode: AtomicBool::new(workflow_config.global_mode),
                unavailable_reason: Some(error.to_string()),
                launch_config: None,
                client: None,
                adapter: None,
                service: WorkflowService::unavailable(error.to_string()),
                journal: None,
                workflow_config: StdRwLock::new(workflow_config),
                permission_snapshots: Mutex::new(HashMap::new()),
                tasks: Mutex::new(HashMap::new()),
                run_locks: Mutex::new(HashMap::new()),
                scheduler: Arc::new(StdMutex::new(Scheduler::default())),
                accepting: AtomicBool::new(false),
                recovery_started: AtomicBool::new(false),
                offline_epoch,
            }),
        }
    }

    fn build_enabled(workflow_config: WorkflowConfig, base: &Path) -> WorkflowResult<Self> {
        let journal_path = base.join("workflow.sqlite");
        let artifact_root = base.join("workflow-artifacts");
        let journal = Journal::open(&journal_path)?;
        let artifacts = ArtifactStore::open(&artifact_root)?;
        let executable = std::env::current_exe()
            .map_err(|error| WorkflowError::Unavailable(error.to_string()))?;
        if !executable.is_absolute() {
            return Err(WorkflowError::Unavailable(
                "Codey executable path is not absolute".to_string(),
            ));
        }
        let port = crate::codex_startup_patch::reserve_loopback_port()
            .map_err(|error| WorkflowError::Unavailable(error.to_string()))?;
        let capability_token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let control_address = format!("127.0.0.1:{port}");
        let launch_config = WorkflowProxyLaunchConfig {
            executable: executable.to_string_lossy().to_string(),
            control_address: control_address.clone(),
            capability_token: capability_token.clone(),
        };
        let client = Arc::new(WorkflowControlClient::new(
            control_address,
            capability_token,
        ));
        let adapter = Arc::new(ProxyAppServerAdapter::new(client.clone()));
        let policy = workflow_policy(&workflow_config);
        let service = WorkflowService::new(journal.clone(), artifacts, adapter.clone(), policy);
        Ok(Self {
            enabled: true,
            global_mode: AtomicBool::new(workflow_config.global_mode),
            unavailable_reason: None,
            launch_config: Some(launch_config),
            client: Some(client),
            adapter: Some(adapter),
            service,
            journal: Some(journal),
            workflow_config: StdRwLock::new(workflow_config),
            permission_snapshots: Mutex::new(HashMap::new()),
            tasks: Mutex::new(HashMap::new()),
            run_locks: Mutex::new(HashMap::new()),
            scheduler: Arc::new(StdMutex::new(Scheduler::default())),
            accepting: AtomicBool::new(true),
            recovery_started: AtomicBool::new(false),
            offline_epoch: format!("offline-{}", uuid::Uuid::new_v4()),
        })
    }

    pub fn global_mode(&self) -> bool {
        self.global_mode.load(Ordering::Acquire)
    }

    pub fn set_global_mode(&self, enabled: bool) {
        self.global_mode.store(enabled, Ordering::Release);
    }

    pub fn apply_config(&self, config: WorkflowConfig) {
        self.set_global_mode(config.global_mode);
        if self.enabled
            && config.enabled
            && let Err(error) = self.service.update_policy(workflow_policy(&config))
        {
            crate::error_log::record_failure(
                "workflow_policy_update_failed",
                "apply_workflow_config",
                error.to_string(),
                json!({ "recoverable": true }),
            );
        }
        *self
            .workflow_config
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = config;
    }

    pub fn proxy_launch_config(&self) -> Option<&WorkflowProxyLaunchConfig> {
        self.launch_config.as_ref()
    }

    pub fn service(&self) -> &WorkflowService {
        &self.service
    }

    pub(crate) fn journal(&self) -> Option<&Journal> {
        self.journal.as_ref()
    }

    pub async fn proxy_health(&self) -> ProxyHealth {
        match &self.client {
            Some(client) => client.health().await,
            None => ProxyHealth {
                connected: false,
                accepting: false,
                engine_epoch: Some(self.offline_epoch.clone()),
                reason: self.unavailable_reason.clone(),
            },
        }
    }

    pub async fn resolve_composer_context(
        &self,
        thread_id: Option<&str>,
        cwd_hint: Option<&str>,
    ) -> WorkflowResult<ResolvedComposerContext> {
        if !self.enabled || !self.global_mode() || !self.accepting.load(Ordering::Acquire) {
            return Err(WorkflowError::Unavailable(
                self.unavailable_reason
                    .clone()
                    .unwrap_or_else(|| "workflow admission is paused".to_string()),
            ));
        }
        let adapter = self.adapter.as_ref().ok_or_else(|| {
            WorkflowError::Unavailable("app-server adapter is unavailable".to_string())
        })?;
        if let Some(thread_id) = thread_id.filter(|value| !value.trim().is_empty()) {
            if let Some(run) = self.active_run_for_origin(thread_id).await? {
                let public: PermissionSnapshot = serde_json::from_value(
                    run.input
                        .get("permissionSnapshot")
                        .cloned()
                        .ok_or_else(|| {
                            WorkflowError::Storage(
                                "active workflow is missing its permission snapshot".to_string(),
                            )
                        })?,
                )?;
                return Ok(ResolvedComposerContext {
                    cwd: run
                        .input
                        .get("cwd")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    permission_snapshot: public,
                    active_run: Some(run.clone()),
                    latest_turn_id: run
                        .input
                        .get("originTurnId")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                });
            }
            let context = adapter.inspect_thread(thread_id).await?;
            let snapshot = self.freeze_context(Some(thread_id), &context).await?;
            return Ok(ResolvedComposerContext {
                cwd: context.cwd,
                permission_snapshot: snapshot,
                active_run: None,
                latest_turn_id: context.latest_turn_id,
            });
        }
        let cwd = cwd_hint
            .map(str::trim)
            .filter(|value| is_absolute_path(value))
            .ok_or_else(|| {
                WorkflowError::InvalidRequest(
                    "new tasks require a verified absolute workspace path".to_string(),
                )
            })?;
        let context = adapter.inspect_defaults(cwd).await?;
        let snapshot = self.freeze_context(None, &context).await?;
        Ok(ResolvedComposerContext {
            cwd: context.cwd,
            permission_snapshot: snapshot,
            active_run: None,
            latest_turn_id: None,
        })
    }

    async fn freeze_context(
        &self,
        thread_id: Option<&str>,
        context: &ThreadContext,
    ) -> WorkflowResult<PermissionSnapshot> {
        if !is_absolute_path(&context.cwd) {
            return Err(WorkflowError::InvalidRequest(
                "Codex returned a non-absolute workspace path".to_string(),
            ));
        }
        let snapshot_id = uuid::Uuid::new_v4().to_string();
        let approval_policy = approval_policy_label(&context.approval_policy);
        let sandbox_mode = sandbox_mode_label(&context.sandbox)?;
        let revision = 0;
        let hash = payload_hash(&json!({
            "snapshotId": snapshot_id,
            "revision": revision,
            "cwd": context.cwd,
            "threadId": thread_id,
            "approvalPolicy": context.approval_policy,
            "sandbox": context.sandbox,
        }))?;
        let public = PermissionSnapshot {
            resolved: true,
            snapshot_id: snapshot_id.clone(),
            hash,
            revision,
            approval_policy,
            sandbox_mode,
        };
        let frozen = FrozenPermissionSnapshot {
            public: public.clone(),
            raw_approval_policy: context.approval_policy.clone(),
            raw_sandbox: context.sandbox.clone(),
            cwd: context.cwd.clone(),
            thread_id: thread_id.map(ToOwned::to_owned),
            created_at_ms: super::journal::now_ms(),
        };
        let mut snapshots = self.permission_snapshots.lock().await;
        let oldest = super::journal::now_ms().saturating_sub(PERMISSION_SNAPSHOT_TTL_MS);
        snapshots.retain(|_, value| value.created_at_ms >= oldest);
        snapshots.insert(snapshot_id, frozen);
        Ok(public)
    }

    async fn frozen_snapshot(
        &self,
        snapshot_id: &str,
        cwd: &str,
        thread_id: Option<&str>,
    ) -> WorkflowResult<FrozenPermissionSnapshot> {
        let snapshots = self.permission_snapshots.lock().await;
        let snapshot = snapshots.get(snapshot_id).cloned().ok_or_else(|| {
            WorkflowError::Conflict("permission snapshot expired or is unknown".to_string())
        })?;
        if super::journal::now_ms().saturating_sub(snapshot.created_at_ms)
            > PERMISSION_SNAPSHOT_TTL_MS
            || snapshot.cwd != cwd
            || snapshot.thread_id.as_deref() != thread_id
        {
            return Err(WorkflowError::Conflict(
                "permission snapshot no longer matches the composer context".to_string(),
            ));
        }
        Ok(snapshot)
    }

    pub async fn create_origin_for_snapshot(
        &self,
        snapshot_id: &str,
        cwd: &str,
    ) -> WorkflowResult<ThreadContext> {
        let snapshot = self.frozen_snapshot(snapshot_id, cwd, None).await?;
        self.adapter
            .as_ref()
            .ok_or_else(|| {
                WorkflowError::Unavailable("app-server adapter unavailable".to_string())
            })?
            .start_origin(cwd, snapshot.raw_approval_policy, snapshot.raw_sandbox)
            .await
    }

    pub async fn discard_origin(&self, thread_id: &str) {
        if let Some(adapter) = &self.adapter {
            let _ = adapter.delete_thread(thread_id).await;
        }
    }

    pub async fn start_bound(
        self: &Arc<Self>,
        request: BoundStartRequest,
    ) -> WorkflowResult<StartResult> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(WorkflowError::Unavailable(
                "workflow admission is paused".to_string(),
            ));
        }
        let frozen = match self
            .frozen_snapshot(
                &request.permission_snapshot_id,
                &request.cwd,
                Some(&request.origin_thread_id),
            )
            .await
        {
            Ok(value) => value,
            Err(original_error) => {
                // A new origin is created from a snapshot issued before that
                // thread existed. It remains fenced by snapshot id, cwd and TTL.
                let snapshots = self.permission_snapshots.lock().await;
                let value = snapshots
                    .get(&request.permission_snapshot_id)
                    .cloned()
                    .ok_or(original_error)?;
                if value.cwd != request.cwd
                    || value.created_at_ms
                        < super::journal::now_ms().saturating_sub(PERMISSION_SNAPSHOT_TTL_MS)
                    || value
                        .thread_id
                        .as_deref()
                        .is_some_and(|thread| thread != request.origin_thread_id)
                {
                    return Err(WorkflowError::Conflict(
                        "permission snapshot does not match the bound origin".to_string(),
                    ));
                }
                value
            }
        };
        let (permissions, request_intent) =
            permissions_for_request(permissions_from_snapshot(&frozen), &request.text);
        let route = request
            .requested_route
            .unwrap_or_else(|| classify_route(&request.text, &permissions, request_intent));
        let workspace_risk = workspace_risk(&request.cwd).await;
        let workflow_config = self
            .workflow_config
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let run_id = uuid::Uuid::new_v4().to_string();
        let input = json!({
            "originalRequest": request.text,
            "title": compact_title(&request.text),
            "originThreadId": request.origin_thread_id,
            "originTurnId": format!("workflow:{run_id}"),
            "cwd": request.cwd,
            "permissionSnapshot": frozen.public,
            "permissionRuntime": {
                "approvalPolicy": frozen.raw_approval_policy,
                "sandbox": frozen.raw_sandbox,
            },
            "roleModels": role_models_for_run(&workflow_config.roles),
            "runtimePolicy": {
                "leaseSeconds": workflow_config.lease_seconds,
            },
            "source": request.source,
            "profile": workflow_config.profile,
            "requestIntent": request_intent.as_str(),
        });
        let ack = self
            .service
            .start(StartWorkflowRequest {
                command_id: request.command_id,
                run_id: Some(run_id.clone()),
                route,
                input,
                permissions,
                workspace_risk,
                isolated_workspace_available: false,
            })
            .await?;
        let run = self.service.get(&ack.run_id).await?.run;
        let adapter = self.adapter.as_ref().ok_or_else(|| {
            WorkflowError::Unavailable("app-server adapter unavailable".to_string())
        })?;
        if run.route != RouteMode::Direct {
            adapter
                .inject_user_once(
                    run.input
                        .get("originThreadId")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    run.input
                        .get("originalRequest")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                )
                .await?;
        }
        if let Some(journal) = &self.journal {
            let journal = journal.clone();
            let run_id = ack.run_id.clone();
            let direct = run.route == RouteMode::Direct;
            run_blocking(move || {
                journal.append_event(
                    &run_id,
                    &EventDraft::new(
                        "origin.original_request_injected",
                        json!({ "reconciled": true, "viaTurnStart": direct }),
                    ),
                )
            })
            .await?;
        }
        self.spawn_executor(ack.run_id.clone()).await;
        Ok(StartResult { ack, run })
    }

    pub async fn replay_start(
        self: &Arc<Self>,
        command_id: &str,
        text: &str,
        cwd: &str,
        permission_snapshot_hash: &str,
    ) -> WorkflowResult<Option<StartResult>> {
        let Some(journal) = &self.journal else {
            return Ok(None);
        };
        let journal = journal.clone();
        let command_id = command_id.to_string();
        let Some(stored) = run_blocking(move || journal.command(&command_id)).await? else {
            return Ok(None);
        };
        let mut ack: DurableAck = serde_json::from_str(&stored.response_json)?;
        let run = self.service.get(&ack.run_id).await?.run;
        let same_request = run.input.get("originalRequest").and_then(Value::as_str) == Some(text)
            && run.input.get("cwd").and_then(Value::as_str) == Some(cwd)
            && run
                .input
                .get("permissionSnapshot")
                .and_then(|snapshot| snapshot.get("hash"))
                .and_then(Value::as_str)
                == Some(permission_snapshot_hash);
        if !same_request {
            return Err(WorkflowError::Conflict(
                "command id was already used with a different workflow request".to_string(),
            ));
        }
        let origin_thread_id = run
            .input
            .get("originThreadId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                WorkflowError::Storage(
                    "replayed workflow is missing its origin thread binding".to_string(),
                )
            })?;
        if run.route != RouteMode::Direct {
            self.adapter
                .as_ref()
                .ok_or_else(|| {
                    WorkflowError::Unavailable("app-server adapter unavailable".to_string())
                })?
                .inject_user_once(origin_thread_id, text)
                .await?;
        }
        ack.duplicate = true;
        if matches!(run.status, RunStatus::Queued | RunStatus::Running) {
            self.spawn_executor(run.id.clone()).await;
        }
        Ok(Some(StartResult { ack, run }))
    }

    pub async fn steer(
        self: &Arc<Self>,
        command_id: String,
        run_id: String,
        expected_revision: u64,
        text: String,
    ) -> WorkflowResult<DurableAck> {
        let run_lock = self.run_lock(&run_id).await;
        let guard = run_lock.lock().await;
        let current = self.service.get(&run_id).await?.run;
        if current.revision != expected_revision {
            return Err(WorkflowError::Conflict(format!(
                "run revision changed from {expected_revision} to {}",
                current.revision
            )));
        }
        let ack = self
            .service
            .steer(SteerWorkflowRequest {
                command_id,
                run_id: run_id.clone(),
                node_id: None,
                message: json!({ "text": text }),
            })
            .await?;
        drop(guard);
        self.spawn_executor(run_id).await;
        Ok(ack)
    }

    pub async fn active_run_for_origin(
        &self,
        origin_thread_id: &str,
    ) -> WorkflowResult<Option<RunRecord>> {
        let Some(journal) = &self.journal else {
            return Ok(None);
        };
        let journal = journal.clone();
        let origin_thread_id = origin_thread_id.to_string();
        run_blocking(move || {
            let mut runs = journal.unfinished_runs()?;
            runs.sort_by_key(|run| std::cmp::Reverse(run.updated_at_ms));
            Ok(runs.into_iter().find(|run| {
                run.input.get("originThreadId").and_then(Value::as_str)
                    == Some(origin_thread_id.as_str())
            }))
        })
        .await
    }

    pub async fn recover_when_ready(self: &Arc<Self>) {
        if !self.enabled
            || self.recovery_started.swap(true, Ordering::AcqRel)
            || self.journal.is_none()
            || self.adapter.is_none()
        {
            return;
        }
        let host = Arc::clone(self);
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
            loop {
                let health = host.proxy_health().await;
                if health.accepting {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    host.recovery_started.store(false, Ordering::Release);
                    return;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            let manager = RecoveryManager::new(
                host.journal.as_ref().expect("journal checked").clone(),
                host.adapter.as_ref().expect("adapter checked").clone(),
            );
            if let Ok(run_ids) = manager.recover().await {
                for run_id in run_ids {
                    if host.service.get(&run_id).await.is_ok_and(|details| {
                        matches!(details.run.status, RunStatus::Queued | RunStatus::Running)
                    }) {
                        host.spawn_executor(run_id).await;
                    }
                }
            }
        });
    }

    pub async fn shutdown(self: &Arc<Self>) {
        self.accepting.store(false, Ordering::Release);
        let (Some(journal), Some(adapter)) = (&self.journal, &self.adapter) else {
            return;
        };
        let manager = RecoveryManager::new(journal.clone(), adapter.clone());
        if let Err(error) = manager.shutdown(self.as_ref()).await {
            crate::error_log::record_failure(
                "workflow_shutdown_failed",
                "pause_active_workflows",
                error.to_string(),
                json!({ "recoverable": true }),
            );
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let run_ids = self.tasks.lock().await.keys().cloned().collect::<Vec<_>>();
        for run_id in run_ids {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if let Err(error) = self.quiesce_executor(&run_id, remaining).await {
                crate::error_log::record_failure(
                    "workflow_shutdown_settlement_failed",
                    "settle_interrupted_workflow",
                    error.to_string(),
                    json!({ "runId": run_id, "recoverable": true }),
                );
            }
        }
    }

    async fn spawn_executor(self: &Arc<Self>, run_id: String) {
        let mut tasks = self.tasks.lock().await;
        if tasks.get(&run_id).is_some_and(|task| !task.is_finished()) {
            return;
        }
        let host = Arc::clone(self);
        let task_run_id = run_id.clone();
        let task = tokio::spawn(async move {
            if let Err(error) = host.execute_run(&task_run_id).await {
                crate::error_log::record_failure(
                    "workflow_executor_failed",
                    "execute_workflow_run",
                    error.to_string(),
                    json!({ "runId": task_run_id, "recoverable": true }),
                );
                if let Ok(details) = host.service.get(&task_run_id).await
                    && matches!(
                        details.run.status,
                        RunStatus::Queued | RunStatus::Running | RunStatus::Recovering
                    )
                    && let Err(settle_error) = host
                        .mark_needs_attention(
                            &details.run,
                            format!("workflow executor stopped unexpectedly: {error}"),
                        )
                        .await
                {
                    crate::error_log::record_failure(
                        "workflow_executor_settlement_failed",
                        "mark_workflow_needs_attention",
                        settle_error.to_string(),
                        json!({ "runId": task_run_id, "recoverable": true }),
                    );
                }
            }
            host.tasks.lock().await.remove(&task_run_id);
            if host
                .service
                .get(&task_run_id)
                .await
                .is_ok_and(|details| details.run.status.is_terminal())
            {
                host.run_locks.lock().await.remove(&task_run_id);
            }
        });
        tasks.insert(run_id, task);
    }

    async fn run_lock(&self, run_id: &str) -> Arc<Mutex<()>> {
        self.run_locks
            .lock()
            .await
            .entry(run_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn quiesce_executor(&self, run_id: &str, timeout: Duration) -> WorkflowResult<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let active = self
                .tasks
                .lock()
                .await
                .get(run_id)
                .is_some_and(|task| !task.is_finished());
            if !active {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        if let Some(task) = self.tasks.lock().await.remove(run_id) {
            task.abort();
            let _ = task.await;
        }
        self.settle_aborted_nodes(run_id).await
    }

    async fn settle_aborted_nodes(&self, run_id: &str) -> WorkflowResult<()> {
        let details = self.service.get(run_id.to_string()).await?;
        for node in details.nodes.into_iter().filter(|node| {
            matches!(
                node.status,
                NodeStatus::Leased | NodeStatus::Running | NodeStatus::WaitingApproval
            )
        }) {
            if node.status == NodeStatus::Leased {
                let target = if node.repo_writer {
                    NodeStatus::UnknownOutcome
                } else {
                    NodeStatus::Ready
                };
                let journal = self.journal.as_ref().expect("available service").clone();
                let run_id = run_id.to_string();
                let node_id = node.id.clone();
                let cas = node_cas(&node);
                run_blocking(move || {
                    journal
                        .transition_node(
                            &run_id,
                            &node_id,
                            &cas,
                            target,
                            None,
                            &EventDraft::new(
                                "node.interrupt_settled",
                                json!({ "nodeId": node_id, "status": target }),
                            ),
                        )
                        .map(|_| ())
                })
                .await?;
            } else if node.repo_writer {
                self.finish_node(
                    run_id,
                    &node.id,
                    NodeStatus::UnknownOutcome,
                    None,
                    Some("worker did not settle within the interrupt deadline".to_string()),
                )
                .await?;
            } else {
                self.finish_infrastructure_failure(
                    run_id,
                    &node.id,
                    "read-only worker did not settle within the interrupt deadline".to_string(),
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn execute_run(self: &Arc<Self>, run_id: &str) -> WorkflowResult<()> {
        let initial = self.service.get(run_id).await?.run;
        if initial.status == RunStatus::Queued {
            let journal = self.journal.as_ref().expect("available service").clone();
            let run_id_owned = run_id.to_string();
            run_blocking(move || {
                journal.transition_run(
                    &run_id_owned,
                    initial.revision,
                    initial.generation,
                    RunStatus::Running,
                    &EventDraft::new("run.running", json!({})),
                )
            })
            .await?;
        }
        loop {
            let details = self.service.get(run_id).await?;
            if details.run.status != RunStatus::Running {
                return Ok(());
            }
            let journal = self.journal.as_ref().expect("available service").clone();
            let promote_run_id = run_id.to_string();
            let _ = run_blocking(move || journal.promote_ready_nodes(&promote_run_id)).await?;
            let details = self.service.get(run_id).await?;
            if let Some(problem) = details.nodes.iter().find(|node| {
                matches!(
                    node.status,
                    NodeStatus::Failed | NodeStatus::UnknownOutcome | NodeStatus::WaitingApproval
                )
            }) {
                self.mark_needs_attention(
                    &details.run,
                    format!(
                        "node {} requires attention ({})",
                        problem.id, problem.status
                    ),
                )
                .await?;
                return Ok(());
            }
            if details.nodes.iter().any(|node| {
                node.role == NodeRole::FinalDelivery && node.status == NodeStatus::Ready
            }) {
                let run_lock = self.run_lock(run_id).await;
                let _guard = run_lock.lock().await;
                let locked = self.service.get(run_id.to_string()).await?;
                if locked.run.status != RunStatus::Running {
                    return Ok(());
                }
                let Some(final_node) = locked.nodes.iter().find(|node| {
                    node.role == NodeRole::FinalDelivery && node.status == NodeStatus::Ready
                }) else {
                    continue;
                };
                if let Some((steer_sequence, feedback)) =
                    self.stale_builder_steer(run_id, &locked.nodes).await?
                {
                    if self
                        .schedule_steer_recompile(
                            &locked.run,
                            &locked.nodes,
                            steer_sequence,
                            &feedback,
                        )
                        .await?
                    {
                        continue;
                    }
                    self.mark_needs_attention(
                        &locked.run,
                        "the Builder attempt budget was exhausted while applying follow-up input"
                            .to_string(),
                    )
                    .await?;
                    return Ok(());
                }
                match final_decision(&locked.nodes, final_node)? {
                    FinalDecision::Deliver(content) => {
                        match self.service.finalize(run_id.to_string(), content).await {
                            Ok(_) => return Ok(()),
                            Err(WorkflowError::Conflict(_)) => continue,
                            Err(error) => return Err(error),
                        }
                    }
                    FinalDecision::Repair(feedback) => {
                        if self
                            .schedule_builder_repair(&locked.run, &locked.nodes, &feedback)
                            .await?
                        {
                            continue;
                        }
                        self.mark_needs_attention(
                            &locked.run,
                            "independent review still requires changes after two repair cycles"
                                .to_string(),
                        )
                        .await?;
                        return Ok(());
                    }
                    FinalDecision::Inconclusive(reason) => {
                        self.mark_needs_attention(&locked.run, reason).await?;
                        return Ok(());
                    }
                }
            }

            let ready = details
                .nodes
                .iter()
                .filter(|node| {
                    node.status == NodeStatus::Ready && node.role != NodeRole::FinalDelivery
                })
                .cloned()
                .collect::<Vec<_>>();
            if ready.is_empty() {
                if details.nodes.iter().any(|node| {
                    matches!(
                        node.status,
                        NodeStatus::Running | NodeStatus::Leased | NodeStatus::WaitingApproval
                    )
                }) {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                self.mark_needs_attention(
                    &details.run,
                    "workflow DAG has no runnable node".to_string(),
                )
                .await?;
                return Ok(());
            }

            let selected = {
                let mut scheduler = self
                    .scheduler
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let mut selected = Vec::new();
                for candidate in scheduler.ready_nodes(&details.nodes, &details.run.policy) {
                    if candidate.role == NodeRole::FinalDelivery {
                        continue;
                    }
                    if candidate.repo_writer && !selected.is_empty() {
                        continue;
                    }
                    if selected.iter().any(|node: &NodeRecord| node.repo_writer) {
                        break;
                    }
                    if scheduler.reserve(candidate, &details.run.policy).is_ok() {
                        selected.push(candidate.clone());
                    }
                }
                selected
            };
            if selected.is_empty() {
                // Capacity is global across runs and providers. Another run may
                // hold the slot, so this is backpressure rather than a failure.
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            let mut join_set = JoinSet::new();
            for node in selected {
                let host = Arc::clone(self);
                let run_id = run_id.to_string();
                let reservation = SchedulerReservation {
                    scheduler: self.scheduler.clone(),
                    node: node.clone(),
                };
                join_set.spawn(async move {
                    let _reservation = reservation;
                    host.execute_node(&run_id, &node.id).await
                });
            }
            let mut first_error = None;
            while let Some(result) = join_set.join_next().await {
                let result = match result {
                    Ok(result) => result,
                    Err(error) => Err(WorkflowError::Join(error.to_string())),
                };
                if first_error.is_none() {
                    first_error = result.err();
                }
            }
            if let Some(error) = first_error {
                return Err(error);
            }
        }
    }

    async fn schedule_builder_repair(
        &self,
        run: &RunRecord,
        nodes: &[NodeRecord],
        feedback: &str,
    ) -> WorkflowResult<bool> {
        let journal = self.journal.as_ref().expect("available service").clone();
        let lookup_id = run.id.clone();
        let repair_count = run_blocking(move || journal.events_after(&lookup_id, 0, 500))
            .await?
            .into_iter()
            .filter(|event| event.kind == "run.repair_requested")
            .count();
        if repair_count >= usize::from(run.policy.builder_repair_limit) {
            return Ok(false);
        }
        let builder = nodes
            .iter()
            .find(|node| node.role == NodeRole::Builder)
            .cloned()
            .ok_or_else(|| WorkflowError::Conflict("workflow has no Builder node".to_string()))?;
        let downstream = nodes
            .iter()
            .filter(|node| {
                matches!(
                    node.role,
                    NodeRole::Validator | NodeRole::Reviewer | NodeRole::FinalDelivery
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let cycle = u8::try_from(repair_count + 1)
            .map_err(|_| WorkflowError::Conflict("repair cycle overflow".to_string()))?;
        let journal = self.journal.as_ref().expect("available service").clone();
        let run_id = run.id.clone();
        let feedback = feedback.to_string();
        run_blocking(move || {
            journal.schedule_builder_repair(
                &run_id,
                &builder,
                &downstream,
                cycle,
                &feedback,
                &EventDraft::new(
                    "run.repair_requested",
                    json!({ "cycle": cycle, "feedback": feedback }),
                ),
            )
        })
        .await?;
        Ok(true)
    }

    async fn stale_builder_steer(
        &self,
        run_id: &str,
        nodes: &[NodeRecord],
    ) -> WorkflowResult<Option<(u64, String)>> {
        let Some(builder) = nodes.iter().find(|node| node.role == NodeRole::Builder) else {
            return Ok(None);
        };
        if builder.status != NodeStatus::Succeeded {
            return Ok(None);
        }
        let journal = self.journal.as_ref().expect("available service").clone();
        let lookup_id = run_id.to_string();
        let latest = run_blocking(move || journal.latest_sequence(&lookup_id)).await?;
        let journal = self.journal.as_ref().expect("available service").clone();
        let lookup_id = run_id.to_string();
        let events =
            run_blocking(move || journal.events_after(&lookup_id, latest.saturating_sub(500), 500))
                .await?;
        Ok(find_stale_builder_steer(&events, &builder.id))
    }

    async fn schedule_steer_recompile(
        &self,
        run: &RunRecord,
        nodes: &[NodeRecord],
        steer_sequence: u64,
        feedback: &str,
    ) -> WorkflowResult<bool> {
        let builder = nodes
            .iter()
            .find(|node| node.role == NodeRole::Builder)
            .cloned()
            .ok_or_else(|| WorkflowError::Conflict("workflow has no Builder node".to_string()))?;
        if builder.attempt_count >= builder.max_attempts {
            return Ok(false);
        }
        let downstream = nodes
            .iter()
            .filter(|node| {
                matches!(
                    node.role,
                    NodeRole::Validator | NodeRole::Reviewer | NodeRole::FinalDelivery
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let journal = self.journal.as_ref().expect("available service").clone();
        let run_id = run.id.clone();
        let feedback = feedback.to_string();
        run_blocking(move || {
            journal.schedule_builder_repair(
                &run_id,
                &builder,
                &downstream,
                0,
                &feedback,
                &EventDraft::new(
                    "run.steer_recompiled",
                    json!({ "steerSequence": steer_sequence, "feedback": feedback }),
                ),
            )
        })
        .await?;
        Ok(true)
    }

    async fn execute_node(&self, run_id: &str, node_id: &str) -> WorkflowResult<()> {
        let run = self.service.get(run_id.to_string()).await?.run;
        let configured_lease = self
            .workflow_config
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .lease_seconds;
        let lease_seconds = run
            .input
            .pointer("/runtimePolicy/leaseSeconds")
            .and_then(Value::as_u64)
            .unwrap_or(configured_lease)
            .max(3);
        let lease_ttl = Duration::from_secs(lease_seconds);
        let (started, mut lease) = match self
            .service
            .dispatch_node(
                run_id.to_string(),
                node_id.to_string(),
                LEASE_OWNER,
                lease_ttl,
            )
            .await
        {
            Ok(started) => started,
            Err(error) => {
                self.finish_unknown_or_failed(run_id, node_id, error.to_string())
                    .await?;
                return Ok(());
            }
        };
        let adapter = self.adapter.as_ref().expect("available service").clone();
        let wait_adapter = adapter.clone();
        let wait_thread_id = started.thread_id.clone();
        let wait_turn_id = started.turn_id.clone();
        let mut execution = tokio::spawn(async move {
            wait_adapter
                .wait_for_execution(&wait_thread_id, wait_turn_id.as_deref())
                .await
        });
        let heartbeat_period = lease_ttl / 3;
        let mut heartbeat = tokio::time::interval(heartbeat_period);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        let outcome = loop {
            tokio::select! {
                result = &mut execution => {
                    break result.map_err(|error| WorkflowError::Join(error.to_string()))?;
                }
                _ = heartbeat.tick() => {
                    let journal = self.journal.as_ref().expect("available service").clone();
                    let current_lease = lease.clone();
                    match run_blocking(move || journal.renew_lease(&current_lease, lease_ttl)).await {
                        Ok(renewed) => lease = renewed,
                        Err(error) => {
                            execution.abort();
                            let _ = adapter
                                .interrupt_thread(
                                    run_id,
                                    &started.thread_id,
                                    &format!("lease-lost:{run_id}:{node_id}"),
                                )
                                .await;
                            break Err(WorkflowError::Adapter(format!(
                                "worker lease could not be renewed: {error}"
                            )));
                        }
                    }
                }
            }
        };
        match outcome {
            Ok(ExecutionOutcome::Succeeded(result)) => {
                self.finish_node(run_id, node_id, NodeStatus::Succeeded, Some(result), None)
                    .await
            }
            Ok(ExecutionOutcome::Failed(error)) if error.contains("interrupted") => {
                self.finish_interrupted(run_id, node_id, error).await
            }
            Ok(ExecutionOutcome::Failed(error)) => {
                self.finish_node(run_id, node_id, NodeStatus::Failed, None, Some(error))
                    .await
            }
            Ok(ExecutionOutcome::Unknown(reason)) | Err(WorkflowError::Adapter(reason)) => {
                self.finish_infrastructure_failure(run_id, node_id, reason)
                    .await
            }
            Err(error) => {
                self.finish_infrastructure_failure(run_id, node_id, error.to_string())
                    .await
            }
        }
    }

    async fn finish_interrupted(
        &self,
        run_id: &str,
        node_id: &str,
        error: String,
    ) -> WorkflowResult<()> {
        let details = self.service.get(run_id.to_string()).await?;
        let node = details
            .nodes
            .iter()
            .find(|node| node.id == node_id)
            .ok_or_else(|| WorkflowError::NotFound {
                entity: "node",
                id: format!("{run_id}/{node_id}"),
            })?;
        if node.repo_writer {
            return self
                .finish_node(
                    run_id,
                    node_id,
                    NodeStatus::UnknownOutcome,
                    None,
                    Some(error),
                )
                .await;
        }
        if matches!(
            details.run.status,
            RunStatus::Canceling | RunStatus::Canceled
        ) {
            return self
                .finish_node(run_id, node_id, NodeStatus::Canceled, None, Some(error))
                .await;
        }
        // Read-only interrupted work is safe to replay, subject to the frozen
        // infrastructure retry budget. This leaves a paused run resumable.
        self.finish_infrastructure_failure(run_id, node_id, error)
            .await
    }

    async fn finish_infrastructure_failure(
        &self,
        run_id: &str,
        node_id: &str,
        error: String,
    ) -> WorkflowResult<()> {
        self.finish_unknown_or_failed(run_id, node_id, error.clone())
            .await?;
        let details = self.service.get(run_id.to_string()).await?;
        let Some(node) = details.nodes.iter().find(|node| node.id == node_id) else {
            return Err(WorkflowError::NotFound {
                entity: "node",
                id: format!("{run_id}/{node_id}"),
            });
        };
        if node.repo_writer || node.status != NodeStatus::Failed {
            return Ok(());
        }
        let policy = WorkflowPolicy {
            version: details.run.policy.version,
            max_read_only_concurrency: details.run.policy.max_read_only_concurrency,
            max_provider_concurrency: details.run.policy.max_provider_concurrency,
            max_repo_writers: details.run.policy.max_repo_writers,
            max_delegation_depth: details.run.policy.max_delegation_depth,
            infrastructure_retry_limit: details.run.policy.infrastructure_retry_limit,
            builder_repair_limit: details.run.policy.builder_repair_limit,
            permissions: details.run.policy.permissions.clone(),
        };
        if policy.retry_decision(RetryClass::Infrastructure, node.attempt_count, true)
            != RetryDecision::Retry
        {
            return Ok(());
        }
        let next_hash = payload_hash(&json!({
            "previous": node.payload_hash,
            "reason": "infrastructure",
            "generation": node.generation + 1,
        }))?;
        let journal = self.journal.as_ref().expect("available service").clone();
        let run_id = run_id.to_string();
        let node_id = node_id.to_string();
        let cas = node_cas(node);
        run_blocking(move || {
            journal
                .reset_node_for_retry(
                    &run_id,
                    &node_id,
                    &cas,
                    &next_hash,
                    &EventDraft::new(
                        "node.infrastructure_retry_scheduled",
                        json!({ "nodeId": node_id, "error": error }),
                    ),
                )
                .map(|_| ())
        })
        .await
    }

    async fn finish_unknown_or_failed(
        &self,
        run_id: &str,
        node_id: &str,
        error: String,
    ) -> WorkflowResult<()> {
        let node = self
            .service
            .get(run_id.to_string())
            .await?
            .nodes
            .into_iter()
            .find(|node| node.id == node_id)
            .ok_or_else(|| WorkflowError::NotFound {
                entity: "node",
                id: format!("{run_id}/{node_id}"),
            })?;
        let next = if node.repo_writer {
            NodeStatus::UnknownOutcome
        } else {
            NodeStatus::Failed
        };
        self.finish_node(run_id, node_id, next, None, Some(error))
            .await
    }

    async fn finish_node(
        &self,
        run_id: &str,
        node_id: &str,
        next: NodeStatus,
        result: Option<Value>,
        error: Option<String>,
    ) -> WorkflowResult<()> {
        let details = self.service.get(run_id.to_string()).await?;
        let node = details
            .nodes
            .into_iter()
            .find(|node| node.id == node_id)
            .ok_or_else(|| WorkflowError::NotFound {
                entity: "node",
                id: format!("{run_id}/{node_id}"),
            })?;
        if node.status == next || node.status.is_terminal() {
            return Ok(());
        }
        if next == NodeStatus::Succeeded
            && let Some(result) = result.as_ref()
        {
            self.service
                .store_node_result(run_id, node_id, node.role, result.clone())
                .await?;
        }
        let cas = node_cas(&node);
        let journal = self.journal.as_ref().expect("available service").clone();
        let run_id = run_id.to_string();
        let node_id = node_id.to_string();
        run_blocking(move || {
            journal
                .finish_attempt_and_node(
                    &run_id,
                    &node_id,
                    &cas,
                    next,
                    result.as_ref(),
                    error.as_deref(),
                    &EventDraft::new(
                        "node.completed",
                        json!({ "nodeId": node_id, "status": next, "error": error }),
                    ),
                )
                .map(|_| ())
        })
        .await
    }

    async fn mark_needs_attention(&self, run: &RunRecord, reason: String) -> WorkflowResult<()> {
        let journal = self.journal.as_ref().expect("available service").clone();
        let run_id = run.id.clone();
        let revision = run.revision;
        let generation = run.generation;
        run_blocking(move || {
            let persisted_reason = reason.clone();
            journal
                .transition_run_with_error(
                    &run_id,
                    revision,
                    generation,
                    RunStatus::NeedsAttention,
                    &persisted_reason,
                    &EventDraft::new("run.needs_attention", json!({ "reason": reason })),
                )
                .map(|_| ())
        })
        .await
    }

    pub async fn run_command(
        self: &Arc<Self>,
        action: &str,
        command_id: String,
        run_id: String,
        expected_revision: u64,
    ) -> WorkflowResult<DurableAck> {
        let run_lock = self.run_lock(&run_id).await;
        let guard = run_lock.lock().await;
        let run = self.service.get(&run_id).await?.run;
        if run.revision != expected_revision {
            return Err(WorkflowError::Conflict(format!(
                "run revision changed from {expected_revision} to {}",
                run.revision
            )));
        }
        let request = RunCommandRequest {
            command_id,
            run_id: run_id.clone(),
        };
        let (ack, should_quiesce) = match action {
            "pause" => (self.service.pause(request).await?, true),
            "resume" => (self.service.resume(request).await?, false),
            "cancel" => (self.service.cancel(request).await?, true),
            _ => {
                return Err(WorkflowError::InvalidRequest(format!(
                    "unknown workflow action {action}"
                )));
            }
        };
        drop(guard);
        if should_quiesce {
            self.quiesce_executor(&run_id, Duration::from_secs(10))
                .await?;
        }
        if action == "resume" {
            self.spawn_executor(run_id).await;
        }
        Ok(ack)
    }

    pub async fn retry_node(
        self: &Arc<Self>,
        request: RetryNodeRequest,
        expected_revision: u64,
    ) -> WorkflowResult<DurableAck> {
        let run_id = request.run_id.clone();
        let run_lock = self.run_lock(&run_id).await;
        let guard = run_lock.lock().await;
        let run = self.service.get(&request.run_id).await?.run;
        if run.revision != expected_revision {
            return Err(WorkflowError::Conflict("run revision changed".to_string()));
        }
        let ack = self.service.retry(request).await?;
        let refreshed = self.service.get(&run_id).await?.run;
        drop(guard);
        if matches!(refreshed.status, RunStatus::Queued | RunStatus::Running) {
            self.spawn_executor(run_id).await;
        }
        Ok(ack)
    }

    pub async fn reply_approval(
        &self,
        request: ApprovalReplyRequest,
        expected_revision: u64,
    ) -> WorkflowResult<DurableAck> {
        let run = self.service.get(&request.run_id).await?.run;
        if run.revision != expected_revision {
            return Err(WorkflowError::Conflict("run revision changed".to_string()));
        }
        self.service.reply(request).await
    }

    pub async fn list_runs(&self, limit: usize) -> WorkflowResult<Vec<RunRecord>> {
        self.service
            .list(ListRunsRequest {
                status: None,
                after_updated_at_ms: None,
                limit,
            })
            .await
    }

    pub async fn list_runs_with_stats(
        &self,
        limit: usize,
    ) -> WorkflowResult<Vec<(RunRecord, RunSummaryStats)>> {
        let runs = self.list_runs(limit).await?;
        self.attach_run_stats(runs).await
    }

    pub async fn list_runs_for_origin_with_stats(
        &self,
        origin_thread_id: &str,
        limit: usize,
    ) -> WorkflowResult<Vec<(RunRecord, RunSummaryStats)>> {
        let journal = self
            .journal
            .as_ref()
            .ok_or_else(|| WorkflowError::Unavailable("workflow journal unavailable".to_string()))?
            .clone();
        let origin_thread_id = origin_thread_id.to_string();
        let runs =
            run_blocking(move || journal.list_runs_for_origin(&origin_thread_id, None, limit))
                .await?;
        self.attach_run_stats(runs).await
    }

    async fn attach_run_stats(
        &self,
        runs: Vec<RunRecord>,
    ) -> WorkflowResult<Vec<(RunRecord, RunSummaryStats)>> {
        let run_ids = runs.iter().map(|run| run.id.clone()).collect::<Vec<_>>();
        let journal = self
            .journal
            .as_ref()
            .ok_or_else(|| WorkflowError::Unavailable("workflow journal unavailable".to_string()))?
            .clone();
        let mut stats = run_blocking(move || journal.run_summary_stats(&run_ids)).await?;
        Ok(runs
            .into_iter()
            .map(|run| {
                let summary = stats.remove(&run.id).unwrap_or_default();
                (run, summary)
            })
            .collect())
    }
}

#[async_trait]
impl ShutdownHooks for WorkflowHost {
    async fn pause_admission(&self) -> WorkflowResult<()> {
        self.accepting.store(false, Ordering::Release);
        Ok(())
    }

    async fn interrupt(&self, node: &NodeRecord, idempotency_key: &str) -> WorkflowResult<()> {
        let Some(thread_id) = node.thread_id.as_deref() else {
            return Ok(());
        };
        self.adapter
            .as_ref()
            .ok_or_else(|| {
                WorkflowError::Unavailable("app-server adapter unavailable".to_string())
            })?
            .interrupt_thread(&node.run_id, thread_id, idempotency_key)
            .await
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

fn permissions_from_snapshot(snapshot: &FrozenPermissionSnapshot) -> PermissionSet {
    let mut read_paths = BTreeSet::new();
    read_paths.insert(snapshot.cwd.clone());
    let mut write_paths = BTreeSet::new();
    if snapshot.public.sandbox_mode != "read-only" {
        write_paths.insert(snapshot.cwd.clone());
    }
    PermissionSet {
        read_paths,
        write_paths,
        allowed_commands: BTreeSet::new(),
        network_hosts: BTreeSet::new(),
        can_request_approval: snapshot.public.approval_policy != "never",
    }
}

fn workflow_policy(config: &WorkflowConfig) -> WorkflowPolicy {
    WorkflowPolicy {
        max_read_only_concurrency: config.max_read_only_concurrency,
        max_provider_concurrency: config.max_provider_concurrency,
        max_repo_writers: config.max_repo_writers,
        max_delegation_depth: config.max_delegation_depth,
        infrastructure_retry_limit: config.infrastructure_retry_limit,
        builder_repair_limit: config.builder_repair_limit,
        ..WorkflowPolicy::default()
    }
}

fn approval_policy_label(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "granular".to_string())
}

fn sandbox_mode_label(value: &Value) -> WorkflowResult<String> {
    if let Some(value) = value.as_str() {
        return match value {
            "read-only" | "readOnly" => Ok("read-only".to_string()),
            "workspace-write" | "workspaceWrite" => Ok("workspace-write".to_string()),
            "danger-full-access" | "dangerFullAccess" => Ok("danger-full-access".to_string()),
            other => Err(WorkflowError::InvalidRequest(format!(
                "unknown sandbox mode {other}"
            ))),
        };
    }
    let Some(object) = value.as_object() else {
        return Err(WorkflowError::InvalidRequest(
            "sandbox policy has an unsupported shape".to_string(),
        ));
    };
    if object.get("type").and_then(Value::as_str) == Some("dangerFullAccess")
        || object.contains_key("dangerFullAccess")
        || object.contains_key("danger-full-access")
    {
        Ok("danger-full-access".to_string())
    } else if object.get("type").and_then(Value::as_str) == Some("workspaceWrite")
        || object.contains_key("workspaceWrite")
        || object.contains_key("workspace-write")
    {
        Ok("workspace-write".to_string())
    } else if object.get("type").and_then(Value::as_str) == Some("readOnly")
        || object.contains_key("readOnly")
        || object.contains_key("read-only")
    {
        Ok("read-only".to_string())
    } else {
        Err(WorkflowError::InvalidRequest(
            "sandbox policy cannot be proven".to_string(),
        ))
    }
}

fn role_models_for_run(roles: &BTreeMap<String, SubagentRoleConfig>) -> Value {
    let mut values = serde_json::Map::new();
    for (role, selection) in roles {
        values.insert(
            role.clone(),
            json!({
                "model": selection.model,
                "reasoningEffort": selection.reasoning_effort,
            }),
        );
    }
    if let Some(preflight) = values.get("preflight").cloned() {
        values.insert("planner".to_string(), preflight);
    }
    if let Some(scout) = values.get("scout").cloned() {
        values.insert("researcher".to_string(), scout);
    }
    if let Some(expert) = values.get("expert").cloned() {
        values.insert("integrator".to_string(), expert);
    }
    Value::Object(values)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestIntent {
    ReadOnly,
    Write,
    Ambiguous,
}

impl RequestIntent {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Write => "write",
            Self::Ambiguous => "ambiguous",
        }
    }
}

fn permissions_for_request(
    mut upper_bound: PermissionSet,
    text: &str,
) -> (PermissionSet, RequestIntent) {
    let intent = classify_request_intent(text);
    // Only an explicitly read-only request narrows the frozen Codex upper
    // bound. Unknown natural-language imperatives stay Guarded instead of
    // being irreversibly downgraded by a missing keyword.
    if intent == RequestIntent::ReadOnly {
        upper_bound.write_paths.clear();
    }
    (upper_bound, intent)
}

fn classify_route(
    text: &str,
    permissions: &PermissionSet,
    request_intent: RequestIntent,
) -> RouteMode {
    let normalized = text.to_ascii_lowercase();
    let high_risk = [
        "migration",
        "schema",
        "delete",
        "remove",
        "security",
        "permission",
        "auth",
        "支付",
        "迁移",
        "删除",
        "权限",
        "安全",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    if high_risk && !permissions.is_read_only() {
        return RouteMode::Expert;
    }
    let parallel = [
        "research",
        "compare",
        "investigate",
        "调研",
        "比较",
        "多方案",
        "多个模块",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    if parallel {
        return RouteMode::Parallel;
    }
    if request_intent != RequestIntent::Write
        && permissions.is_read_only()
        && normalized.len() <= 500
        && !normalized.contains("review")
        && !normalized.contains("审查")
    {
        return RouteMode::Direct;
    }
    RouteMode::Guarded
}

fn classify_request_intent(text: &str) -> RequestIntent {
    let normalized = text.trim().to_ascii_lowercase();
    let write_requested = [
        "implement",
        "build",
        "create",
        "add ",
        "update",
        "edit",
        "modify",
        "fix",
        "refactor",
        "migrate",
        "delete",
        "remove",
        "write ",
        "resolve",
        "address",
        "apply the",
        "开发",
        "实现",
        "创建",
        "新增",
        "添加",
        "修改",
        "更新",
        "修复",
        "重构",
        "迁移",
        "删除",
        "解决",
        "处理",
        "完善",
        "补齐",
        "落实",
        "优化",
        "调整",
        "替换",
        "升级",
        "修正",
        "整改",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    let read_requested = [
        "why ",
        "what is",
        "how to",
        "explain",
        "review",
        "inspect",
        "research",
        "investigate",
        "analyze",
        "analyse",
        "summarize",
        "compare",
        "能否",
        "能不能",
        "是否",
        "可不可以",
        "为什么",
        "是什么",
        "如何",
        "怎么",
        "怎样",
        "哪些",
        "解释",
        "分析",
        "审查",
        "检查",
        "查看",
        "看看",
        "调研",
        "总结",
        "对比",
        "比较",
        "评估",
        "梳理",
        "方案",
        "建议",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    let explicit_write_directive = [
        "please fix",
        "please implement",
        "and fix",
        "then fix",
        "go ahead",
        "请修改",
        "请修复",
        "请解决",
        "帮我修改",
        "帮我修复",
        "帮我解决",
        "直接修改",
        "直接修复",
        "直接解决",
        "并修改",
        "并修复",
        "并解决",
        "然后修改",
        "然后修复",
        "后修改",
        "后修复",
        "解决下",
        "处理下",
        "改一下",
        "修一下",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    if read_requested && !explicit_write_directive {
        return RequestIntent::ReadOnly;
    }
    if write_requested {
        return RequestIntent::Write;
    }
    if read_requested {
        RequestIntent::ReadOnly
    } else {
        RequestIntent::Ambiguous
    }
}

async fn workspace_risk(cwd: &str) -> WorkspaceRisk {
    let cwd = cwd.to_string();
    tokio::task::spawn_blocking(move || {
        let output = Command::new("git")
            .args(["status", "--porcelain=v1", "--untracked-files=normal"])
            .current_dir(&cwd)
            .output();
        match output {
            Ok(output) if output.status.success() => {
                let dirty_paths = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter_map(|line| line.get(3..).map(str::to_string))
                    .take(200)
                    .collect::<Vec<_>>();
                if dirty_paths.is_empty() {
                    WorkspaceRisk::default()
                } else {
                    WorkspaceRisk {
                        level: WorkspaceRiskLevel::Dirty,
                        dirty_paths,
                        reason: Some("working tree contains user changes".to_string()),
                    }
                }
            }
            Ok(output) => WorkspaceRisk {
                level: WorkspaceRiskLevel::HighRisk,
                dirty_paths: Vec::new(),
                reason: Some(format!(
                    "git workspace inspection failed with status {}",
                    output.status
                )),
            },
            Err(_) => WorkspaceRisk {
                level: WorkspaceRiskLevel::HighRisk,
                dirty_paths: Vec::new(),
                reason: Some("git workspace inspection could not start".to_string()),
            },
        }
    })
    .await
    .unwrap_or_else(|_| WorkspaceRisk {
        level: WorkspaceRiskLevel::HighRisk,
        dirty_paths: Vec::new(),
        reason: Some("workspace risk inspection failed".to_string()),
    })
}

enum FinalDecision {
    Deliver(Value),
    Repair(String),
    Inconclusive(String),
}

fn workflow_message_text(value: &Value) -> String {
    value
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| value.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn find_stale_builder_steer(events: &[WorkflowEvent], builder_id: &str) -> Option<(u64, String)> {
    let builder_started = events
        .iter()
        .filter(|event| event.kind == "node.started")
        .filter(|event| event.payload.get("nodeId").and_then(Value::as_str) == Some(builder_id))
        .map(|event| event.workflow_sequence)
        .max()
        .unwrap_or(0);
    let forwarded = events
        .iter()
        .filter(|event| event.kind == "run.steer_forwarded")
        .filter(|event| event.payload.get("nodeId").and_then(Value::as_str) == Some(builder_id))
        .filter_map(|event| {
            event
                .payload
                .get("commandId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect::<BTreeSet<_>>();
    events
        .iter()
        .rev()
        .find(|event| {
            event.kind == "run.steered"
                && event.workflow_sequence > builder_started
                && event
                    .payload
                    .get("commandId")
                    .and_then(Value::as_str)
                    .is_none_or(|command_id| !forwarded.contains(command_id))
        })
        .map(|event| {
            (
                event.workflow_sequence,
                event
                    .payload
                    .get("message")
                    .map(workflow_message_text)
                    .unwrap_or_else(|| "follow-up input".to_string()),
            )
        })
}

fn final_decision(nodes: &[NodeRecord], final_node: &NodeRecord) -> WorkflowResult<FinalDecision> {
    let dependencies = final_node
        .dependencies
        .iter()
        .map(|dependency| {
            nodes
                .iter()
                .find(|node| &node.id == dependency)
                .ok_or_else(|| {
                    WorkflowError::Conflict(format!(
                        "final delivery dependency {dependency} is missing"
                    ))
                })
        })
        .collect::<WorkflowResult<Vec<_>>>()?;
    let reviewers = dependencies
        .iter()
        .copied()
        .filter(|node| node.role == NodeRole::Reviewer)
        .collect::<Vec<_>>();
    if reviewers.is_empty() {
        let Some(result) = dependencies.first().and_then(|node| node.result.clone()) else {
            return Ok(FinalDecision::Inconclusive(
                "direct delivery dependency did not produce a result".to_string(),
            ));
        };
        if result.is_null() || workflow_message_text(&result).trim().is_empty() {
            return Ok(FinalDecision::Inconclusive(
                "direct delivery dependency produced an empty result".to_string(),
            ));
        }
        return Ok(FinalDecision::Deliver(result));
    }

    let mut deliveries = Vec::new();
    let mut requested_changes = Vec::new();
    let mut inconclusive = Vec::new();
    for reviewer in reviewers {
        let Some(result) = reviewer.result.as_ref() else {
            inconclusive.push(format!("{}: reviewer produced no result", reviewer.id));
            continue;
        };
        match parse_reviewer_verdict(result) {
            ReviewerVerdict::Pass { delivery } => deliveries.push(delivery),
            ReviewerVerdict::ChangesRequired { feedback } => {
                requested_changes.push(format!("{}: {feedback}", reviewer.id));
            }
            ReviewerVerdict::Inconclusive { feedback } => {
                inconclusive.push(format!("{}: {feedback}", reviewer.id));
            }
        }
    }
    if !requested_changes.is_empty() {
        if !inconclusive.is_empty() {
            requested_changes.extend(inconclusive);
        }
        return Ok(FinalDecision::Repair(requested_changes.join("\n\n")));
    }
    if !inconclusive.is_empty() || deliveries.len() != final_node.dependencies.len() {
        return Ok(FinalDecision::Inconclusive(format!(
            "independent review was inconclusive: {}",
            if inconclusive.is_empty() {
                "not every required reviewer explicitly passed".to_string()
            } else {
                inconclusive.join("; ")
            }
        )));
    }
    Ok(FinalDecision::Deliver(json!({
        "text": deliveries.remove(0),
        "reviewerCount": deliveries.len() + 1,
    })))
}

fn compact_title(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut title = compact.chars().take(60).collect::<String>();
    if compact.chars().count() > 60 {
        title.push('…');
    }
    title
}

fn is_absolute_path(value: &str) -> bool {
    PathBuf::from(value).is_absolute()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sequence: u64, kind: &str, payload: Value) -> WorkflowEvent {
        WorkflowEvent {
            event_id: format!("event-{sequence}"),
            run_id: "run".to_string(),
            workflow_sequence: sequence,
            kind: kind.to_string(),
            payload,
            created_at_ms: 0,
        }
    }

    #[test]
    fn sandbox_response_shapes_are_proven_before_admission() {
        assert_eq!(sandbox_mode_label(&json!("readOnly")).unwrap(), "read-only");
        assert_eq!(
            sandbox_mode_label(&json!({"type": "workspaceWrite"})).unwrap(),
            "workspace-write"
        );
        assert_eq!(
            sandbox_mode_label(&json!({"dangerFullAccess": {}})).unwrap(),
            "danger-full-access"
        );
        assert!(sandbox_mode_label(&json!({"type": "mystery"})).is_err());
    }

    #[test]
    fn natural_fix_requests_keep_the_existing_write_upper_bound() {
        let upper_bound = PermissionSet {
            read_paths: ["repo"].into_iter().map(String::from).collect(),
            write_paths: ["repo"].into_iter().map(String::from).collect(),
            ..PermissionSet::default()
        };

        let (permissions, intent) = permissions_for_request(upper_bound.clone(), "解决下这些问题");
        assert_eq!(intent, RequestIntent::Write);
        assert_eq!(permissions.write_paths, upper_bound.write_paths);
        assert_eq!(
            classify_route("解决下这些问题", &permissions, intent),
            RouteMode::Guarded
        );

        let (ambiguous, intent) = permissions_for_request(upper_bound, "把这个收个尾");
        assert_eq!(intent, RequestIntent::Ambiguous);
        assert!(!ambiguous.is_read_only());
    }

    #[test]
    fn explanation_and_feasibility_requests_remain_read_only() {
        let upper_bound = PermissionSet {
            read_paths: ["repo"].into_iter().map(String::from).collect(),
            write_paths: ["repo"].into_iter().map(String::from).collect(),
            ..PermissionSet::default()
        };

        for request in [
            "为什么会有这个限制",
            "看看能不能解决这个问题",
            "how to fix this",
            "解释一下删除策略",
            "审查现有修复结果",
        ] {
            let (permissions, intent) = permissions_for_request(upper_bound.clone(), request);
            assert_eq!(intent, RequestIntent::ReadOnly, "{request}");
            assert!(permissions.is_read_only(), "{request}");
        }

        for request in ["请分析并修复这个问题", "检查后修复全部回归"] {
            let (permissions, intent) = permissions_for_request(upper_bound.clone(), request);
            assert_eq!(intent, RequestIntent::Write, "{request}");
            assert!(!permissions.is_read_only(), "{request}");
        }
    }

    #[test]
    fn follow_up_input_must_reach_or_postdate_the_builder() {
        let mut events = vec![
            event(2, "node.started", json!({"nodeId": "builder"})),
            event(
                3,
                "run.steered",
                json!({"commandId": "steer-1", "message": {"text": "clarify"}}),
            ),
            event(
                4,
                "run.steer_forwarded",
                json!({"commandId": "steer-1", "nodeId": "builder"}),
            ),
        ];
        assert!(find_stale_builder_steer(&events, "builder").is_none());

        events.push(event(
            5,
            "run.steered",
            json!({"commandId": "steer-2", "message": {"text": "new requirement"}}),
        ));
        assert_eq!(
            find_stale_builder_steer(&events, "builder"),
            Some((5, "new requirement".to_string()))
        );

        events.push(event(6, "node.started", json!({"nodeId": "builder"})));
        assert!(find_stale_builder_steer(&events, "builder").is_none());
    }
}
