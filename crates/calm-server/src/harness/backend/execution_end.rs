//! Private execution closure, independent of input delivery and resource reuse.
use super::*;
use calm_types::conversation::{LogicalTurnId, SubmissionDispatchProof};

/// Transient original execution association supplied by the protected dispatch
/// owner. Never serialized into the already-versioned provider proof/journal.
#[derive(Clone)]
pub(crate) enum ExecutionInput {
    Legacy,
    Canonical(CanonicalExecutionInput),
}

#[derive(Clone)]
pub(crate) struct CanonicalExecutionInput {
    logical_turn_id: LogicalTurnId,
    proof: SubmissionDispatchProof,
}

impl ExecutionInput {
    pub(crate) fn canonical(
        logical_turn_id: &LogicalTurnId,
        proof: &SubmissionDispatchProof,
    ) -> Self {
        Self::Canonical(CanonicalExecutionInput {
            logical_turn_id: logical_turn_id.clone(),
            proof: proof.clone(),
        })
    }

    pub(crate) fn matches_logical(&self, logical_turn_id: &LogicalTurnId) -> bool {
        matches!(self, Self::Canonical(actual) if &actual.logical_turn_id == logical_turn_id)
    }

    pub(crate) fn matches(&self, target: &ExecutionEndTarget) -> bool {
        matches!(self, Self::Canonical(actual)
            if actual.logical_turn_id == target.logical_turn_id && actual.proof == target.proof)
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
    LocalExecution {
        thread_id: String,
        native_session_id: String,
        local_turn_id: String,
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QualifiedExecutionEnd {
    target: ExecutionEndTarget,
}

impl QualifiedExecutionEnd {
    pub(crate) fn matches(&self, target: &ExecutionEndTarget) -> bool {
        &self.target == target
    }
}

impl PlannerBackend {
    pub(crate) async fn qualify_execution_end(
        &self,
        target: &ExecutionEndTarget,
    ) -> Result<Option<QualifiedExecutionEnd>> {
        let qualified = match &self.0 {
            Arm::Claude(session) => session.qualify_execution_end(target).await?,
            Arm::Acp(session) => session.qualify_execution_end(target).await?,
            Arm::Codex(daemon, owner) => {
                owner.binding.matches(target) && daemon.qualify_execution_end(target)
            }
        };
        Ok(qualified.then(|| QualifiedExecutionEnd {
            target: target.clone(),
        }))
    }
}
