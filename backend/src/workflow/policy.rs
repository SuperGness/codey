use serde::{Deserialize, Serialize};

use super::domain::{
    PermissionSet, PolicySnapshot, WorkflowError, WorkflowResult, WorkspaceRisk, WorkspaceRiskLevel,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPolicy {
    pub version: u32,
    pub max_read_only_concurrency: u16,
    pub max_provider_concurrency: u16,
    pub max_repo_writers: u16,
    pub max_delegation_depth: u8,
    pub infrastructure_retry_limit: u8,
    pub builder_repair_limit: u8,
    pub permissions: PermissionSet,
}

impl Default for WorkflowPolicy {
    fn default() -> Self {
        Self {
            version: 1,
            max_read_only_concurrency: 4,
            max_provider_concurrency: 2,
            max_repo_writers: 1,
            max_delegation_depth: 1,
            infrastructure_retry_limit: 3,
            builder_repair_limit: 2,
            permissions: PermissionSet::default(),
        }
    }
}

impl WorkflowPolicy {
    pub fn validate(&self) -> WorkflowResult<()> {
        if self.max_read_only_concurrency == 0 {
            return Err(WorkflowError::InvalidRequest(
                "read-only concurrency must be at least one".to_string(),
            ));
        }
        if self.max_provider_concurrency == 0 {
            return Err(WorkflowError::InvalidRequest(
                "provider concurrency must be at least one".to_string(),
            ));
        }
        if self.max_repo_writers == 0 {
            return Err(WorkflowError::InvalidRequest(
                "repo writer concurrency must be at least one".to_string(),
            ));
        }
        Ok(())
    }

    pub fn snapshot(
        &self,
        requested_permissions: &PermissionSet,
        workspace_risk: WorkspaceRisk,
    ) -> WorkflowResult<PolicySnapshot> {
        self.validate()?;
        Ok(PolicySnapshot {
            version: self.version,
            max_read_only_concurrency: self.max_read_only_concurrency,
            max_provider_concurrency: self.max_provider_concurrency,
            max_repo_writers: self.max_repo_writers,
            max_delegation_depth: self.max_delegation_depth,
            infrastructure_retry_limit: self.infrastructure_retry_limit,
            builder_repair_limit: self.builder_repair_limit,
            permissions: if self.permissions == PermissionSet::default() {
                requested_permissions.clone()
            } else {
                self.permissions.intersect(requested_permissions)
            },
            workspace_risk,
        })
    }

    pub fn workspace_decision(
        &self,
        risk: &WorkspaceRisk,
        repo_writer: bool,
        isolated_workspace_available: bool,
    ) -> WorkspaceDecision {
        if !repo_writer {
            return WorkspaceDecision::InPlace;
        }
        match risk.level {
            WorkspaceRiskLevel::Clean => WorkspaceDecision::InPlace,
            WorkspaceRiskLevel::Dirty if isolated_workspace_available => WorkspaceDecision::Isolate,
            WorkspaceRiskLevel::Dirty => WorkspaceDecision::RequireApproval,
            WorkspaceRiskLevel::HighRisk if isolated_workspace_available => {
                WorkspaceDecision::Isolate
            }
            WorkspaceRiskLevel::HighRisk => WorkspaceDecision::Reject,
        }
    }

    pub fn retry_decision(
        &self,
        class: RetryClass,
        attempts_already_started: u32,
        write_outcome_known: bool,
    ) -> RetryDecision {
        if class == RetryClass::WriteUnknownOutcome || !write_outcome_known {
            return RetryDecision::NeedsAttention;
        }
        let limit = match class {
            RetryClass::Infrastructure => self.infrastructure_retry_limit,
            RetryClass::BuilderRepair => self.builder_repair_limit,
            RetryClass::Logical | RetryClass::User => 0,
            RetryClass::WriteUnknownOutcome => return RetryDecision::NeedsAttention,
        } as u32;
        // This method is called after an attempt has failed. A configured
        // limit of N therefore permits retries after attempts 1 through N,
        // for at most 1 + N total attempts.
        if attempts_already_started <= limit && limit > 0 {
            RetryDecision::Retry
        } else {
            RetryDecision::Exhausted
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceDecision {
    InPlace,
    Isolate,
    RequireApproval,
    Reject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryClass {
    Infrastructure,
    BuilderRepair,
    Logical,
    User,
    WriteUnknownOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryDecision {
    Retry,
    Exhausted,
    NeedsAttention,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_limits_match_the_contract() {
        let policy = WorkflowPolicy::default();
        assert_eq!(policy.max_read_only_concurrency, 4);
        assert_eq!(policy.max_provider_concurrency, 2);
        assert_eq!(policy.max_repo_writers, 1);
        assert_eq!(policy.max_delegation_depth, 1);
        assert_eq!(policy.infrastructure_retry_limit, 3);
        assert_eq!(policy.builder_repair_limit, 2);
    }

    #[test]
    fn unknown_write_is_never_replayed_automatically() {
        let policy = WorkflowPolicy::default();
        assert_eq!(
            policy.retry_decision(RetryClass::Infrastructure, 0, false),
            RetryDecision::NeedsAttention
        );
        assert_eq!(
            policy.retry_decision(RetryClass::WriteUnknownOutcome, 0, true),
            RetryDecision::NeedsAttention
        );
    }

    #[test]
    fn infrastructure_limit_counts_retries_not_total_attempts() {
        let policy = WorkflowPolicy::default();
        assert_eq!(
            policy.retry_decision(RetryClass::Infrastructure, 3, true),
            RetryDecision::Retry
        );
        assert_eq!(
            policy.retry_decision(RetryClass::Infrastructure, 4, true),
            RetryDecision::Exhausted
        );
    }

    #[test]
    fn dirty_writer_requires_isolation_or_approval() {
        let policy = WorkflowPolicy::default();
        let risk = WorkspaceRisk {
            level: WorkspaceRiskLevel::Dirty,
            dirty_paths: vec!["src/lib.rs".to_string()],
            reason: None,
        };
        assert_eq!(
            policy.workspace_decision(&risk, true, true),
            WorkspaceDecision::Isolate
        );
        assert_eq!(
            policy.workspace_decision(&risk, true, false),
            WorkspaceDecision::RequireApproval
        );
    }
}
