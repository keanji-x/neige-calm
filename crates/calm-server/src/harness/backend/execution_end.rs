//! Private execution closure, independent of input delivery and resource reuse.
use super::*;
use calm_types::conversation::{LogicalTurnId, SubmissionDispatchProof};

/// Transient original execution association supplied by the protected dispatch
/// owner. Never serialized into the already-versioned provider proof/journal.
#[derive(Clone)]
pub(crate) struct ExecutionInput {
    logical_turn_id: LogicalTurnId,
    proof: SubmissionDispatchProof,
}

impl ExecutionInput {
    pub(crate) fn canonical(
        logical_turn_id: &LogicalTurnId,
        proof: &SubmissionDispatchProof,
    ) -> Self {
        Self {
            logical_turn_id: logical_turn_id.clone(),
            proof: proof.clone(),
        }
    }

    pub(crate) fn matches(&self, target: &ExecutionEndTarget) -> bool {
        self.logical_turn_id == target.logical_turn_id && self.proof == target.proof
    }
}

/// Sampled by the canonical owner from its current committed carrier. This is
/// correlation data, never a stop certificate or an execution authorization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExecutionEndTarget {
    pub(crate) card_id: String,
    pub(crate) track_id: String,
    pub(crate) worker_session_id: String,
    pub(crate) logical_turn_id: LogicalTurnId,
    pub(crate) proof: SubmissionDispatchProof,
    pub(crate) scope: ExecutionEndScope,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExecutionEndScope {
    NativeTurn {
        thread_id: String,
        native_turn_id: String,
    },
}

impl ExecutionEndTarget {
    pub(crate) fn matches_owner(&self, card: &str, track: &str, runtime: &str) -> bool {
        !card.is_empty()
            && !track.is_empty()
            && !runtime.is_empty()
            && self.card_id == card
            && self.track_id == track
            && self.worker_session_id == runtime
    }
}

/// Only the adapter owner can construct this after inspecting its actual current
/// native owner. Kept reusable across a canonical snapshot CAS retry.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct QualifiedExecutionEnd {
    target: ExecutionEndTarget,
    terminal: Option<serde_json::Value>,
}

impl std::fmt::Debug for QualifiedExecutionEnd {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QualifiedExecutionEnd")
            .field("target", &self.target)
            .field("has_terminal", &self.terminal.is_some())
            .finish()
    }
}

impl QualifiedExecutionEnd {
    pub(crate) fn matches(&self, target: &ExecutionEndTarget) -> bool {
        &self.target == target
    }
    pub(crate) fn terminal(&self) -> Option<&serde_json::Value> {
        self.terminal.as_ref()
    }
}

impl PlannerBackend {
    pub(crate) async fn qualify_execution_end(
        &self,
        target: &ExecutionEndTarget,
    ) -> Result<Option<QualifiedExecutionEnd>> {
        let terminal = match &self.0 {
            Arm::Claude(_) | Arm::Acp(_) => None,
            Arm::Codex(daemon, owner) => {
                if owner.binding.matches(target) {
                    daemon.qualified_execution_terminal(target).map(Some)
                } else {
                    None
                }
            }
        };
        Ok(terminal.map(|terminal| QualifiedExecutionEnd {
            target: target.clone(),
            terminal,
        }))
    }
}
