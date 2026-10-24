use crate::error::CalmError;
use crate::ids::{CardId, TrackId};
use crate::mcp_server::registry::ToolCallIdentity;
use crate::model::CardRole;
use async_trait::async_trait;
use calm_truth::decision_gate::{GateDecision, PrincipalDecisionGate};
use calm_types::worker::Principal;
use calm_types::worker::WorkerSessionId;
use sqlx::{Sqlite, Transaction};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecorderAdmissionDecisionKind {
    ReportWrite,
    TrackClose,
    PlannerMessage,
}

impl RecorderAdmissionDecisionKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ReportWrite => "report_write",
            Self::TrackClose => "track_close",
            Self::PlannerMessage => "planner_message",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecorderAdmissionDenial {
    pub(crate) track_id: TrackId,
    pub(crate) session_id: WorkerSessionId,
    pub(crate) decision_kind: RecorderAdmissionDecisionKind,
}

#[async_trait]
pub(crate) trait RecorderAdmissionGate: Send + Sync {
    async fn require_admission(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        decision_kind: RecorderAdmissionDecisionKind,
    ) -> Result<(), CalmError>;
}

pub(crate) fn emit_denial(denial: &RecorderAdmissionDenial) {
    tracing::warn!(
        target: "neige::recorder_shadow",
        track_id = %denial.track_id,
        session_id = %denial.session_id,
        decision_kind = denial.decision_kind.as_str(),
        "recorder admission gate denied write"
    );
}

pub(crate) struct CardDecisionSinkRecorderAdmissionGate {
    principal: Option<Principal>,
    track_id: TrackId,
    card_id: CardId,
}

impl CardDecisionSinkRecorderAdmissionGate {
    /// The recorder gate for a write the MCP caller makes on `track_id` outside the report funnel.
    pub(crate) fn for_identity(identity: &ToolCallIdentity, track_id: TrackId) -> Self {
        Self {
            principal: identity.to_principal(),
            track_id,
            card_id: identity.card_id.clone().into(),
        }
    }
}

#[async_trait]
impl RecorderAdmissionGate for CardDecisionSinkRecorderAdmissionGate {
    async fn require_admission(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        decision_kind: RecorderAdmissionDecisionKind,
    ) -> Result<(), CalmError> {
        let principal = self.principal.as_ref().ok_or_else(|| {
            CalmError::Forbidden("recorder gate requires an agent principal".into())
        })?;
        let Principal::Agent { session_id, .. } = principal else {
            return Err(CalmError::Forbidden(
                "recorder gate requires an agent session".into(),
            ));
        };
        let gate = PrincipalDecisionGate::new(principal.clone());
        let decision = if decision_kind == RecorderAdmissionDecisionKind::PlannerMessage {
            gate.decide_card_write(tx, &self.track_id, &self.card_id, CardRole::Planner)
                .await
        } else {
            gate.decide_recorder(tx, &self.track_id).await
        };
        match decision {
            Ok(GateDecision::Allow) => Ok(()),
            Ok(GateDecision::Deny(message)) => {
                emit_denial(&RecorderAdmissionDenial {
                    track_id: self.track_id.clone(),
                    session_id: session_id.clone(),
                    decision_kind,
                });
                Err(CalmError::Forbidden(format!(
                    "recorder gate denied {}: {message}",
                    decision_kind.as_str()
                )))
            }
            Err(error) => {
                tracing::warn!(
                    target: "neige::recorder_shadow",
                    track_id = %self.track_id,
                    session_id = %session_id,
                    decision_kind = decision_kind.as_str(),
                    error = %error,
                    "recorder admission gate computation failed; denying write"
                );
                Err(CalmError::Forbidden(format!(
                    "recorder gate failed closed for {}: {error}",
                    decision_kind.as_str()
                )))
            }
        }
    }
}
