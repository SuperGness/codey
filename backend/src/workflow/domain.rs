use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type RunId = String;
pub type NodeId = String;
pub type EventId = String;
pub type CommandId = String;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowError {
    Unavailable(String),
    NotFound {
        entity: &'static str,
        id: String,
    },
    Conflict(String),
    InvalidTransition {
        entity: &'static str,
        from: String,
        to: String,
    },
    InvalidRequest(String),
    Storage(String),
    Adapter(String),
    Join(String),
}

impl fmt::Display for WorkflowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(reason) => {
                write!(formatter, "workflow service unavailable: {reason}")
            }
            Self::NotFound { entity, id } => write!(formatter, "{entity} not found: {id}"),
            Self::Conflict(message) => write!(formatter, "workflow conflict: {message}"),
            Self::InvalidTransition { entity, from, to } => {
                write!(formatter, "invalid {entity} transition: {from} -> {to}")
            }
            Self::InvalidRequest(message) => {
                write!(formatter, "invalid workflow request: {message}")
            }
            Self::Storage(message) => write!(formatter, "workflow storage error: {message}"),
            Self::Adapter(message) => write!(formatter, "app-server error: {message}"),
            Self::Join(message) => write!(formatter, "workflow worker failed: {message}"),
        }
    }
}

impl std::error::Error for WorkflowError {}

impl From<rusqlite::Error> for WorkflowError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

impl From<serde_json::Error> for WorkflowError {
    fn from(error: serde_json::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

impl From<std::io::Error> for WorkflowError {
    fn from(error: std::io::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

pub type WorkflowResult<T> = Result<T, WorkflowError>;

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = WorkflowError;

            fn from_str(value: &str) -> WorkflowResult<Self> {
                match value {
                    $($text => Ok(Self::$variant),)+
                    other => Err(WorkflowError::Storage(format!(
                        "unknown {} value: {other}", stringify!($name)
                    ))),
                }
            }
        }
    };
}

string_enum!(RunStatus {
    Created => "created",
    Queued => "queued",
    Running => "running",
    Recovering => "recovering",
    Pausing => "pausing",
    Paused => "paused",
    Canceling => "canceling",
    Canceled => "canceled",
    Succeeded => "succeeded",
    Failed => "failed",
    NeedsAttention => "needs_attention",
});

impl RunStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Canceled | Self::Succeeded | Self::Failed)
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        use RunStatus::*;
        if self == next {
            return false;
        }
        matches!(
            (self, next),
            (
                Created,
                Queued | Running | Recovering | Pausing | Canceling | Canceled | Failed
            ) | (
                Queued,
                Running | Pausing | Canceling | Failed | NeedsAttention | Recovering
            ) | (
                Running,
                Pausing | Canceling | Succeeded | Failed | NeedsAttention | Recovering
            ) | (
                Recovering,
                Queued
                    | Running
                    | Pausing
                    | Paused
                    | Canceling
                    | Canceled
                    | Failed
                    | NeedsAttention
            ) | (
                Pausing,
                Paused | Recovering | Canceling | Failed | NeedsAttention
            ) | (Paused, Queued | Running | Canceling | Recovering | Failed)
                | (Canceling, Recovering | Canceled | Failed | NeedsAttention)
                | (Failed, Queued | Recovering)
                | (
                    NeedsAttention,
                    Queued | Running | Recovering | Pausing | Paused | Canceling | Failed
                )
        )
    }

    pub fn validate_transition(self, next: Self) -> WorkflowResult<()> {
        if self.can_transition_to(next) {
            Ok(())
        } else {
            Err(WorkflowError::InvalidTransition {
                entity: "run",
                from: self.to_string(),
                to: next.to_string(),
            })
        }
    }
}

string_enum!(NodeStatus {
    Pending => "pending",
    Ready => "ready",
    Leased => "leased",
    Running => "running",
    WaitingApproval => "waiting_approval",
    Skipped => "skipped",
    UnknownOutcome => "unknown_outcome",
    Compensating => "compensating",
    Compensated => "compensated",
    Succeeded => "succeeded",
    Failed => "failed",
    Canceled => "canceled",
});

impl NodeStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Skipped | Self::Compensated | Self::Succeeded | Self::Failed | Self::Canceled
        )
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        use NodeStatus::*;
        if self == next {
            return false;
        }
        matches!(
            (self, next),
            (Pending, Ready | Skipped | Canceled)
                | (Ready, Leased | Skipped | Canceled)
                | (Leased, Ready | Running | UnknownOutcome | Failed | Canceled)
                | (
                    Running,
                    WaitingApproval | Succeeded | Failed | UnknownOutcome | Canceled
                )
                | (
                    WaitingApproval,
                    Running | Succeeded | Failed | Canceled | UnknownOutcome
                )
                | (
                    UnknownOutcome,
                    Ready | Compensating | Succeeded | Failed | Canceled
                )
                | (Compensating, Compensated | Failed | UnknownOutcome)
                | (Failed, Ready | Compensating)
        )
    }

    pub fn validate_transition(self, next: Self) -> WorkflowResult<()> {
        if self.can_transition_to(next) {
            Ok(())
        } else {
            Err(WorkflowError::InvalidTransition {
                entity: "node",
                from: self.to_string(),
                to: next.to_string(),
            })
        }
    }
}

string_enum!(RouteMode {
    Direct => "direct",
    Guarded => "guarded",
    Parallel => "parallel",
    Expert => "expert",
});

string_enum!(NodeRole {
    Planner => "planner",
    Preflight => "preflight",
    Researcher => "researcher",
    Scout => "scout",
    Builder => "builder",
    Validator => "validator",
    Reviewer => "reviewer",
    Expert => "expert",
    Integrator => "integrator",
    FinalDelivery => "final_delivery",
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewerVerdict {
    Pass { delivery: String },
    ChangesRequired { feedback: String },
    Inconclusive { feedback: String },
}

/// Reviewer output is deliberately machine-gated. A successful worker turn is
/// not itself an approval: the first non-empty line must contain an explicit
/// verdict, and PASS must include a user-ready delivery body.
pub fn parse_reviewer_verdict(value: &Value) -> ReviewerVerdict {
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| value.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string());
    let trimmed = text.trim();
    let mut lines = trimmed.lines();
    let verdict = lines.next().unwrap_or_default().trim().to_ascii_uppercase();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    match verdict.as_str() {
        "PASS" if !body.is_empty() => ReviewerVerdict::Pass { delivery: body },
        "PASS" => ReviewerVerdict::Inconclusive {
            feedback: "reviewer returned PASS without a final delivery".to_string(),
        },
        "CHANGES_REQUIRED" | "REJECTED" => ReviewerVerdict::ChangesRequired {
            feedback: if body.is_empty() {
                "reviewer requested changes without actionable feedback".to_string()
            } else {
                body
            },
        },
        "INCONCLUSIVE" => ReviewerVerdict::Inconclusive {
            feedback: if body.is_empty() {
                "reviewer could not reach a conclusion".to_string()
            } else {
                body
            },
        },
        _ => ReviewerVerdict::Inconclusive {
            feedback: "reviewer output omitted the required verdict line".to_string(),
        },
    }
}

string_enum!(WorkspaceRiskLevel {
    Clean => "clean",
    Dirty => "dirty",
    HighRisk => "high_risk",
});

string_enum!(AttemptStatus {
    Leased => "leased",
    Running => "running",
    Succeeded => "succeeded",
    Failed => "failed",
    UnknownOutcome => "unknown_outcome",
    Canceled => "canceled",
});

string_enum!(ApprovalStatus {
    Pending => "pending",
    Approved => "approved",
    Rejected => "rejected",
    Canceled => "canceled",
});

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionSet {
    #[serde(default)]
    pub read_paths: BTreeSet<String>,
    #[serde(default)]
    pub write_paths: BTreeSet<String>,
    #[serde(default)]
    pub allowed_commands: BTreeSet<String>,
    #[serde(default)]
    pub network_hosts: BTreeSet<String>,
    #[serde(default)]
    pub can_request_approval: bool,
}

impl PermissionSet {
    /// Computes the effective permission set. Every dimension is an
    /// intersection, so a child or policy snapshot can never broaden access.
    pub fn intersect(&self, restriction: &Self) -> Self {
        Self {
            read_paths: intersection(&self.read_paths, &restriction.read_paths),
            write_paths: intersection(&self.write_paths, &restriction.write_paths),
            allowed_commands: intersection(&self.allowed_commands, &restriction.allowed_commands),
            network_hosts: intersection(&self.network_hosts, &restriction.network_hosts),
            can_request_approval: self.can_request_approval && restriction.can_request_approval,
        }
    }

    pub fn is_read_only(&self) -> bool {
        self.write_paths.is_empty()
    }
}

fn intersection(left: &BTreeSet<String>, right: &BTreeSet<String>) -> BTreeSet<String> {
    left.intersection(right).cloned().collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRisk {
    pub level: WorkspaceRiskLevel,
    #[serde(default)]
    pub dirty_paths: Vec<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

impl Default for WorkspaceRisk {
    fn default() -> Self {
        Self {
            level: WorkspaceRiskLevel::Clean,
            dirty_paths: Vec::new(),
            reason: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicySnapshot {
    pub version: u32,
    pub max_read_only_concurrency: u16,
    pub max_provider_concurrency: u16,
    pub max_repo_writers: u16,
    pub max_delegation_depth: u8,
    pub infrastructure_retry_limit: u8,
    pub builder_repair_limit: u8,
    pub permissions: PermissionSet,
    pub workspace_risk: WorkspaceRisk,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub id: RunId,
    pub status: RunStatus,
    pub route: RouteMode,
    pub revision: u64,
    pub generation: u64,
    pub input: Value,
    pub policy: PolicySnapshot,
    pub final_delivery_committed: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRecord {
    pub run_id: RunId,
    pub id: NodeId,
    pub role: NodeRole,
    pub label: String,
    pub status: NodeStatus,
    pub revision: u64,
    pub generation: u64,
    pub lease_epoch: u64,
    pub lease_owner: Option<String>,
    pub lease_expires_at_ms: Option<i64>,
    pub payload_hash: String,
    pub dependencies: Vec<NodeId>,
    pub permissions: PermissionSet,
    pub repo_writer: bool,
    pub provider: String,
    pub depth: u8,
    pub attempt_count: u32,
    pub max_attempts: u32,
    pub result: Option<Value>,
    pub thread_id: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptRecord {
    pub id: String,
    pub run_id: RunId,
    pub node_id: NodeId,
    pub number: u32,
    pub generation: u64,
    pub lease_epoch: u64,
    pub status: AttemptStatus,
    pub is_write: bool,
    pub started_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEvent {
    pub event_id: EventId,
    pub run_id: RunId,
    pub workflow_sequence: u64,
    pub kind: String,
    pub payload: Value,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandEnvelope {
    pub command_id: CommandId,
    pub run_id: RunId,
    pub kind: String,
    pub payload_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lease {
    pub run_id: RunId,
    pub node_id: NodeId,
    pub owner: String,
    pub generation: u64,
    pub epoch: u64,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeCas {
    pub expected_revision: u64,
    pub expected_generation: u64,
    pub expected_lease_epoch: u64,
    pub expected_payload_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactMetadata {
    pub id: String,
    pub run_id: RunId,
    pub node_id: Option<NodeId>,
    pub name: String,
    pub mime_type: String,
    pub size: u64,
    pub storage_key: String,
    pub sha256: String,
    pub metadata: Value,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactPage {
    pub items: Vec<ArtifactMetadata>,
    pub next_after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRecord {
    pub id: String,
    pub run_id: RunId,
    pub node_id: NodeId,
    pub status: ApprovalStatus,
    pub prompt: String,
    pub response: Option<Value>,
    pub requested_at_ms: i64,
    pub resolved_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableAck {
    pub command_id: CommandId,
    pub run_id: RunId,
    pub workflow_sequence: u64,
    pub duplicate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCommand {
    pub command_id: CommandId,
    pub payload_hash: String,
    pub response_json: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_transitions_are_strict() {
        assert!(RunStatus::Created.can_transition_to(RunStatus::Queued));
        assert!(RunStatus::Running.can_transition_to(RunStatus::Pausing));
        assert!(RunStatus::Recovering.can_transition_to(RunStatus::Canceled));
        assert!(!RunStatus::Succeeded.can_transition_to(RunStatus::Running));
        assert!(!RunStatus::Running.can_transition_to(RunStatus::Running));
    }

    #[test]
    fn node_transitions_are_strict() {
        assert!(NodeStatus::Ready.can_transition_to(NodeStatus::Leased));
        assert!(NodeStatus::UnknownOutcome.can_transition_to(NodeStatus::Compensating));
        assert!(!NodeStatus::Succeeded.can_transition_to(NodeStatus::Ready));
    }

    #[test]
    fn permission_intersection_cannot_broaden() {
        let parent = PermissionSet {
            read_paths: ["repo", "docs"].into_iter().map(String::from).collect(),
            write_paths: ["repo"].into_iter().map(String::from).collect(),
            allowed_commands: ["cargo"].into_iter().map(String::from).collect(),
            network_hosts: ["api.openai.com"].into_iter().map(String::from).collect(),
            can_request_approval: true,
        };
        let child = PermissionSet {
            read_paths: ["repo", "secret"].into_iter().map(String::from).collect(),
            write_paths: ["repo", "outside"].into_iter().map(String::from).collect(),
            allowed_commands: ["cargo", "curl"].into_iter().map(String::from).collect(),
            network_hosts: ["example.com"].into_iter().map(String::from).collect(),
            can_request_approval: false,
        };
        let effective = parent.intersect(&child);
        assert_eq!(
            effective.read_paths,
            ["repo"].into_iter().map(String::from).collect()
        );
        assert_eq!(
            effective.write_paths,
            ["repo"].into_iter().map(String::from).collect()
        );
        assert!(effective.network_hosts.is_empty());
        assert!(!effective.can_request_approval);
    }

    #[test]
    fn reviewer_verdict_fails_closed() {
        assert_eq!(
            parse_reviewer_verdict(&serde_json::json!({"text": "PASS\nReady to ship."})),
            ReviewerVerdict::Pass {
                delivery: "Ready to ship.".to_string()
            }
        );
        assert!(matches!(
            parse_reviewer_verdict(&serde_json::json!({"text": "Looks good"})),
            ReviewerVerdict::Inconclusive { .. }
        ));
        assert!(matches!(
            parse_reviewer_verdict(&serde_json::json!({"text": "PASS"})),
            ReviewerVerdict::Inconclusive { .. }
        ));
        assert!(matches!(
            parse_reviewer_verdict(
                &serde_json::json!({"text": "CHANGES_REQUIRED\nAdd a regression test."})
            ),
            ReviewerVerdict::ChangesRequired { .. }
        ));
    }
}
