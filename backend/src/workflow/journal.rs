use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use rusqlite::{
    Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params, params_from_iter,
    types::Type,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::domain::{
    ApprovalRecord, ApprovalStatus, ArtifactMetadata, ArtifactPage, AttemptRecord, AttemptStatus,
    CommandEnvelope, DurableAck, Lease, NodeCas, NodeRecord, NodeRole, NodeStatus, RunRecord,
    RunStatus, StoredCommand, WorkflowError, WorkflowEvent, WorkflowResult,
};
use super::scheduler::{NodeSpec, WorkflowPlan};

const SCHEMA_VERSION: i64 = 1;

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDraft {
    pub event_id: String,
    pub kind: String,
    pub payload: Value,
    pub topic: String,
}

impl EventDraft {
    pub fn new(kind: impl Into<String>, payload: Value) -> Self {
        Self {
            event_id: uuid::Uuid::new_v4().to_string(),
            kind: kind.into(),
            payload,
            topic: "workflow.event".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboxRecord {
    pub id: i64,
    pub event_id: String,
    pub topic: String,
    pub payload: Value,
    pub attempts: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSummaryStats {
    pub latest_sequence: u64,
    pub total_nodes: usize,
    pub completed_nodes: usize,
    pub failed_nodes: usize,
    pub running_nodes: usize,
    pub ready_non_final_nodes: usize,
    pub attention_blockers: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome<T> {
    Applied(T),
    Duplicate(T),
}

impl<T> CommandOutcome<T> {
    pub fn into_inner(self) -> T {
        match self {
            Self::Applied(value) | Self::Duplicate(value) => value,
        }
    }

    pub fn is_duplicate(&self) -> bool {
        matches!(self, Self::Duplicate(_))
    }
}

/// Handle to an independent workflow journal database.
///
/// A `Journal` stores only its path. Each operation opens and closes its own
/// configured connection, which also makes it safe to clone into
/// `spawn_blocking` without sharing a connection between threads.
#[derive(Debug, Clone)]
pub struct Journal {
    path: Arc<PathBuf>,
}

impl Journal {
    pub fn open(path: impl AsRef<Path>) -> WorkflowResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let journal = Self {
            path: Arc::new(path),
        };
        let mut connection = journal.connect()?;
        migrate(&mut connection)?;
        Ok(journal)
    }

    pub fn schema_version(&self) -> WorkflowResult<i64> {
        let connection = self.connect()?;
        Ok(connection.query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    fn connect(&self) -> WorkflowResult<Connection> {
        let connection = Connection::open(self.path.as_ref())?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA synchronous = FULL;
             PRAGMA busy_timeout = 5000;",
        )?;
        Ok(connection)
    }

    pub fn create_run(
        &self,
        run: &RunRecord,
        plan: &WorkflowPlan,
        command: &CommandEnvelope,
        event: &EventDraft,
    ) -> WorkflowResult<CommandOutcome<DurableAck>> {
        if run.id != command.run_id {
            return Err(WorkflowError::InvalidRequest(
                "command run id does not match the new run".to_string(),
            ));
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(stored) = command_in_tx(&transaction, &command.command_id)? {
            validate_command_replay(&stored, command)?;
            let ack = serde_json::from_str(&stored.response_json)?;
            transaction.commit()?;
            return Ok(CommandOutcome::Duplicate(ack));
        }

        transaction.execute(
            "INSERT INTO runs (
                id, status, route, revision, generation, input_json, policy_json,
                permissions_json, workspace_risk, final_delivery_committed,
                created_at_ms, updated_at_ms, error
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                run.id,
                run.status.as_str(),
                run.route.as_str(),
                to_i64(run.revision, "run revision")?,
                to_i64(run.generation, "run generation")?,
                serde_json::to_string(&run.input)?,
                serde_json::to_string(&run.policy)?,
                serde_json::to_string(&run.policy.permissions)?,
                run.policy.workspace_risk.level.as_str(),
                run.final_delivery_committed,
                run.created_at_ms,
                run.updated_at_ms,
                run.error,
            ],
        )?;
        for node in &plan.nodes {
            insert_node(&transaction, run, node)?;
        }
        let sequence = insert_event_and_outbox(&transaction, &run.id, event)?;
        let ack = DurableAck {
            command_id: command.command_id.clone(),
            run_id: run.id.clone(),
            workflow_sequence: sequence,
            duplicate: false,
        };
        insert_command(&transaction, command, &ack)?;
        transaction.commit()?;
        Ok(CommandOutcome::Applied(ack))
    }

    pub fn get_run(&self, run_id: &str) -> WorkflowResult<RunRecord> {
        let connection = self.connect()?;
        connection
            .query_row(
                "SELECT id, status, route, revision, generation, input_json, policy_json,
                        final_delivery_committed, created_at_ms, updated_at_ms, error
                 FROM runs WHERE id=?1",
                [run_id],
                row_to_run,
            )
            .optional()?
            .ok_or_else(|| WorkflowError::NotFound {
                entity: "run",
                id: run_id.to_string(),
            })
    }

    pub fn list_runs(
        &self,
        status: Option<RunStatus>,
        after_updated_at_ms: Option<i64>,
        limit: usize,
    ) -> WorkflowResult<Vec<RunRecord>> {
        let connection = self.connect()?;
        let limit = clamp_limit(limit, 200) as i64;
        let mut statement = connection.prepare(
            "SELECT id, status, route, revision, generation, input_json, policy_json,
                    final_delivery_committed, created_at_ms, updated_at_ms, error
             FROM runs
             WHERE (?1 IS NULL OR status=?1)
               AND (?2 IS NULL OR updated_at_ms < ?2)
             ORDER BY updated_at_ms DESC, id DESC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![status.map(RunStatus::as_str), after_updated_at_ms, limit],
            row_to_run,
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn list_runs_for_origin(
        &self,
        origin_thread_id: &str,
        after_updated_at_ms: Option<i64>,
        limit: usize,
    ) -> WorkflowResult<Vec<RunRecord>> {
        let connection = self.connect()?;
        let limit = clamp_limit(limit, 200) as i64;
        let mut statement = connection.prepare(
            "SELECT id, status, route, revision, generation, input_json, policy_json,
                    final_delivery_committed, created_at_ms, updated_at_ms, error
             FROM runs
             WHERE json_extract(input_json, '$.originThreadId')=?1
               AND (?2 IS NULL OR updated_at_ms < ?2)
             ORDER BY updated_at_ms DESC, id DESC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![origin_thread_id, after_updated_at_ms, limit],
            row_to_run,
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn run_summary_stats(
        &self,
        run_ids: &[String],
    ) -> WorkflowResult<HashMap<String, RunSummaryStats>> {
        if run_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = (0..run_ids.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let query = format!(
            "SELECT r.id,
                    COALESCE((
                      SELECT MAX(e.workflow_sequence) FROM events e WHERE e.run_id=r.id
                    ), 0),
                    COUNT(n.id),
                    COALESCE(SUM(CASE WHEN n.status IN (
                      'skipped', 'compensated', 'succeeded', 'failed', 'canceled'
                    ) THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN n.status IN (
                      'failed', 'unknown_outcome'
                    ) THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN n.status IN (
                      'running', 'leased'
                    ) THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN n.status='ready' AND n.role<>'final_delivery'
                      THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN n.status IN (
                      'failed', 'unknown_outcome', 'waiting_approval', 'running', 'leased',
                      'canceled'
                    ) THEN 1 ELSE 0 END), 0)
             FROM runs r
             LEFT JOIN nodes n ON n.run_id=r.id
             WHERE r.id IN ({placeholders})
             GROUP BY r.id"
        );
        let connection = self.connect()?;
        let mut statement = connection.prepare(&query)?;
        let rows = statement.query_map(params_from_iter(run_ids.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                RunSummaryStats {
                    latest_sequence: u64_column(row, 1)?,
                    total_nodes: usize_column(row, 2)?,
                    completed_nodes: usize_column(row, 3)?,
                    failed_nodes: usize_column(row, 4)?,
                    running_nodes: usize_column(row, 5)?,
                    ready_non_final_nodes: usize_column(row, 6)?,
                    attention_blockers: usize_column(row, 7)?,
                },
            ))
        })?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect())
    }

    pub fn unfinished_runs(&self) -> WorkflowResult<Vec<RunRecord>> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT id, status, route, revision, generation, input_json, policy_json,
                    final_delivery_committed, created_at_ms, updated_at_ms, error
             FROM runs WHERE status NOT IN ('canceled', 'succeeded', 'failed')
             ORDER BY created_at_ms, id",
        )?;
        Ok(statement
            .query_map([], row_to_run)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn nodes(&self, run_id: &str) -> WorkflowResult<Vec<NodeRecord>> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT run_id, id, role, label, status, revision, generation, lease_epoch,
                    lease_owner, lease_expires_at_ms, payload_hash, dependencies_json,
                    permissions_json, repo_writer, provider, depth, attempt_count,
                    max_attempts, result_json, thread_id, updated_at_ms
             FROM nodes WHERE run_id=?1 ORDER BY rowid",
        )?;
        Ok(statement
            .query_map([run_id], row_to_node)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn attempts(&self, run_id: &str) -> WorkflowResult<Vec<AttemptRecord>> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT id, run_id, node_id, number, generation, lease_epoch, status,
                    is_write, started_at_ms, completed_at_ms, error
             FROM attempts WHERE run_id=?1 ORDER BY started_at_ms, id",
        )?;
        Ok(statement
            .query_map([run_id], row_to_attempt)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn approvals(&self, run_id: &str) -> WorkflowResult<Vec<ApprovalRecord>> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT id, run_id, node_id, status, prompt, response_json,
                    requested_at_ms, resolved_at_ms
             FROM approvals WHERE run_id=?1 ORDER BY requested_at_ms, id",
        )?;
        Ok(statement
            .query_map([run_id], row_to_approval)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn latest_sequence(&self, run_id: &str) -> WorkflowResult<u64> {
        let connection = self.connect()?;
        let sequence: i64 = connection.query_row(
            "SELECT COALESCE(MAX(workflow_sequence), 0) FROM events WHERE run_id=?1",
            [run_id],
            |row| row.get(0),
        )?;
        u64::try_from(sequence)
            .map_err(|_| WorkflowError::Storage("workflow sequence is negative".to_string()))
    }

    /// Promotes dependency-complete nodes in one transaction. This is safe to
    /// call repeatedly after recovery or duplicate completion notifications.
    pub fn promote_ready_nodes(&self, run_id: &str) -> WorkflowResult<Vec<String>> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_run(&transaction, run_id)?;
        let nodes = {
            let mut statement = transaction.prepare(
                "SELECT id, status, dependencies_json FROM nodes WHERE run_id=?1 ORDER BY id",
            )?;
            statement
                .query_map([run_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        enum_column::<NodeStatus>(row, 1)?,
                        json_column::<Vec<String>>(row, 2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let statuses: std::collections::HashMap<&str, NodeStatus> = nodes
            .iter()
            .map(|(id, status, _)| (id.as_str(), *status))
            .collect();
        let ready = nodes
            .iter()
            .filter(|(_, status, _)| *status == NodeStatus::Pending)
            .filter(|(_, _, dependencies)| {
                dependencies.iter().all(|dependency| {
                    matches!(
                        statuses.get(dependency.as_str()),
                        Some(NodeStatus::Succeeded | NodeStatus::Skipped | NodeStatus::Compensated)
                    )
                })
            })
            .map(|(id, _, _)| id.clone())
            .collect::<Vec<_>>();
        for node_id in &ready {
            let changed = transaction.execute(
                "UPDATE nodes SET status='ready', revision=revision+1, updated_at_ms=?1
                 WHERE run_id=?2 AND id=?3 AND status='pending'",
                params![now_ms(), run_id, node_id],
            )?;
            ensure_changed(changed, "node changed while promoting dependencies")?;
            insert_event_and_outbox(
                &transaction,
                run_id,
                &EventDraft::new("node.ready", serde_json::json!({ "nodeId": node_id })),
            )?;
        }
        transaction.commit()?;
        Ok(ready)
    }

    pub fn get_node(&self, run_id: &str, node_id: &str) -> WorkflowResult<NodeRecord> {
        let connection = self.connect()?;
        connection
            .query_row(
                "SELECT run_id, id, role, label, status, revision, generation, lease_epoch,
                        lease_owner, lease_expires_at_ms, payload_hash, dependencies_json,
                        permissions_json, repo_writer, provider, depth, attempt_count,
                        max_attempts, result_json, thread_id, updated_at_ms
                 FROM nodes WHERE run_id=?1 AND id=?2",
                params![run_id, node_id],
                row_to_node,
            )
            .optional()?
            .ok_or_else(|| WorkflowError::NotFound {
                entity: "node",
                id: format!("{run_id}/{node_id}"),
            })
    }

    pub fn transition_run(
        &self,
        run_id: &str,
        expected_revision: u64,
        expected_generation: u64,
        next: RunStatus,
        event: &EventDraft,
    ) -> WorkflowResult<WorkflowEvent> {
        self.transition_run_inner(
            run_id,
            expected_revision,
            expected_generation,
            next,
            None,
            event,
        )
    }

    pub fn transition_run_with_error(
        &self,
        run_id: &str,
        expected_revision: u64,
        expected_generation: u64,
        next: RunStatus,
        error: &str,
        event: &EventDraft,
    ) -> WorkflowResult<WorkflowEvent> {
        if error.trim().is_empty() {
            return Err(WorkflowError::InvalidRequest(
                "run error cannot be empty".to_string(),
            ));
        }
        self.transition_run_inner(
            run_id,
            expected_revision,
            expected_generation,
            next,
            Some(error),
            event,
        )
    }

    fn transition_run_inner(
        &self,
        run_id: &str,
        expected_revision: u64,
        expected_generation: u64,
        next: RunStatus,
        error: Option<&str>,
        event: &EventDraft,
    ) -> WorkflowResult<WorkflowEvent> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current, revision, generation, final_committed) = run_cas_state(&transaction, run_id)?;
        validate_run_cas(
            current,
            revision,
            generation,
            final_committed,
            expected_revision,
            expected_generation,
            next,
        )?;
        let now = now_ms();
        let changed = transaction.execute(
            "UPDATE runs
             SET status=?1, revision=revision+1, updated_at_ms=?2,
                 error=CASE
                   WHEN ?3 IS NOT NULL THEN ?3
                   WHEN ?1 IN ('queued', 'running', 'succeeded', 'canceled') THEN NULL
                   ELSE error
                 END
             WHERE id=?4 AND revision=?5 AND generation=?6",
            params![
                next.as_str(),
                now,
                error,
                run_id,
                to_i64(expected_revision, "run revision")?,
                to_i64(expected_generation, "run generation")?,
            ],
        )?;
        ensure_changed(changed, "run changed during transition")?;
        let sequence = insert_event_and_outbox(&transaction, run_id, event)?;
        transaction.commit()?;
        Ok(event.materialize(run_id, sequence, now))
    }

    pub fn transition_run_command(
        &self,
        command: &CommandEnvelope,
        expected_revision: u64,
        expected_generation: u64,
        next: RunStatus,
        event: &EventDraft,
    ) -> WorkflowResult<CommandOutcome<DurableAck>> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(stored) = command_in_tx(&transaction, &command.command_id)? {
            validate_command_replay(&stored, command)?;
            let mut ack: DurableAck = serde_json::from_str(&stored.response_json)?;
            ack.duplicate = true;
            transaction.commit()?;
            return Ok(CommandOutcome::Duplicate(ack));
        }
        let (current, revision, generation, final_committed) =
            run_cas_state(&transaction, &command.run_id)?;
        validate_run_cas(
            current,
            revision,
            generation,
            final_committed,
            expected_revision,
            expected_generation,
            next,
        )?;
        let changed = transaction.execute(
            "UPDATE runs
             SET status=?1, revision=revision+1, updated_at_ms=?2,
                 error=CASE
                   WHEN ?1 IN ('queued', 'running', 'succeeded', 'canceled') THEN NULL
                   ELSE error
                 END
             WHERE id=?3 AND revision=?4 AND generation=?5",
            params![
                next.as_str(),
                now_ms(),
                command.run_id,
                to_i64(expected_revision, "run revision")?,
                to_i64(expected_generation, "run generation")?,
            ],
        )?;
        ensure_changed(changed, "run changed during command")?;
        let sequence = insert_event_and_outbox(&transaction, &command.run_id, event)?;
        let ack = DurableAck {
            command_id: command.command_id.clone(),
            run_id: command.run_id.clone(),
            workflow_sequence: sequence,
            duplicate: false,
        };
        insert_command(&transaction, command, &ack)?;
        transaction.commit()?;
        Ok(CommandOutcome::Applied(ack))
    }

    pub fn append_command_event(
        &self,
        command: &CommandEnvelope,
        event: &EventDraft,
    ) -> WorkflowResult<CommandOutcome<DurableAck>> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(stored) = command_in_tx(&transaction, &command.command_id)? {
            validate_command_replay(&stored, command)?;
            let mut ack: DurableAck = serde_json::from_str(&stored.response_json)?;
            ack.duplicate = true;
            transaction.commit()?;
            return Ok(CommandOutcome::Duplicate(ack));
        }
        require_run(&transaction, &command.run_id)?;
        let sequence = insert_event_and_outbox(&transaction, &command.run_id, event)?;
        transaction.execute(
            "UPDATE runs SET revision=revision+1, updated_at_ms=?1 WHERE id=?2",
            params![now_ms(), command.run_id],
        )?;
        let ack = DurableAck {
            command_id: command.command_id.clone(),
            run_id: command.run_id.clone(),
            workflow_sequence: sequence,
            duplicate: false,
        };
        insert_command(&transaction, command, &ack)?;
        transaction.commit()?;
        Ok(CommandOutcome::Applied(ack))
    }

    pub fn append_event(&self, run_id: &str, event: &EventDraft) -> WorkflowResult<WorkflowEvent> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_run(&transaction, run_id)?;
        if let Some(existing) = event_by_id_in_tx(&transaction, &event.event_id)? {
            validate_event_replay(&existing, run_id, event)?;
            transaction.commit()?;
            return Ok(existing);
        }
        let created_at = now_ms();
        let sequence = insert_event_and_outbox_at(&transaction, run_id, event, created_at)?;
        transaction.commit()?;
        Ok(event.materialize(run_id, sequence, created_at))
    }

    pub fn transition_node(
        &self,
        run_id: &str,
        node_id: &str,
        cas: &NodeCas,
        next: NodeStatus,
        result: Option<&Value>,
        event: &EventDraft,
    ) -> WorkflowResult<WorkflowEvent> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = node_cas_state(&transaction, run_id, node_id)?;
        validate_node_cas(&current, cas, next)?;
        let now = now_ms();
        let changed = transaction.execute(
            "UPDATE nodes
             SET status=?1, revision=revision+1, result_json=COALESCE(?2, result_json),
                 updated_at_ms=?3
             WHERE run_id=?4 AND id=?5 AND revision=?6 AND generation=?7
               AND lease_epoch=?8 AND payload_hash=?9",
            params![
                next.as_str(),
                result.map(serde_json::to_string).transpose()?,
                now,
                run_id,
                node_id,
                to_i64(cas.expected_revision, "node revision")?,
                to_i64(cas.expected_generation, "node generation")?,
                to_i64(cas.expected_lease_epoch, "lease epoch")?,
                cas.expected_payload_hash,
            ],
        )?;
        ensure_changed(changed, "node changed during transition")?;
        let sequence = insert_event_and_outbox(&transaction, run_id, event)?;
        transaction.commit()?;
        Ok(event.materialize(run_id, sequence, now))
    }

    // CAS coordinates and completion evidence stay explicit at the atomic
    // attempt/node commit boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_attempt_and_node(
        &self,
        run_id: &str,
        node_id: &str,
        cas: &NodeCas,
        next: NodeStatus,
        result: Option<&Value>,
        error: Option<&str>,
        event: &EventDraft,
    ) -> WorkflowResult<WorkflowEvent> {
        if !matches!(
            next,
            NodeStatus::Succeeded
                | NodeStatus::Failed
                | NodeStatus::Canceled
                | NodeStatus::UnknownOutcome
                | NodeStatus::WaitingApproval
        ) {
            return Err(WorkflowError::InvalidRequest(
                "attempt completion requires a completion or waiting status".to_string(),
            ));
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = node_cas_state(&transaction, run_id, node_id)?;
        validate_node_cas(&current, cas, next)?;
        let now = now_ms();
        let changed = transaction.execute(
            "UPDATE nodes
             SET status=?1, revision=revision+1, result_json=COALESCE(?2, result_json),
                 lease_owner=CASE WHEN ?1='waiting_approval' THEN lease_owner ELSE NULL END,
                 lease_expires_at_ms=CASE WHEN ?1='waiting_approval' THEN lease_expires_at_ms ELSE NULL END,
                 updated_at_ms=?3
             WHERE run_id=?4 AND id=?5 AND revision=?6 AND generation=?7
               AND lease_epoch=?8 AND payload_hash=?9",
            params![
                next.as_str(),
                result.map(serde_json::to_string).transpose()?,
                now,
                run_id,
                node_id,
                to_i64(cas.expected_revision, "node revision")?,
                to_i64(cas.expected_generation, "node generation")?,
                to_i64(cas.expected_lease_epoch, "lease epoch")?,
                cas.expected_payload_hash,
            ],
        )?;
        ensure_changed(changed, "node changed during attempt completion")?;
        let attempt_status = match next {
            NodeStatus::Succeeded => AttemptStatus::Succeeded,
            NodeStatus::Failed => AttemptStatus::Failed,
            NodeStatus::Canceled => AttemptStatus::Canceled,
            NodeStatus::UnknownOutcome => AttemptStatus::UnknownOutcome,
            NodeStatus::WaitingApproval => AttemptStatus::Running,
            _ => unreachable!(),
        };
        if next != NodeStatus::WaitingApproval {
            let attempt_changed = transaction.execute(
                "UPDATE attempts SET status=?1, completed_at_ms=?2, error=?3
                 WHERE id=(
                    SELECT id FROM attempts
                    WHERE run_id=?4 AND node_id=?5 AND generation=?6 AND lease_epoch=?7
                      AND status='running'
                    ORDER BY number DESC LIMIT 1
                 )",
                params![
                    attempt_status.as_str(),
                    now,
                    error,
                    run_id,
                    node_id,
                    to_i64(cas.expected_generation, "attempt generation")?,
                    to_i64(cas.expected_lease_epoch, "attempt lease epoch")?,
                ],
            )?;
            ensure_changed(
                attempt_changed,
                "running attempt was not found during completion",
            )?;
        }
        let sequence = insert_event_and_outbox_at(&transaction, run_id, event, now)?;
        transaction.commit()?;
        Ok(event.materialize(run_id, sequence, now))
    }

    /// Commits the virtual FinalDelivery node and the successful run together,
    /// but only after the adapter has acknowledged (or reconciled) the origin
    /// thread injection.
    // Final delivery uses one explicit transactional fence across run, node,
    // content and event identities.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_succeeded_after_delivery(
        &self,
        run_id: &str,
        final_node_id: &str,
        expected_run_revision: u64,
        expected_run_generation: u64,
        final_node_cas: &NodeCas,
        content: &Value,
        event: &EventDraft,
    ) -> WorkflowResult<WorkflowEvent> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = run_record_in_tx(&transaction, run_id)?;
        if run.final_delivery_committed && run.status == RunStatus::Succeeded {
            return Err(WorkflowError::Conflict(
                "run is already finalized".to_string(),
            ));
        }
        if run.revision != expected_run_revision || run.generation != expected_run_generation {
            return Err(WorkflowError::Conflict(
                "run changed while committing final delivery".to_string(),
            ));
        }
        run.status.validate_transition(RunStatus::Succeeded)?;
        let node = node_cas_state(&transaction, run_id, final_node_id)?;
        validate_node_cas(&node, final_node_cas, NodeStatus::Succeeded)?;
        let dependencies: Vec<String> = transaction.query_row(
            "SELECT dependencies_json FROM nodes WHERE run_id=?1 AND id=?2",
            params![run_id, final_node_id],
            |row| json_column(row, 0),
        )?;
        for dependency in dependencies {
            let dependency_status: NodeStatus = transaction.query_row(
                "SELECT status FROM nodes WHERE run_id=?1 AND id=?2",
                params![run_id, dependency],
                |row| enum_column(row, 0),
            )?;
            if !matches!(
                dependency_status,
                NodeStatus::Succeeded | NodeStatus::Skipped | NodeStatus::Compensated
            ) {
                return Err(WorkflowError::Conflict(
                    "final delivery acceptance dependencies have not passed".to_string(),
                ));
            }
        }
        let now = now_ms();
        let node_changed = transaction.execute(
            "UPDATE nodes SET status='succeeded', revision=revision+1, result_json=?1,
                    lease_owner=NULL, lease_expires_at_ms=NULL, updated_at_ms=?2
             WHERE run_id=?3 AND id=?4 AND revision=?5 AND generation=?6
               AND lease_epoch=?7 AND payload_hash=?8 AND status='ready'",
            params![
                serde_json::to_string(content)?,
                now,
                run_id,
                final_node_id,
                to_i64(final_node_cas.expected_revision, "node revision")?,
                to_i64(final_node_cas.expected_generation, "node generation")?,
                to_i64(final_node_cas.expected_lease_epoch, "node lease epoch")?,
                final_node_cas.expected_payload_hash,
            ],
        )?;
        ensure_changed(node_changed, "final node changed while committing delivery")?;
        let run_changed = transaction.execute(
            "UPDATE runs SET status='succeeded', final_delivery_committed=1,
                    revision=revision+1, updated_at_ms=?1, error=NULL
             WHERE id=?2 AND revision=?3 AND generation=?4 AND final_delivery_committed=0",
            params![
                now,
                run_id,
                to_i64(expected_run_revision, "run revision")?,
                to_i64(expected_run_generation, "run generation")?,
            ],
        )?;
        ensure_changed(run_changed, "run changed while committing final delivery")?;
        let sequence = insert_event_and_outbox_at(&transaction, run_id, event, now)?;
        transaction.commit()?;
        Ok(event.materialize(run_id, sequence, now))
    }

    pub fn reset_node_for_retry(
        &self,
        run_id: &str,
        node_id: &str,
        cas: &NodeCas,
        new_payload_hash: &str,
        event: &EventDraft,
    ) -> WorkflowResult<WorkflowEvent> {
        if new_payload_hash.is_empty() {
            return Err(WorkflowError::InvalidRequest(
                "retry payload hash cannot be empty".to_string(),
            ));
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = node_cas_state(&transaction, run_id, node_id)?;
        validate_node_cas(&current, cas, NodeStatus::Ready)?;
        if current.status == NodeStatus::UnknownOutcome && current.repo_writer {
            return Err(WorkflowError::Conflict(
                "a writer with unknown outcome cannot be replayed automatically".to_string(),
            ));
        }
        if current.attempt_count >= current.max_attempts {
            return Err(WorkflowError::Conflict(format!(
                "node {node_id} exhausted its retry budget"
            )));
        }
        let now = now_ms();
        let changed = transaction.execute(
            "UPDATE nodes
             SET status='ready', revision=revision+1, generation=generation+1,
                 lease_epoch=lease_epoch+1, lease_owner=NULL, lease_expires_at_ms=NULL,
                 payload_hash=?1, result_json=NULL, updated_at_ms=?2
             WHERE run_id=?3 AND id=?4 AND revision=?5 AND generation=?6
               AND lease_epoch=?7 AND payload_hash=?8",
            params![
                new_payload_hash,
                now,
                run_id,
                node_id,
                to_i64(cas.expected_revision, "node revision")?,
                to_i64(cas.expected_generation, "node generation")?,
                to_i64(cas.expected_lease_epoch, "lease epoch")?,
                cas.expected_payload_hash,
            ],
        )?;
        ensure_changed(changed, "node changed while scheduling retry")?;
        let created_at = now_ms();
        let sequence = insert_event_and_outbox_at(&transaction, run_id, event, created_at)?;
        transaction.commit()?;
        Ok(event.materialize(run_id, sequence, created_at))
    }

    /// Atomically schedules a logical Builder repair and invalidates every
    /// downstream validation/review result. Attempt counters remain monotonic,
    /// while generations, lease epochs and payload hashes fence late workers.
    pub fn schedule_builder_repair(
        &self,
        run_id: &str,
        builder: &NodeRecord,
        downstream: &[NodeRecord],
        repair_cycle: u8,
        feedback: &str,
        event: &EventDraft,
    ) -> WorkflowResult<WorkflowEvent> {
        if builder.run_id != run_id
            || builder.role != NodeRole::Builder
            || builder.status != NodeStatus::Succeeded
        {
            return Err(WorkflowError::Conflict(
                "builder is not in a repairable state".to_string(),
            ));
        }
        if builder.attempt_count >= builder.max_attempts {
            return Err(WorkflowError::Conflict(
                "builder exhausted its combined retry and repair budget".to_string(),
            ));
        }
        if downstream.is_empty()
            || downstream.iter().any(|node| {
                node.run_id != run_id
                    || node.role == NodeRole::Builder
                    || !matches!(node.status, NodeStatus::Succeeded | NodeStatus::Ready)
            })
        {
            return Err(WorkflowError::Conflict(
                "repair cascade contains a stale or invalid downstream node".to_string(),
            ));
        }

        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = run_record_in_tx(&transaction, run_id)?;
        if run.status != RunStatus::Running {
            return Err(WorkflowError::Conflict(
                "repairs can only be scheduled for a running workflow".to_string(),
            ));
        }
        let now = now_ms();
        let builder_hash = payload_hash(&serde_json::json!({
            "previous": builder.payload_hash,
            "repairCycle": repair_cycle,
            "feedback": feedback,
            "generation": builder.generation + 1,
        }))?;
        let changed = transaction.execute(
            "UPDATE nodes
             SET status='ready', revision=revision+1, generation=generation+1,
                 lease_epoch=lease_epoch+1, lease_owner=NULL, lease_expires_at_ms=NULL,
                 payload_hash=?1, result_json=NULL, thread_id=NULL, updated_at_ms=?2
             WHERE run_id=?3 AND id=?4 AND status='succeeded' AND revision=?5
               AND generation=?6 AND lease_epoch=?7 AND payload_hash=?8
               AND attempt_count < max_attempts",
            params![
                builder_hash,
                now,
                run_id,
                builder.id,
                to_i64(builder.revision, "builder revision")?,
                to_i64(builder.generation, "builder generation")?,
                to_i64(builder.lease_epoch, "builder lease epoch")?,
                builder.payload_hash,
            ],
        )?;
        ensure_changed(changed, "builder changed while scheduling repair")?;
        transaction.execute(
            "DELETE FROM thread_links WHERE run_id=?1 AND node_id=?2",
            params![run_id, builder.id],
        )?;

        for node in downstream {
            let next_hash = payload_hash(&serde_json::json!({
                "previous": node.payload_hash,
                "repairCycle": repair_cycle,
                "builder": builder.id,
                "generation": node.generation + 1,
            }))?;
            let changed = transaction.execute(
                "UPDATE nodes
                 SET status='pending', revision=revision+1, generation=generation+1,
                     lease_epoch=lease_epoch+1, lease_owner=NULL, lease_expires_at_ms=NULL,
                     payload_hash=?1, result_json=NULL, thread_id=NULL, updated_at_ms=?2
                 WHERE run_id=?3 AND id=?4 AND status=?5 AND revision=?6
                   AND generation=?7 AND lease_epoch=?8 AND payload_hash=?9",
                params![
                    next_hash,
                    now,
                    run_id,
                    node.id,
                    node.status.as_str(),
                    to_i64(node.revision, "downstream revision")?,
                    to_i64(node.generation, "downstream generation")?,
                    to_i64(node.lease_epoch, "downstream lease epoch")?,
                    node.payload_hash,
                ],
            )?;
            ensure_changed(changed, "downstream node changed while scheduling repair")?;
            transaction.execute(
                "DELETE FROM thread_links WHERE run_id=?1 AND node_id=?2",
                params![run_id, node.id],
            )?;
        }
        let sequence = insert_event_and_outbox_at(&transaction, run_id, event, now)?;
        transaction.commit()?;
        Ok(event.materialize(run_id, sequence, now))
    }

    pub fn reset_node_for_retry_command(
        &self,
        command: &CommandEnvelope,
        run_id: &str,
        node_id: &str,
        cas: &NodeCas,
        new_payload_hash: &str,
        event: &EventDraft,
    ) -> WorkflowResult<CommandOutcome<DurableAck>> {
        if command.run_id != run_id || new_payload_hash.is_empty() {
            return Err(WorkflowError::InvalidRequest(
                "retry command run id and payload hash must be valid".to_string(),
            ));
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(stored) = command_in_tx(&transaction, &command.command_id)? {
            validate_command_replay(&stored, command)?;
            let mut ack: DurableAck = serde_json::from_str(&stored.response_json)?;
            ack.duplicate = true;
            transaction.commit()?;
            return Ok(CommandOutcome::Duplicate(ack));
        }
        let current = node_cas_state(&transaction, run_id, node_id)?;
        validate_node_cas(&current, cas, NodeStatus::Ready)?;
        if current.status == NodeStatus::UnknownOutcome && current.repo_writer {
            return Err(WorkflowError::Conflict(
                "a writer with unknown outcome cannot be replayed automatically".to_string(),
            ));
        }
        if current.attempt_count >= current.max_attempts {
            return Err(WorkflowError::Conflict(format!(
                "node {node_id} exhausted its retry budget"
            )));
        }
        let (run_status, run_revision, run_generation, final_committed) =
            run_cas_state(&transaction, run_id)?;
        let requeue_run = matches!(run_status, RunStatus::NeedsAttention | RunStatus::Failed);
        if requeue_run {
            validate_run_cas(
                run_status,
                run_revision,
                run_generation,
                final_committed,
                run_revision,
                run_generation,
                RunStatus::Queued,
            )?;
        }
        let now = now_ms();
        let changed = transaction.execute(
            "UPDATE nodes
             SET status='ready', revision=revision+1, generation=generation+1,
                 lease_epoch=lease_epoch+1, lease_owner=NULL, lease_expires_at_ms=NULL,
                 payload_hash=?1, result_json=NULL, updated_at_ms=?2
             WHERE run_id=?3 AND id=?4 AND revision=?5 AND generation=?6
               AND lease_epoch=?7 AND payload_hash=?8",
            params![
                new_payload_hash,
                now,
                run_id,
                node_id,
                to_i64(cas.expected_revision, "node revision")?,
                to_i64(cas.expected_generation, "node generation")?,
                to_i64(cas.expected_lease_epoch, "lease epoch")?,
                cas.expected_payload_hash,
            ],
        )?;
        ensure_changed(changed, "node changed while scheduling retry")?;
        if requeue_run {
            let changed = transaction.execute(
                "UPDATE runs
                 SET status='queued', revision=revision+1, updated_at_ms=?1, error=NULL
                 WHERE id=?2 AND revision=?3 AND generation=?4",
                params![
                    now,
                    run_id,
                    to_i64(run_revision, "run revision")?,
                    to_i64(run_generation, "run generation")?,
                ],
            )?;
            ensure_changed(changed, "run changed while scheduling node retry")?;
        }
        let sequence = insert_event_and_outbox(&transaction, run_id, event)?;
        let ack = DurableAck {
            command_id: command.command_id.clone(),
            run_id: run_id.to_string(),
            workflow_sequence: sequence,
            duplicate: false,
        };
        insert_command(&transaction, command, &ack)?;
        transaction.commit()?;
        Ok(CommandOutcome::Applied(ack))
    }

    pub fn acquire_lease(
        &self,
        run_id: &str,
        node_id: &str,
        owner: &str,
        expected_generation: u64,
        expected_epoch: u64,
        ttl: Duration,
    ) -> WorkflowResult<Lease> {
        if owner.trim().is_empty() || ttl.is_zero() {
            return Err(WorkflowError::InvalidRequest(
                "lease owner and positive ttl are required".to_string(),
            ));
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = node_cas_state(&transaction, run_id, node_id)?;
        if state.status != NodeStatus::Ready
            || state.generation != expected_generation
            || state.lease_epoch != expected_epoch
        {
            return Err(WorkflowError::Conflict(format!(
                "node lease CAS failed for {run_id}/{node_id}"
            )));
        }
        let epoch = expected_epoch
            .checked_add(1)
            .ok_or_else(|| WorkflowError::Conflict("lease epoch overflow".to_string()))?;
        let ttl_ms = i64::try_from(ttl.as_millis())
            .map_err(|_| WorkflowError::InvalidRequest("lease ttl is too large".to_string()))?;
        let expires_at = now_ms().saturating_add(ttl_ms);
        let changed = transaction.execute(
            "UPDATE nodes SET status='leased', revision=revision+1, lease_epoch=?1,
                    lease_owner=?2, lease_expires_at_ms=?3, updated_at_ms=?4
             WHERE run_id=?5 AND id=?6 AND status='ready' AND generation=?7 AND lease_epoch=?8",
            params![
                to_i64(epoch, "lease epoch")?,
                owner,
                expires_at,
                now_ms(),
                run_id,
                node_id,
                to_i64(expected_generation, "node generation")?,
                to_i64(expected_epoch, "lease epoch")?,
            ],
        )?;
        ensure_changed(changed, "node lease changed concurrently")?;
        insert_event_and_outbox(
            &transaction,
            run_id,
            &EventDraft::new(
                "node.leased",
                serde_json::json!({ "nodeId": node_id, "owner": owner, "epoch": epoch }),
            ),
        )?;
        transaction.commit()?;
        Ok(Lease {
            run_id: run_id.to_string(),
            node_id: node_id.to_string(),
            owner: owner.to_string(),
            generation: expected_generation,
            epoch,
            expires_at_ms: expires_at,
        })
    }

    pub fn renew_lease(&self, lease: &Lease, ttl: Duration) -> WorkflowResult<Lease> {
        if ttl.is_zero() {
            return Err(WorkflowError::InvalidRequest(
                "lease ttl must be positive".to_string(),
            ));
        }
        let ttl_ms = i64::try_from(ttl.as_millis())
            .map_err(|_| WorkflowError::InvalidRequest("lease ttl is too large".to_string()))?;
        let now = now_ms();
        let expires_at = now.saturating_add(ttl_ms);
        let connection = self.connect()?;
        let changed = connection.execute(
            "UPDATE nodes SET lease_expires_at_ms=?1, revision=revision+1, updated_at_ms=?2
             WHERE run_id=?3 AND id=?4 AND status IN ('leased', 'running')
               AND generation=?5 AND lease_epoch=?6 AND lease_owner=?7
               AND lease_expires_at_ms>=?2",
            params![
                expires_at,
                now,
                lease.run_id,
                lease.node_id,
                to_i64(lease.generation, "node generation")?,
                to_i64(lease.epoch, "lease epoch")?,
                lease.owner,
            ],
        )?;
        ensure_changed(changed, "lease is stale, expired, or no longer owned")?;
        Ok(Lease {
            expires_at_ms: expires_at,
            ..lease.clone()
        })
    }

    #[cfg(test)]
    pub fn release_lease(&self, lease: &Lease) -> WorkflowResult<()> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE nodes SET status='ready', revision=revision+1, lease_owner=NULL,
                    lease_expires_at_ms=NULL, updated_at_ms=?1
             WHERE run_id=?2 AND id=?3 AND status='leased' AND generation=?4
               AND lease_epoch=?5 AND lease_owner=?6",
            params![
                now_ms(),
                lease.run_id,
                lease.node_id,
                to_i64(lease.generation, "node generation")?,
                to_i64(lease.epoch, "lease epoch")?,
                lease.owner,
            ],
        )?;
        ensure_changed(changed, "lease is stale or no longer owned")?;
        insert_event_and_outbox(
            &transaction,
            &lease.run_id,
            &EventDraft::new(
                "node.lease_released",
                serde_json::json!({ "nodeId": lease.node_id, "epoch": lease.epoch }),
            ),
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn start_attempt(&self, lease: &Lease, is_write: bool) -> WorkflowResult<String> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = node_cas_state(&transaction, &lease.run_id, &lease.node_id)?;
        if state.status != NodeStatus::Leased
            || state.generation != lease.generation
            || state.lease_epoch != lease.epoch
            || state.lease_owner.as_deref() != Some(lease.owner.as_str())
            || state.lease_expires_at_ms.unwrap_or_default() < now_ms()
        {
            return Err(WorkflowError::Conflict(
                "cannot start an attempt with a stale lease".to_string(),
            ));
        }
        if state.attempt_count >= state.max_attempts {
            return Err(WorkflowError::Conflict(
                "node attempt budget exhausted".to_string(),
            ));
        }
        let attempt_number = state.attempt_count + 1;
        let attempt_id = uuid::Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO attempts (
                id, run_id, node_id, number, generation, lease_epoch, status,
                is_write, started_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running', ?7, ?8)",
            params![
                attempt_id,
                lease.run_id,
                lease.node_id,
                attempt_number,
                to_i64(lease.generation, "node generation")?,
                to_i64(lease.epoch, "lease epoch")?,
                is_write,
                now_ms(),
            ],
        )?;
        transaction.execute(
            "UPDATE nodes SET status='running', revision=revision+1,
                    attempt_count=?1, updated_at_ms=?2
             WHERE run_id=?3 AND id=?4 AND status='leased' AND generation=?5
               AND lease_epoch=?6 AND lease_owner=?7",
            params![
                attempt_number,
                now_ms(),
                lease.run_id,
                lease.node_id,
                to_i64(lease.generation, "node generation")?,
                to_i64(lease.epoch, "lease epoch")?,
                lease.owner,
            ],
        )?;
        insert_event_and_outbox(
            &transaction,
            &lease.run_id,
            &EventDraft::new(
                "node.started",
                serde_json::json!({
                    "nodeId": lease.node_id,
                    "attemptId": attempt_id,
                    "attemptNumber": attempt_number,
                }),
            ),
        )?;
        transaction.commit()?;
        Ok(attempt_id)
    }

    pub fn record_thread_link(
        &self,
        run_id: &str,
        node_id: &str,
        thread_id: &str,
    ) -> WorkflowResult<()> {
        if thread_id.trim().is_empty() {
            return Err(WorkflowError::InvalidRequest(
                "thread id cannot be empty".to_string(),
            ));
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_node(&transaction, run_id, node_id)?;
        transaction.execute(
            "INSERT INTO thread_links (run_id, node_id, thread_id, created_at_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(run_id, node_id) DO UPDATE SET thread_id=excluded.thread_id",
            params![run_id, node_id, thread_id, now_ms()],
        )?;
        transaction.execute(
            "UPDATE nodes SET thread_id=?1, revision=revision+1, updated_at_ms=?2
             WHERE run_id=?3 AND id=?4",
            params![thread_id, now_ms(), run_id, node_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn events_after(
        &self,
        run_id: &str,
        after_sequence: u64,
        limit: usize,
    ) -> WorkflowResult<Vec<WorkflowEvent>> {
        let connection = self.connect()?;
        require_run_connection(&connection, run_id)?;
        let mut statement = connection.prepare(
            "SELECT event_id, run_id, workflow_sequence, kind, payload_json, created_at_ms
             FROM events WHERE run_id=?1 AND workflow_sequence>?2
             ORDER BY workflow_sequence ASC LIMIT ?3",
        )?;
        Ok(statement
            .query_map(
                params![
                    run_id,
                    to_i64(after_sequence, "event sequence")?,
                    clamp_limit(limit, 500) as i64,
                ],
                row_to_event,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn insert_artifact(&self, artifact: &ArtifactMetadata) -> WorkflowResult<()> {
        let connection = self.connect()?;
        connection.execute(
            "INSERT INTO artifacts (
                id, run_id, node_id, name, mime_type, size, storage_key, sha256,
                metadata_json, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                artifact.id,
                artifact.run_id,
                artifact.node_id,
                artifact.name,
                artifact.mime_type,
                to_i64(artifact.size, "artifact size")?,
                artifact.storage_key,
                artifact.sha256,
                serde_json::to_string(&artifact.metadata)?,
                artifact.created_at_ms,
            ],
        )?;
        Ok(())
    }

    pub fn get_artifact(
        &self,
        run_id: &str,
        artifact_id: &str,
    ) -> WorkflowResult<ArtifactMetadata> {
        let connection = self.connect()?;
        connection
            .query_row(
                "SELECT id, run_id, node_id, name, mime_type, size, storage_key,
                        sha256, metadata_json, created_at_ms
                 FROM artifacts WHERE run_id=?1 AND id=?2",
                params![run_id, artifact_id],
                row_to_artifact,
            )
            .optional()?
            .ok_or_else(|| WorkflowError::NotFound {
                entity: "artifact",
                id: format!("{run_id}/{artifact_id}"),
            })
    }

    pub fn artifacts_page(
        &self,
        run_id: &str,
        after_id: Option<&str>,
        limit: usize,
    ) -> WorkflowResult<ArtifactPage> {
        let connection = self.connect()?;
        require_run_connection(&connection, run_id)?;
        let limit = clamp_limit(limit, 100);
        let mut statement = connection.prepare(
            "SELECT id, run_id, node_id, name, mime_type, size, storage_key,
                    sha256, metadata_json, created_at_ms
             FROM artifacts
             WHERE run_id=?1 AND (?2 IS NULL OR id>?2)
             ORDER BY id ASC LIMIT ?3",
        )?;
        let mut items = statement
            .query_map(
                params![run_id, after_id, (limit + 1) as i64],
                row_to_artifact,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let next_after = if items.len() > limit {
            items.pop();
            items.last().map(|artifact| artifact.id.clone())
        } else {
            None
        };
        Ok(ArtifactPage { items, next_after })
    }

    #[allow(dead_code)]
    pub fn create_approval(&self, approval: &ApprovalRecord) -> WorkflowResult<()> {
        let connection = self.connect()?;
        connection.execute(
            "INSERT INTO approvals (
                id, run_id, node_id, status, prompt, response_json,
                requested_at_ms, resolved_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                approval.id,
                approval.run_id,
                approval.node_id,
                approval.status.as_str(),
                approval.prompt,
                approval
                    .response
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                approval.requested_at_ms,
                approval.resolved_at_ms,
            ],
        )?;
        Ok(())
    }

    pub fn resolve_approval_command(
        &self,
        command: &CommandEnvelope,
        approval_id: &str,
        decision: ApprovalStatus,
        response: &Value,
        event: &EventDraft,
    ) -> WorkflowResult<CommandOutcome<DurableAck>> {
        if !matches!(
            decision,
            ApprovalStatus::Approved | ApprovalStatus::Rejected
        ) {
            return Err(WorkflowError::InvalidRequest(
                "approval reply must approve or reject".to_string(),
            ));
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(stored) = command_in_tx(&transaction, &command.command_id)? {
            validate_command_replay(&stored, command)?;
            let mut ack: DurableAck = serde_json::from_str(&stored.response_json)?;
            ack.duplicate = true;
            transaction.commit()?;
            return Ok(CommandOutcome::Duplicate(ack));
        }
        let approval_run: Option<String> = transaction
            .query_row(
                "SELECT run_id FROM approvals WHERE id=?1 AND status='pending'",
                [approval_id],
                |row| row.get(0),
            )
            .optional()?;
        let approval_run = approval_run.ok_or_else(|| {
            WorkflowError::Conflict(format!(
                "approval is missing or already resolved: {approval_id}"
            ))
        })?;
        if approval_run != command.run_id {
            return Err(WorkflowError::Conflict(
                "approval belongs to a different run".to_string(),
            ));
        }
        let changed = transaction.execute(
            "UPDATE approvals SET status=?1, response_json=?2, resolved_at_ms=?3
             WHERE id=?4 AND status='pending'",
            params![
                decision.as_str(),
                serde_json::to_string(response)?,
                now_ms(),
                approval_id
            ],
        )?;
        ensure_changed(changed, "approval changed concurrently")?;
        let sequence = insert_event_and_outbox(&transaction, &command.run_id, event)?;
        let ack = DurableAck {
            command_id: command.command_id.clone(),
            run_id: command.run_id.clone(),
            workflow_sequence: sequence,
            duplicate: false,
        };
        insert_command(&transaction, command, &ack)?;
        transaction.commit()?;
        Ok(CommandOutcome::Applied(ack))
    }

    /// Returns undelivered records for an at-least-once outbox dispatcher.
    /// Consumers must deduplicate by `event_id`; no exactly-once guarantee is made.
    #[allow(dead_code)]
    pub fn pending_outbox(&self, limit: usize) -> WorkflowResult<Vec<OutboxRecord>> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT id, event_id, topic, payload_json, attempts
             FROM outbox WHERE delivered_at_ms IS NULL ORDER BY id LIMIT ?1",
        )?;
        Ok(statement
            .query_map([clamp_limit(limit, 500) as i64], |row| {
                Ok(OutboxRecord {
                    id: row.get(0)?,
                    event_id: row.get(1)?,
                    topic: row.get(2)?,
                    payload: json_column(row, 3)?,
                    attempts: u32_column(row, 4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    #[allow(dead_code)]
    pub fn mark_outbox_delivered(&self, id: i64) -> WorkflowResult<bool> {
        let connection = self.connect()?;
        Ok(connection.execute(
            "UPDATE outbox SET delivered_at_ms=?1, attempts=attempts+1
             WHERE id=?2 AND delivered_at_ms IS NULL",
            params![now_ms(), id],
        )? == 1)
    }

    #[allow(dead_code)]
    pub fn mark_outbox_attempt_failed(&self, id: i64) -> WorkflowResult<bool> {
        let connection = self.connect()?;
        Ok(connection.execute(
            "UPDATE outbox SET attempts=attempts+1 WHERE id=?1 AND delivered_at_ms IS NULL",
            [id],
        )? == 1)
    }

    pub fn command(&self, command_id: &str) -> WorkflowResult<Option<StoredCommand>> {
        let connection = self.connect()?;
        Ok(connection
            .query_row(
                "SELECT command_id, payload_hash, response_json FROM commands WHERE command_id=?1",
                [command_id],
                |row| {
                    Ok(StoredCommand {
                        command_id: row.get(0)?,
                        payload_hash: row.get(1)?,
                        response_json: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }
}

impl EventDraft {
    fn materialize(&self, run_id: &str, sequence: u64, created_at_ms: i64) -> WorkflowEvent {
        WorkflowEvent {
            event_id: self.event_id.clone(),
            run_id: run_id.to_string(),
            workflow_sequence: sequence,
            kind: self.kind.clone(),
            payload: self.payload.clone(),
            created_at_ms,
        }
    }
}

fn migrate(connection: &mut Connection) -> WorkflowResult<()> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(WorkflowError::Storage(format!(
            "workflow journal schema {version} is newer than supported {SCHEMA_VERSION}"
        )));
    }
    if version < 1 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "CREATE TABLE runs (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                route TEXT NOT NULL,
                revision INTEGER NOT NULL CHECK(revision >= 0),
                generation INTEGER NOT NULL CHECK(generation >= 0),
                input_json TEXT NOT NULL,
                policy_json TEXT NOT NULL,
                permissions_json TEXT NOT NULL,
                workspace_risk TEXT NOT NULL,
                final_delivery_committed INTEGER NOT NULL DEFAULT 0,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                error TEXT
             );
             CREATE INDEX runs_status_updated_idx ON runs(status, updated_at_ms DESC);

             CREATE TABLE nodes (
                run_id TEXT NOT NULL,
                id TEXT NOT NULL,
                role TEXT NOT NULL,
                label TEXT NOT NULL,
                status TEXT NOT NULL,
                revision INTEGER NOT NULL CHECK(revision >= 0),
                generation INTEGER NOT NULL CHECK(generation >= 0),
                lease_epoch INTEGER NOT NULL CHECK(lease_epoch >= 0),
                lease_owner TEXT,
                lease_expires_at_ms INTEGER,
                payload_hash TEXT NOT NULL,
                dependencies_json TEXT NOT NULL,
                permissions_json TEXT NOT NULL,
                repo_writer INTEGER NOT NULL,
                provider TEXT NOT NULL,
                depth INTEGER NOT NULL CHECK(depth >= 0),
                attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
                max_attempts INTEGER NOT NULL CHECK(max_attempts > 0),
                result_json TEXT,
                thread_id TEXT,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY(run_id, id),
                FOREIGN KEY(run_id) REFERENCES runs(id) ON DELETE CASCADE
             );
             CREATE INDEX nodes_runnable_idx ON nodes(status, provider, repo_writer);

             CREATE TABLE attempts (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                node_id TEXT NOT NULL,
                number INTEGER NOT NULL CHECK(number > 0),
                generation INTEGER NOT NULL,
                lease_epoch INTEGER NOT NULL,
                status TEXT NOT NULL,
                is_write INTEGER NOT NULL,
                started_at_ms INTEGER NOT NULL,
                completed_at_ms INTEGER,
                error TEXT,
                UNIQUE(run_id, node_id, number),
                FOREIGN KEY(run_id, node_id) REFERENCES nodes(run_id, id) ON DELETE CASCADE
             );

             CREATE TABLE events (
                event_id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                workflow_sequence INTEGER NOT NULL CHECK(workflow_sequence > 0),
                kind TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                UNIQUE(run_id, workflow_sequence),
                FOREIGN KEY(run_id) REFERENCES runs(id) ON DELETE CASCADE
             );
             CREATE INDEX events_after_idx ON events(run_id, workflow_sequence);

             CREATE TABLE outbox (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id TEXT NOT NULL UNIQUE,
                topic TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                delivered_at_ms INTEGER,
                attempts INTEGER NOT NULL DEFAULT 0,
                FOREIGN KEY(event_id) REFERENCES events(event_id) ON DELETE CASCADE
             );
             CREATE INDEX outbox_pending_idx ON outbox(delivered_at_ms, id);

             CREATE TABLE artifacts (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                node_id TEXT,
                name TEXT NOT NULL,
                mime_type TEXT NOT NULL,
                size INTEGER NOT NULL CHECK(size >= 0),
                storage_key TEXT NOT NULL UNIQUE,
                sha256 TEXT NOT NULL,
                metadata_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                FOREIGN KEY(run_id) REFERENCES runs(id) ON DELETE CASCADE,
                FOREIGN KEY(run_id, node_id) REFERENCES nodes(run_id, id) ON DELETE CASCADE
             );
             CREATE INDEX artifacts_page_idx ON artifacts(run_id, id);

             CREATE TABLE approvals (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                node_id TEXT NOT NULL,
                status TEXT NOT NULL,
                prompt TEXT NOT NULL,
                response_json TEXT,
                requested_at_ms INTEGER NOT NULL,
                resolved_at_ms INTEGER,
                FOREIGN KEY(run_id, node_id) REFERENCES nodes(run_id, id) ON DELETE CASCADE
             );
             CREATE INDEX approvals_pending_idx ON approvals(run_id, status);

             CREATE TABLE commands (
                command_id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                payload_hash TEXT NOT NULL,
                response_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                FOREIGN KEY(run_id) REFERENCES runs(id) ON DELETE CASCADE
             );
             CREATE INDEX commands_run_idx ON commands(run_id, created_at_ms);

             CREATE TABLE thread_links (
                run_id TEXT NOT NULL,
                node_id TEXT NOT NULL,
                thread_id TEXT NOT NULL UNIQUE,
                created_at_ms INTEGER NOT NULL,
                PRIMARY KEY(run_id, node_id),
                FOREIGN KEY(run_id, node_id) REFERENCES nodes(run_id, id) ON DELETE CASCADE
             );
             PRAGMA user_version = 1;",
        )?;
        transaction.commit()?;
    }
    Ok(())
}

fn insert_node(
    transaction: &Transaction<'_>,
    run: &RunRecord,
    node: &NodeSpec,
) -> WorkflowResult<()> {
    let initial_status = if node.dependencies.is_empty() {
        NodeStatus::Ready
    } else {
        NodeStatus::Pending
    };
    let payload_hash = payload_hash(&serde_json::json!({
        "runId": run.id,
        "nodeId": node.id,
        "role": node.role,
        "input": run.input,
        "generation": run.generation,
    }))?;
    transaction.execute(
        "INSERT INTO nodes (
            run_id, id, role, label, status, revision, generation, lease_epoch,
            payload_hash, dependencies_json, permissions_json, repo_writer,
            provider, depth, attempt_count, max_attempts, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, 0, ?7, ?8, ?9, ?10, ?11, ?12, 0, ?13, ?14)",
        params![
            run.id,
            node.id,
            node.role.as_str(),
            node.label,
            initial_status.as_str(),
            to_i64(run.generation, "node generation")?,
            payload_hash,
            serde_json::to_string(&node.dependencies)?,
            serde_json::to_string(&node.permissions)?,
            node.repo_writer,
            node.provider,
            node.depth,
            node.max_attempts,
            run.created_at_ms,
        ],
    )?;
    Ok(())
}

fn insert_event_and_outbox(
    transaction: &Transaction<'_>,
    run_id: &str,
    event: &EventDraft,
) -> WorkflowResult<u64> {
    insert_event_and_outbox_at(transaction, run_id, event, now_ms())
}

fn insert_event_and_outbox_at(
    transaction: &Transaction<'_>,
    run_id: &str,
    event: &EventDraft,
    created_at_ms: i64,
) -> WorkflowResult<u64> {
    if event.event_id.trim().is_empty()
        || event.kind.trim().is_empty()
        || event.topic.trim().is_empty()
    {
        return Err(WorkflowError::InvalidRequest(
            "event id, kind, and outbox topic are required".to_string(),
        ));
    }
    if let Some(existing) = event_by_id_in_tx(transaction, &event.event_id)? {
        validate_event_replay(&existing, run_id, event)?;
        return Ok(existing.workflow_sequence);
    }
    let sequence: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(workflow_sequence), 0) + 1 FROM events WHERE run_id=?1",
        [run_id],
        |row| row.get(0),
    )?;
    let payload_json = serde_json::to_string(&event.payload)?;
    transaction.execute(
        "INSERT INTO events (
            event_id, run_id, workflow_sequence, kind, payload_json, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event.event_id,
            run_id,
            sequence,
            event.kind,
            payload_json,
            created_at_ms
        ],
    )?;
    let outbox_payload = serde_json::to_string(&serde_json::json!({
        "eventId": event.event_id,
        "runId": run_id,
        "workflowSequence": sequence,
        "kind": event.kind,
        "payload": event.payload,
        "createdAtMs": created_at_ms,
    }))?;
    transaction.execute(
        "INSERT INTO outbox (event_id, topic, payload_json, created_at_ms)
         VALUES (?1, ?2, ?3, ?4)",
        params![event.event_id, event.topic, outbox_payload, created_at_ms],
    )?;
    u64::try_from(sequence)
        .map_err(|_| WorkflowError::Storage("negative event sequence".to_string()))
}

fn insert_command<T: Serialize>(
    transaction: &Transaction<'_>,
    command: &CommandEnvelope,
    response: &T,
) -> WorkflowResult<()> {
    transaction.execute(
        "INSERT INTO commands (
            command_id, run_id, kind, payload_hash, response_json, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            command.command_id,
            command.run_id,
            command.kind,
            command.payload_hash,
            serde_json::to_string(response)?,
            now_ms(),
        ],
    )?;
    Ok(())
}

fn command_in_tx(
    transaction: &Transaction<'_>,
    command_id: &str,
) -> WorkflowResult<Option<StoredCommand>> {
    Ok(transaction
        .query_row(
            "SELECT command_id, payload_hash, response_json FROM commands WHERE command_id=?1",
            [command_id],
            |row| {
                Ok(StoredCommand {
                    command_id: row.get(0)?,
                    payload_hash: row.get(1)?,
                    response_json: row.get(2)?,
                })
            },
        )
        .optional()?)
}

fn validate_command_replay(
    stored: &StoredCommand,
    command: &CommandEnvelope,
) -> WorkflowResult<()> {
    if stored.payload_hash != command.payload_hash {
        return Err(WorkflowError::Conflict(format!(
            "command {} was reused with a different payload",
            command.command_id
        )));
    }
    Ok(())
}

fn event_by_id_in_tx(
    transaction: &Transaction<'_>,
    event_id: &str,
) -> WorkflowResult<Option<WorkflowEvent>> {
    Ok(transaction
        .query_row(
            "SELECT event_id, run_id, workflow_sequence, kind, payload_json, created_at_ms
             FROM events WHERE event_id=?1",
            [event_id],
            row_to_event,
        )
        .optional()?)
}

fn validate_event_replay(
    existing: &WorkflowEvent,
    run_id: &str,
    draft: &EventDraft,
) -> WorkflowResult<()> {
    if existing.run_id != run_id || existing.kind != draft.kind || existing.payload != draft.payload
    {
        return Err(WorkflowError::Conflict(format!(
            "event {} was reused with different content",
            draft.event_id
        )));
    }
    Ok(())
}

fn run_cas_state(
    transaction: &Transaction<'_>,
    run_id: &str,
) -> WorkflowResult<(RunStatus, u64, u64, bool)> {
    transaction
        .query_row(
            "SELECT status, revision, generation, final_delivery_committed FROM runs WHERE id=?1",
            [run_id],
            |row| {
                Ok((
                    enum_column(row, 0)?,
                    u64_column(row, 1)?,
                    u64_column(row, 2)?,
                    row.get(3)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| WorkflowError::NotFound {
            entity: "run",
            id: run_id.to_string(),
        })
}

fn validate_run_cas(
    current: RunStatus,
    revision: u64,
    generation: u64,
    final_committed: bool,
    expected_revision: u64,
    expected_generation: u64,
    next: RunStatus,
) -> WorkflowResult<()> {
    if revision != expected_revision || generation != expected_generation {
        return Err(cas_conflict(
            "run",
            expected_revision,
            revision,
            expected_generation,
            generation,
        ));
    }
    current.validate_transition(next)?;
    if next == RunStatus::Succeeded && !final_committed {
        return Err(WorkflowError::Conflict(
            "run cannot succeed before final delivery is durably committed".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct NodeCasState {
    status: NodeStatus,
    revision: u64,
    generation: u64,
    lease_epoch: u64,
    lease_owner: Option<String>,
    lease_expires_at_ms: Option<i64>,
    payload_hash: String,
    repo_writer: bool,
    attempt_count: u32,
    max_attempts: u32,
}

fn node_cas_state(
    transaction: &Transaction<'_>,
    run_id: &str,
    node_id: &str,
) -> WorkflowResult<NodeCasState> {
    transaction
        .query_row(
            "SELECT status, revision, generation, lease_epoch, lease_owner,
                    lease_expires_at_ms, payload_hash, repo_writer, attempt_count, max_attempts
             FROM nodes WHERE run_id=?1 AND id=?2",
            params![run_id, node_id],
            |row| {
                Ok(NodeCasState {
                    status: enum_column(row, 0)?,
                    revision: u64_column(row, 1)?,
                    generation: u64_column(row, 2)?,
                    lease_epoch: u64_column(row, 3)?,
                    lease_owner: row.get(4)?,
                    lease_expires_at_ms: row.get(5)?,
                    payload_hash: row.get(6)?,
                    repo_writer: row.get(7)?,
                    attempt_count: u32_column(row, 8)?,
                    max_attempts: u32_column(row, 9)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| WorkflowError::NotFound {
            entity: "node",
            id: format!("{run_id}/{node_id}"),
        })
}

fn validate_node_cas(state: &NodeCasState, cas: &NodeCas, next: NodeStatus) -> WorkflowResult<()> {
    if state.revision != cas.expected_revision
        || state.generation != cas.expected_generation
        || state.lease_epoch != cas.expected_lease_epoch
        || state.payload_hash != cas.expected_payload_hash
    {
        return Err(WorkflowError::Conflict(
            "node revision, generation, lease epoch, or payload hash changed".to_string(),
        ));
    }
    state.status.validate_transition(next)
}

fn require_run(transaction: &Transaction<'_>, run_id: &str) -> WorkflowResult<()> {
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1)",
        [run_id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(WorkflowError::NotFound {
            entity: "run",
            id: run_id.to_string(),
        })
    }
}

fn run_record_in_tx(transaction: &Transaction<'_>, run_id: &str) -> WorkflowResult<RunRecord> {
    transaction
        .query_row(
            "SELECT id, status, route, revision, generation, input_json, policy_json,
                    final_delivery_committed, created_at_ms, updated_at_ms, error
             FROM runs WHERE id=?1",
            [run_id],
            row_to_run,
        )
        .optional()?
        .ok_or_else(|| WorkflowError::NotFound {
            entity: "run",
            id: run_id.to_string(),
        })
}

fn require_run_connection(connection: &Connection, run_id: &str) -> WorkflowResult<()> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1)",
        [run_id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(WorkflowError::NotFound {
            entity: "run",
            id: run_id.to_string(),
        })
    }
}

fn require_node(transaction: &Transaction<'_>, run_id: &str, node_id: &str) -> WorkflowResult<()> {
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM nodes WHERE run_id=?1 AND id=?2)",
        params![run_id, node_id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(WorkflowError::NotFound {
            entity: "node",
            id: format!("{run_id}/{node_id}"),
        })
    }
}

fn row_to_run(row: &Row<'_>) -> rusqlite::Result<RunRecord> {
    Ok(RunRecord {
        id: row.get(0)?,
        status: enum_column(row, 1)?,
        route: enum_column(row, 2)?,
        revision: u64_column(row, 3)?,
        generation: u64_column(row, 4)?,
        input: json_column(row, 5)?,
        policy: json_column(row, 6)?,
        final_delivery_committed: row.get(7)?,
        created_at_ms: row.get(8)?,
        updated_at_ms: row.get(9)?,
        error: row.get(10)?,
    })
}

fn row_to_node(row: &Row<'_>) -> rusqlite::Result<NodeRecord> {
    Ok(NodeRecord {
        run_id: row.get(0)?,
        id: row.get(1)?,
        role: enum_column(row, 2)?,
        label: row.get(3)?,
        status: enum_column(row, 4)?,
        revision: u64_column(row, 5)?,
        generation: u64_column(row, 6)?,
        lease_epoch: u64_column(row, 7)?,
        lease_owner: row.get(8)?,
        lease_expires_at_ms: row.get(9)?,
        payload_hash: row.get(10)?,
        dependencies: json_column(row, 11)?,
        permissions: json_column(row, 12)?,
        repo_writer: row.get(13)?,
        provider: row.get(14)?,
        depth: u8_column(row, 15)?,
        attempt_count: u32_column(row, 16)?,
        max_attempts: u32_column(row, 17)?,
        result: optional_json_column(row, 18)?,
        thread_id: row.get(19)?,
        updated_at_ms: row.get(20)?,
    })
}

fn row_to_attempt(row: &Row<'_>) -> rusqlite::Result<AttemptRecord> {
    Ok(AttemptRecord {
        id: row.get(0)?,
        run_id: row.get(1)?,
        node_id: row.get(2)?,
        number: u32_column(row, 3)?,
        generation: u64_column(row, 4)?,
        lease_epoch: u64_column(row, 5)?,
        status: enum_column(row, 6)?,
        is_write: row.get(7)?,
        started_at_ms: row.get(8)?,
        completed_at_ms: row.get(9)?,
        error: row.get(10)?,
    })
}

fn row_to_approval(row: &Row<'_>) -> rusqlite::Result<ApprovalRecord> {
    Ok(ApprovalRecord {
        id: row.get(0)?,
        run_id: row.get(1)?,
        node_id: row.get(2)?,
        status: enum_column(row, 3)?,
        prompt: row.get(4)?,
        response: optional_json_column(row, 5)?,
        requested_at_ms: row.get(6)?,
        resolved_at_ms: row.get(7)?,
    })
}

fn row_to_event(row: &Row<'_>) -> rusqlite::Result<WorkflowEvent> {
    Ok(WorkflowEvent {
        event_id: row.get(0)?,
        run_id: row.get(1)?,
        workflow_sequence: u64_column(row, 2)?,
        kind: row.get(3)?,
        payload: json_column(row, 4)?,
        created_at_ms: row.get(5)?,
    })
}

fn row_to_artifact(row: &Row<'_>) -> rusqlite::Result<ArtifactMetadata> {
    Ok(ArtifactMetadata {
        id: row.get(0)?,
        run_id: row.get(1)?,
        node_id: row.get(2)?,
        name: row.get(3)?,
        mime_type: row.get(4)?,
        size: u64_column(row, 5)?,
        storage_key: row.get(6)?,
        sha256: row.get(7)?,
        metadata: json_column(row, 8)?,
        created_at_ms: row.get(9)?,
    })
}

fn enum_column<T>(row: &Row<'_>, index: usize) -> rusqlite::Result<T>
where
    T: FromStr<Err = WorkflowError>,
{
    let value: String = row.get(index)?;
    value.parse().map_err(|error| decode_error(index, error))
}

fn json_column<T: DeserializeOwned>(row: &Row<'_>, index: usize) -> rusqlite::Result<T> {
    let value: String = row.get(index)?;
    serde_json::from_str(&value).map_err(|error| decode_error(index, error))
}

fn optional_json_column<T: DeserializeOwned>(
    row: &Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<T>> {
    let value: Option<String> = row.get(index)?;
    value
        .map(|value| serde_json::from_str(&value).map_err(|error| decode_error(index, error)))
        .transpose()
}

fn u64_column(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|error| decode_error(index, error))
}

fn u32_column(row: &Row<'_>, index: usize) -> rusqlite::Result<u32> {
    let value: i64 = row.get(index)?;
    u32::try_from(value).map_err(|error| decode_error(index, error))
}

fn u8_column(row: &Row<'_>, index: usize) -> rusqlite::Result<u8> {
    let value: i64 = row.get(index)?;
    u8::try_from(value).map_err(|error| decode_error(index, error))
}

fn usize_column(row: &Row<'_>, index: usize) -> rusqlite::Result<usize> {
    let value: i64 = row.get(index)?;
    usize::try_from(value).map_err(|error| decode_error(index, error))
}

fn decode_error(
    index: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(error))
}

fn ensure_changed(changed: usize, message: &str) -> WorkflowResult<()> {
    if changed == 1 {
        Ok(())
    } else {
        Err(WorkflowError::Conflict(message.to_string()))
    }
}

fn cas_conflict(
    entity: &str,
    expected_revision: u64,
    actual_revision: u64,
    expected_generation: u64,
    actual_generation: u64,
) -> WorkflowError {
    WorkflowError::Conflict(format!(
        "{entity} CAS failed: revision expected {expected_revision}, actual {actual_revision}; \
         generation expected {expected_generation}, actual {actual_generation}"
    ))
}

fn to_i64(value: u64, name: &str) -> WorkflowResult<i64> {
    i64::try_from(value)
        .map_err(|_| WorkflowError::InvalidRequest(format!("{name} exceeds SQLite integer range")))
}

fn clamp_limit(value: usize, maximum: usize) -> usize {
    value.clamp(1, maximum)
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

pub fn payload_hash<T: Serialize>(payload: &T) -> WorkflowResult<String> {
    let bytes = serde_json::to_vec(payload)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::workflow::domain::{PermissionSet, PolicySnapshot, RouteMode, WorkspaceRisk};
    use crate::workflow::scheduler::PlanCompiler;

    fn fixture() -> (
        tempfile::TempDir,
        Journal,
        RunRecord,
        WorkflowPlan,
        CommandEnvelope,
        EventDraft,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let journal = Journal::open(temp.path().join("workflow.sqlite")).unwrap();
        let policy = PolicySnapshot {
            version: 1,
            max_read_only_concurrency: 4,
            max_provider_concurrency: 2,
            max_repo_writers: 1,
            max_delegation_depth: 1,
            infrastructure_retry_limit: 3,
            builder_repair_limit: 2,
            permissions: PermissionSet {
                read_paths: ["repo"].into_iter().map(String::from).collect(),
                write_paths: ["repo"].into_iter().map(String::from).collect(),
                allowed_commands: BTreeSet::new(),
                network_hosts: BTreeSet::new(),
                can_request_approval: true,
            },
            workspace_risk: WorkspaceRisk::default(),
        };
        let plan = PlanCompiler::new(policy.clone())
            .compile(RouteMode::Direct)
            .unwrap();
        let now = now_ms();
        let run = RunRecord {
            id: "run-1".to_string(),
            status: RunStatus::Created,
            route: RouteMode::Direct,
            revision: 0,
            generation: 0,
            input: serde_json::json!({"task":"test"}),
            policy,
            final_delivery_committed: false,
            created_at_ms: now,
            updated_at_ms: now,
            error: None,
        };
        let command = CommandEnvelope {
            command_id: "command-1".to_string(),
            run_id: run.id.clone(),
            kind: "start".to_string(),
            payload_hash: payload_hash(&run.input).unwrap(),
        };
        let event = EventDraft {
            event_id: "event-1".to_string(),
            kind: "run.created".to_string(),
            payload: serde_json::json!({}),
            topic: "workflow.event".to_string(),
        };
        (temp, journal, run, plan, command, event)
    }

    #[test]
    fn duplicate_command_returns_the_durable_ack() {
        let (_temp, journal, run, plan, command, event) = fixture();
        let first = journal.create_run(&run, &plan, &command, &event).unwrap();
        assert!(!first.is_duplicate());
        let replay = journal.create_run(&run, &plan, &command, &event).unwrap();
        assert!(replay.is_duplicate());
        assert_eq!(
            first.into_inner().workflow_sequence,
            replay.into_inner().workflow_sequence
        );
    }

    #[test]
    fn summary_stats_are_loaded_for_all_requested_runs() {
        let (_temp, journal, run, plan, command, event) = fixture();
        journal.create_run(&run, &plan, &command, &event).unwrap();

        let stats = journal
            .run_summary_stats(std::slice::from_ref(&run.id))
            .unwrap();
        let summary = stats.get(&run.id).unwrap();
        assert_eq!(summary.latest_sequence, 1);
        assert_eq!(summary.total_nodes, 2);
        assert_eq!(summary.completed_nodes, 0);
        assert_eq!(summary.failed_nodes, 0);
        assert_eq!(summary.running_nodes, 0);
        assert_eq!(summary.ready_non_final_nodes, 1);
        assert_eq!(summary.attention_blockers, 0);
    }

    #[test]
    fn runs_can_be_scoped_to_their_origin_thread() {
        let (_temp, journal, mut first, plan, mut first_command, event) = fixture();
        first.input = serde_json::json!({
            "task": "first",
            "originThreadId": "thread-current",
        });
        first_command.payload_hash = payload_hash(&first.input).unwrap();
        journal
            .create_run(&first, &plan, &first_command, &event)
            .unwrap();

        let mut second = first.clone();
        second.id = "run-2".to_string();
        second.input = serde_json::json!({
            "task": "second",
            "originThreadId": "thread-other",
        });
        second.updated_at_ms += 1;
        let second_command = CommandEnvelope {
            command_id: "command-2".to_string(),
            run_id: second.id.clone(),
            kind: "start".to_string(),
            payload_hash: payload_hash(&second.input).unwrap(),
        };
        let second_event = EventDraft {
            event_id: "event-2".to_string(),
            ..event
        };
        journal
            .create_run(&second, &plan, &second_command, &second_event)
            .unwrap();

        let current = journal
            .list_runs_for_origin("thread-current", None, 10)
            .unwrap();
        assert_eq!(
            current
                .iter()
                .map(|run| run.id.as_str())
                .collect::<Vec<_>>(),
            vec!["run-1"]
        );
        assert!(
            journal
                .list_runs_for_origin("thread-missing", None, 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn command_id_with_different_payload_is_rejected() {
        let (_temp, journal, run, plan, command, event) = fixture();
        journal.create_run(&run, &plan, &command, &event).unwrap();
        let mut changed = command;
        changed.payload_hash = "different".to_string();
        assert!(matches!(
            journal.create_run(&run, &plan, &changed, &event),
            Err(WorkflowError::Conflict(_))
        ));
    }

    #[test]
    fn run_cas_and_final_delivery_gate_are_enforced() {
        let (_temp, journal, run, plan, command, event) = fixture();
        journal.create_run(&run, &plan, &command, &event).unwrap();
        let queued = journal
            .transition_run(
                &run.id,
                0,
                0,
                RunStatus::Queued,
                &EventDraft::new("run.queued", serde_json::json!({})),
            )
            .unwrap();
        assert_eq!(queued.workflow_sequence, 2);
        assert!(matches!(
            journal.transition_run(
                &run.id,
                0,
                0,
                RunStatus::Running,
                &EventDraft::new("run.running", serde_json::json!({})),
            ),
            Err(WorkflowError::Conflict(_))
        ));
        journal
            .transition_run(
                &run.id,
                1,
                0,
                RunStatus::Running,
                &EventDraft::new("run.running", serde_json::json!({})),
            )
            .unwrap();
        assert!(matches!(
            journal.transition_run(
                &run.id,
                2,
                0,
                RunStatus::Succeeded,
                &EventDraft::new("run.succeeded", serde_json::json!({})),
            ),
            Err(WorkflowError::Conflict(_))
        ));
    }

    #[test]
    fn events_replay_by_id_and_read_after_sequence() {
        let (_temp, journal, run, plan, command, event) = fixture();
        journal.create_run(&run, &plan, &command, &event).unwrap();
        let replay = journal.append_event(&run.id, &event).unwrap();
        assert_eq!(replay.workflow_sequence, 1);
        let second = EventDraft {
            event_id: "event-2".to_string(),
            kind: "message".to_string(),
            payload: serde_json::json!({"value": 2}),
            topic: "workflow.event".to_string(),
        };
        journal.append_event(&run.id, &second).unwrap();
        let after = journal.events_after(&run.id, 1, 10).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].workflow_sequence, 2);
        let mut conflicting = second;
        conflicting.payload = serde_json::json!({"value": 3});
        assert!(matches!(
            journal.append_event(&run.id, &conflicting),
            Err(WorkflowError::Conflict(_))
        ));
    }

    #[test]
    fn stale_lease_cannot_be_renewed_or_released() {
        let (_temp, journal, run, plan, command, event) = fixture();
        journal.create_run(&run, &plan, &command, &event).unwrap();
        let node = journal.get_node(&run.id, "builder").unwrap();
        let lease = journal
            .acquire_lease(
                &run.id,
                &node.id,
                "worker-a",
                node.generation,
                node.lease_epoch,
                Duration::from_secs(30),
            )
            .unwrap();
        let mut stale = lease.clone();
        stale.epoch -= 1;
        assert!(
            journal
                .renew_lease(&stale, Duration::from_secs(30))
                .is_err()
        );
        assert!(journal.release_lease(&stale).is_err());
        journal.release_lease(&lease).unwrap();
    }

    #[test]
    fn retry_command_requeues_needs_attention_in_the_same_transaction() {
        let (_temp, journal, run, plan, command, event) = fixture();
        journal.create_run(&run, &plan, &command, &event).unwrap();
        journal
            .transition_run(
                &run.id,
                0,
                0,
                RunStatus::Running,
                &EventDraft::new("run.running", serde_json::json!({})),
            )
            .unwrap();
        let builder = journal.get_node(&run.id, "builder").unwrap();
        let lease = journal
            .acquire_lease(
                &run.id,
                &builder.id,
                "worker",
                builder.generation,
                builder.lease_epoch,
                Duration::from_secs(30),
            )
            .unwrap();
        journal.start_attempt(&lease, true).unwrap();
        let running = journal.get_node(&run.id, "builder").unwrap();
        journal
            .finish_attempt_and_node(
                &run.id,
                &running.id,
                &NodeCas {
                    expected_revision: running.revision,
                    expected_generation: running.generation,
                    expected_lease_epoch: running.lease_epoch,
                    expected_payload_hash: running.payload_hash.clone(),
                },
                NodeStatus::Failed,
                None,
                Some("validation failed"),
                &EventDraft::new("node.failed", serde_json::json!({})),
            )
            .unwrap();
        let active = journal.get_run(&run.id).unwrap();
        journal
            .transition_run_with_error(
                &run.id,
                active.revision,
                active.generation,
                RunStatus::NeedsAttention,
                "builder validation failed",
                &EventDraft::new("run.needs_attention", serde_json::json!({})),
            )
            .unwrap();
        assert_eq!(
            journal.get_run(&run.id).unwrap().error.as_deref(),
            Some("builder validation failed")
        );
        let failed = journal.get_node(&run.id, "builder").unwrap();
        let retry_command = CommandEnvelope {
            command_id: "retry-builder".to_string(),
            run_id: run.id.clone(),
            kind: "retry".to_string(),
            payload_hash: payload_hash(&serde_json::json!({"nodeId": "builder"})).unwrap(),
        };
        let cas = NodeCas {
            expected_revision: failed.revision,
            expected_generation: failed.generation,
            expected_lease_epoch: failed.lease_epoch,
            expected_payload_hash: failed.payload_hash.clone(),
        };
        let new_hash = payload_hash(&serde_json::json!({"retry": 1})).unwrap();
        let retry_event = EventDraft::new(
            "node.retry_scheduled",
            serde_json::json!({"nodeId": "builder"}),
        );

        let first = journal
            .reset_node_for_retry_command(
                &retry_command,
                &run.id,
                "builder",
                &cas,
                &new_hash,
                &retry_event,
            )
            .unwrap();
        let queued_revision = journal.get_run(&run.id).unwrap().revision;
        let duplicate = journal
            .reset_node_for_retry_command(
                &retry_command,
                &run.id,
                "builder",
                &cas,
                &new_hash,
                &retry_event,
            )
            .unwrap();

        assert!(!first.is_duplicate());
        assert!(duplicate.is_duplicate());
        assert_eq!(
            journal.get_node(&run.id, "builder").unwrap().status,
            NodeStatus::Ready
        );
        assert_eq!(journal.get_run(&run.id).unwrap().status, RunStatus::Queued);
        assert!(journal.get_run(&run.id).unwrap().error.is_none());
        assert_eq!(journal.get_run(&run.id).unwrap().revision, queued_revision);
    }

    #[test]
    fn needs_attention_reason_is_persisted_and_cleared_on_resume() {
        let (_temp, journal, run, plan, command, event) = fixture();
        journal.create_run(&run, &plan, &command, &event).unwrap();
        journal
            .transition_run(
                &run.id,
                0,
                0,
                RunStatus::Running,
                &EventDraft::new("run.running", serde_json::json!({})),
            )
            .unwrap();
        let running = journal.get_run(&run.id).unwrap();
        journal
            .transition_run_with_error(
                &run.id,
                running.revision,
                running.generation,
                RunStatus::NeedsAttention,
                "manual review required",
                &EventDraft::new("run.needs_attention", serde_json::json!({})),
            )
            .unwrap();
        let attention = journal.get_run(&run.id).unwrap();
        assert_eq!(attention.error.as_deref(), Some("manual review required"));

        let resume = CommandEnvelope {
            command_id: "resume-run".to_string(),
            run_id: run.id.clone(),
            kind: "resume".to_string(),
            payload_hash: payload_hash(&serde_json::json!({})).unwrap(),
        };
        journal
            .transition_run_command(
                &resume,
                attention.revision,
                attention.generation,
                RunStatus::Running,
                &EventDraft::new("run.resumed", serde_json::json!({})),
            )
            .unwrap();
        assert!(journal.get_run(&run.id).unwrap().error.is_none());
    }

    #[test]
    fn builder_repair_invalidates_downstream_results_atomically() {
        let (_temp, journal, run, plan, command, event) = fixture();
        journal.create_run(&run, &plan, &command, &event).unwrap();
        journal
            .transition_run(
                &run.id,
                0,
                0,
                RunStatus::Running,
                &EventDraft::new("run.running", serde_json::json!({})),
            )
            .unwrap();
        let builder = journal.get_node(&run.id, "builder").unwrap();
        let lease = journal
            .acquire_lease(
                &run.id,
                &builder.id,
                "worker",
                builder.generation,
                builder.lease_epoch,
                Duration::from_secs(30),
            )
            .unwrap();
        journal.start_attempt(&lease, true).unwrap();
        let running = journal.get_node(&run.id, "builder").unwrap();
        journal
            .finish_attempt_and_node(
                &run.id,
                &running.id,
                &NodeCas {
                    expected_revision: running.revision,
                    expected_generation: running.generation,
                    expected_lease_epoch: running.lease_epoch,
                    expected_payload_hash: running.payload_hash.clone(),
                },
                NodeStatus::Succeeded,
                Some(&serde_json::json!({"text": "first build"})),
                None,
                &EventDraft::new("node.completed", serde_json::json!({})),
            )
            .unwrap();
        journal.promote_ready_nodes(&run.id).unwrap();

        let succeeded_builder = journal.get_node(&run.id, "builder").unwrap();
        let ready_final = journal.get_node(&run.id, "final_delivery").unwrap();
        journal
            .schedule_builder_repair(
                &run.id,
                &succeeded_builder,
                std::slice::from_ref(&ready_final),
                1,
                "add a regression test",
                &EventDraft::new("run.repair_requested", serde_json::json!({"cycle": 1})),
            )
            .unwrap();

        let repaired_builder = journal.get_node(&run.id, "builder").unwrap();
        let invalidated_final = journal.get_node(&run.id, "final_delivery").unwrap();
        assert_eq!(repaired_builder.status, NodeStatus::Ready);
        assert_eq!(
            repaired_builder.generation,
            succeeded_builder.generation + 1
        );
        assert!(repaired_builder.result.is_none());
        assert_eq!(invalidated_final.status, NodeStatus::Pending);
        assert_eq!(invalidated_final.generation, ready_final.generation + 1);
    }

    #[test]
    fn sqlite_durability_pragmas_and_schema_are_applied() {
        let (_temp, journal, ..) = fixture();
        let connection = journal.connect().unwrap();
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let foreign_keys: i64 = connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        let synchronous: i64 = connection
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        let busy_timeout: i64 = connection
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "wal");
        assert_eq!(foreign_keys, 1);
        assert_eq!(synchronous, 2);
        assert_eq!(busy_timeout, 5_000);
        assert_eq!(journal.schema_version().unwrap(), SCHEMA_VERSION);
    }
}
