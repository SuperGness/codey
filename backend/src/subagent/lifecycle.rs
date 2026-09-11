//! Deterministic lifecycle state machine shared by scheduling and recovery.

use serde::{Deserialize, Serialize};

/// Persisted execution phases. `Failed` is retained only so schema v1-v4
/// ledgers can be deserialized before migration; new records use `Terminal`
/// plus an independent [`ExecutionOutcome`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecutionPhase {
    Pending,
    Running,
    Terminal,
    Failed,
    Recovered,
}

/// Persisted result of an execution attempt. A terminal phase is deliberately
/// not equivalent to success: lifecycle stop notifications and incomplete
/// status snapshots settle an attempt with `Unknown` or `Lost` instead.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecutionOutcome {
    #[default]
    Unknown,
    Succeeded,
    Failed,
    TimedOut,
    Lost,
}

impl From<super::protocol::TerminalOutcome> for ExecutionOutcome {
    fn from(outcome: super::protocol::TerminalOutcome) -> Self {
        use super::protocol::TerminalOutcome;
        match outcome {
            TerminalOutcome::Succeeded => Self::Succeeded,
            TerminalOutcome::Failed => Self::Failed,
            TerminalOutcome::TimedOut => Self::TimedOut,
            TerminalOutcome::Lost => Self::Lost,
        }
    }
}

impl ExecutionOutcome {
    /// Trace error code for a non-successful terminal outcome.
    pub(crate) fn failure_error_code(self) -> &'static str {
        match self {
            Self::Failed => "agent_failed",
            Self::TimedOut => "agent_timed_out",
            Self::Lost => "agent_lost",
            Self::Unknown => "unknown_terminal_outcome",
            Self::Succeeded => unreachable!("successful outcomes carry no error code"),
        }
    }
}

impl ExecutionOutcome {
    pub(crate) fn is_success(self) -> bool {
        self == Self::Succeeded
    }
}

impl ExecutionPhase {
    pub(crate) fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }

    pub(crate) fn is_settled(self) -> bool {
        !self.is_active()
    }

    /// Returns the next phase when the transition is legal. Repeating a phase
    /// is idempotent; terminal phases never regress to live execution.
    pub(crate) fn transition_to(self, requested: Self) -> Option<Self> {
        if self == requested {
            return Some(self);
        }
        match (self, requested) {
            (Self::Pending, Self::Running | Self::Terminal | Self::Failed | Self::Recovered)
            | (Self::Running, Self::Terminal | Self::Failed | Self::Recovered) => Some(requested),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_execution_can_settle_but_cannot_regress() {
        assert_eq!(
            ExecutionPhase::Pending.transition_to(ExecutionPhase::Running),
            Some(ExecutionPhase::Running)
        );
        assert_eq!(
            ExecutionPhase::Running.transition_to(ExecutionPhase::Terminal),
            Some(ExecutionPhase::Terminal)
        );
        assert_eq!(
            ExecutionPhase::Terminal.transition_to(ExecutionPhase::Running),
            None
        );
        assert!(ExecutionPhase::Running.is_active());
        assert!(ExecutionPhase::Recovered.is_settled());
    }

    #[test]
    fn serialized_shape_is_ledger_compatible() {
        assert_eq!(
            serde_json::to_string(&ExecutionPhase::Recovered).unwrap(),
            "\"recovered\""
        );
        assert_eq!(
            serde_json::from_str::<ExecutionPhase>("\"pending\"").unwrap(),
            ExecutionPhase::Pending
        );
        assert_eq!(
            serde_json::from_str::<ExecutionPhase>("\"failed\"").unwrap(),
            ExecutionPhase::Failed
        );
        assert_eq!(
            serde_json::to_string(&ExecutionOutcome::TimedOut).unwrap(),
            "\"timed_out\""
        );
        assert!(!ExecutionOutcome::Unknown.is_success());
        assert!(!ExecutionOutcome::Lost.is_success());
    }
}
