#[cfg(test)]
use std::collections::{HashMap, VecDeque};
#[cfg(test)]
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(test)]
use super::domain::WorkflowError;
use super::domain::{NodeId, NodeRole, PermissionSet, RunId, WorkflowResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartNodeRequest {
    pub run_id: RunId,
    pub node_id: NodeId,
    pub role: NodeRole,
    pub prompt: Value,
    #[serde(default)]
    pub existing_thread_id: Option<String>,
    pub cwd: String,
    pub approval_policy: Value,
    pub sandbox_mode: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    pub permissions: PermissionSet,
    pub idempotency_key: String,
    pub repo_writer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartedNode {
    pub thread_id: String,
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SteerThreadRequest {
    pub run_id: RunId,
    pub thread_id: String,
    pub message: Value,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileOutcome {
    Running,
    WaitingApproval,
    Succeeded(Value),
    Failed,
    NotFound,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalDeliveryRequest {
    pub run_id: RunId,
    pub origin_thread_id: String,
    pub content: Value,
    pub idempotency_key: String,
}

/// Boundary around the Codex app-server. Every operation carries an idempotency
/// key because a journaled command or final delivery can be replayed after a
/// process crash. Implementations must not assume exactly-once transport.
#[async_trait]
pub trait AppServerAdapter: Send + Sync {
    async fn start_node(&self, request: StartNodeRequest) -> WorkflowResult<StartedNode>;

    async fn steer_thread(&self, request: SteerThreadRequest) -> WorkflowResult<()>;

    async fn interrupt_thread(
        &self,
        run_id: &str,
        thread_id: &str,
        idempotency_key: &str,
    ) -> WorkflowResult<()>;

    async fn reconcile_thread(
        &self,
        run_id: &str,
        node_id: &str,
        thread_id: &str,
    ) -> WorkflowResult<ReconcileOutcome>;

    async fn deliver_final(&self, request: FinalDeliveryRequest) -> WorkflowResult<()>;
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub enum FakeCall {
    Start(Box<StartNodeRequest>),
    Steer(SteerThreadRequest),
    Interrupt {
        run_id: String,
        thread_id: String,
        idempotency_key: String,
    },
    Reconcile {
        run_id: String,
        node_id: String,
        thread_id: String,
    },
    Final(FinalDeliveryRequest),
}

#[cfg(test)]
#[derive(Debug, Default)]
struct FakeState {
    calls: Vec<FakeCall>,
    started: HashMap<String, StartedNode>,
    started_overrides: VecDeque<StartedNode>,
    reconcile: HashMap<String, VecDeque<ReconcileOutcome>>,
    fail_next: VecDeque<String>,
}

/// Deterministic adapter for workflow integration tests.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct FakeAppServerAdapter {
    state: Mutex<FakeState>,
}

#[cfg(test)]
impl FakeAppServerAdapter {
    pub fn calls(&self) -> Vec<FakeCall> {
        self.state
            .lock()
            .expect("fake adapter mutex poisoned")
            .calls
            .clone()
    }

    pub fn queue_reconcile(&self, thread_id: impl Into<String>, outcome: ReconcileOutcome) {
        self.state
            .lock()
            .expect("fake adapter mutex poisoned")
            .reconcile
            .entry(thread_id.into())
            .or_default()
            .push_back(outcome);
    }

    pub fn queue_started(&self, started: StartedNode) {
        self.state
            .lock()
            .expect("fake adapter mutex poisoned")
            .started_overrides
            .push_back(started);
    }

    pub fn fail_next(&self, message: impl Into<String>) {
        self.state
            .lock()
            .expect("fake adapter mutex poisoned")
            .fail_next
            .push_back(message.into());
    }

    fn take_failure(state: &mut FakeState) -> WorkflowResult<()> {
        match state.fail_next.pop_front() {
            Some(message) => Err(WorkflowError::Adapter(message)),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
#[async_trait]
impl AppServerAdapter for FakeAppServerAdapter {
    async fn start_node(&self, request: StartNodeRequest) -> WorkflowResult<StartedNode> {
        let mut state = self.state.lock().expect("fake adapter mutex poisoned");
        Self::take_failure(&mut state)?;
        state.calls.push(FakeCall::Start(Box::new(request.clone())));
        if let Some(started) = state.started.get(&request.idempotency_key) {
            return Ok(started.clone());
        }
        let started = state
            .started_overrides
            .pop_front()
            .unwrap_or_else(|| StartedNode {
                thread_id: format!("thread-{}", request.node_id),
                turn_id: Some(format!("turn-{}", request.node_id)),
            });
        state
            .started
            .insert(request.idempotency_key, started.clone());
        Ok(started)
    }

    async fn steer_thread(&self, request: SteerThreadRequest) -> WorkflowResult<()> {
        let mut state = self.state.lock().expect("fake adapter mutex poisoned");
        Self::take_failure(&mut state)?;
        state.calls.push(FakeCall::Steer(request));
        Ok(())
    }

    async fn interrupt_thread(
        &self,
        run_id: &str,
        thread_id: &str,
        idempotency_key: &str,
    ) -> WorkflowResult<()> {
        let mut state = self.state.lock().expect("fake adapter mutex poisoned");
        Self::take_failure(&mut state)?;
        state.calls.push(FakeCall::Interrupt {
            run_id: run_id.to_string(),
            thread_id: thread_id.to_string(),
            idempotency_key: idempotency_key.to_string(),
        });
        Ok(())
    }

    async fn reconcile_thread(
        &self,
        run_id: &str,
        node_id: &str,
        thread_id: &str,
    ) -> WorkflowResult<ReconcileOutcome> {
        let mut state = self.state.lock().expect("fake adapter mutex poisoned");
        Self::take_failure(&mut state)?;
        state.calls.push(FakeCall::Reconcile {
            run_id: run_id.to_string(),
            node_id: node_id.to_string(),
            thread_id: thread_id.to_string(),
        });
        Ok(state
            .reconcile
            .get_mut(thread_id)
            .and_then(VecDeque::pop_front)
            .unwrap_or(ReconcileOutcome::Unknown))
    }

    async fn deliver_final(&self, request: FinalDeliveryRequest) -> WorkflowResult<()> {
        let mut state = self.state.lock().expect("fake adapter mutex poisoned");
        Self::take_failure(&mut state)?;
        state.calls.push(FakeCall::Final(request));
        Ok(())
    }
}
