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

    pub(crate) fn acp_association(&self) -> crate::db::sqlite::AcpCanonicalInput {
        crate::db::sqlite::AcpCanonicalInput {
            logical_turn_id: self.logical_turn_id.clone(),
            proof: self.proof.clone(),
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
    LocalProcess {
        thread_id: uuid::Uuid,
        local_turn_id: uuid::Uuid,
    },
    ReceiptProcess {
        thread_id: uuid::Uuid,
        local_turn_id: uuid::Uuid,
        native_session_id: String,
    },
    NativeTurn {
        thread_id: String,
        native_turn_id: String,
    },
}

impl ExecutionEndScope {
    /// Decode the existing persisted facts once; malformed or mixed tuples hold.
    pub(crate) fn from_correlation(
        correlation: &calm_types::conversation::InputCorrelationFact,
    ) -> Option<Self> {
        use calm_types::runtime::AgentProtocol;
        if correlation.thread_id.is_empty()
            || correlation
                .native_session_id
                .as_ref()
                .is_some_and(String::is_empty)
        {
            return None;
        }
        match (&correlation.native_turn_id, &correlation.local_turn_id) {
            (Some(turn), None) => match correlation.provider.protocol() {
                AgentProtocol::Codex => (!turn.is_empty()).then(|| Self::NativeTurn {
                    thread_id: correlation.thread_id.clone(),
                    native_turn_id: turn.clone(),
                }),
                AgentProtocol::Claude | AgentProtocol::Acp => None,
            },
            (None, Some(turn)) => match correlation.provider.protocol() {
                AgentProtocol::Claude => Some(Self::LocalProcess {
                    thread_id: uuid::Uuid::parse_str(&correlation.thread_id).ok()?,
                    local_turn_id: uuid::Uuid::parse_str(turn).ok()?,
                }),
                AgentProtocol::Acp => Some(Self::ReceiptProcess {
                    thread_id: uuid::Uuid::parse_str(&correlation.thread_id).ok()?,
                    local_turn_id: uuid::Uuid::parse_str(turn).ok()?,
                    native_session_id: correlation.native_session_id.clone()?,
                }),
                AgentProtocol::Codex => None,
            },
            (Some(_), Some(_)) | (None, None) => None,
        }
    }

    pub(crate) fn thread_and_turn(&self) -> (String, String) {
        match self {
            Self::NativeTurn {
                thread_id,
                native_turn_id,
            } => (thread_id.clone(), native_turn_id.clone()),
            Self::LocalProcess {
                thread_id,
                local_turn_id,
            }
            | Self::ReceiptProcess {
                thread_id,
                local_turn_id,
                ..
            } => (thread_id.to_string(), local_turn_id.to_string()),
        }
    }
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
    pub(crate) async fn owns_local_execution(&self, target: &ExecutionEndTarget) -> bool {
        match &self.0 {
            Arm::Claude(session) => session.owns_local_execution(target).await,
            Arm::Acp(session) => session.owns_local_execution(target).await,
            Arm::Codex(..) => false,
        }
    }

    pub(crate) async fn qualify_execution_end(
        &self,
        target: &ExecutionEndTarget,
    ) -> Result<Option<QualifiedExecutionEnd>> {
        let terminal = match &self.0 {
            Arm::Claude(session) => session.qualify_execution_end(target).await?.then_some(None),
            Arm::Acp(session) => session.qualify_execution_end(target).await?.map(Some),
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

/// A maintenance execution has no canonical input claim or user turn identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MaintenanceTarget {
    pub(crate) card_id: String,
    pub(crate) track_id: String,
    pub(crate) worker_session_id: String,
    pub(crate) thread_id: String,
    pub(crate) operation_id: String,
}

pub(crate) enum MaintenanceObservation {
    Turn {
        turn_id: String,
        settled: Option<bool>,
    },
    Unconfirmed,
}

impl PlannerBackend {
    pub(crate) async fn compact_canonical(
        &self,
        target: MaintenanceTarget,
        request: CompactRequest,
        admission: &crate::harness::native_admission::NativePromptAdmission<'_>,
    ) -> Result<CompactSubmission<CalmError>> {
        match &self.0 {
            Arm::Claude(session) => Ok(session
                .compact_canonical(target, &request.selection, admission)
                .await),
            Arm::Codex(daemon, owner) => {
                if !owner.binding.matches_maintenance(&target) {
                    return Err(CalmError::Conflict("maintenance owner changed".into()));
                }
                Ok(super::control::compact_delivery(
                    daemon
                        .thread_compact_delivery_admitted(target, admission)
                        .await,
                ))
            }
            Arm::Acp(session) => Ok(session
                .compact_canonical(target, &request.selection, admission)
                .await),
        }
    }

    pub(crate) async fn observe_maintenance(
        &self,
        target: &MaintenanceTarget,
    ) -> Result<Option<MaintenanceObservation>> {
        match &self.0 {
            Arm::Claude(session) => session.observe_maintenance(target).await,
            Arm::Codex(daemon, owner) => {
                if !owner.binding.matches_maintenance(target) {
                    return Ok(None);
                }
                Ok(daemon.observe_maintenance(target))
            }
            Arm::Acp(session) => session.observe_maintenance(target).await,
        }
    }
}
