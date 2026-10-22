use crate::error::CalmError;
use crate::ids::TrackId;
use async_trait::async_trait;
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
