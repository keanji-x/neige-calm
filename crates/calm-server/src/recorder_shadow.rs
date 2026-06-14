use crate::error::CalmError;
use crate::ids::WaveId;
use async_trait::async_trait;
use calm_types::worker::WorkerSessionId;
use sqlx::{Sqlite, Transaction};
use std::sync::atomic::{AtomicU64, Ordering};

static RECORDER_SHADOW_DIVERGENCES: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecorderShadowDecisionKind {
    WaveLifecycle,
    ReportWrite,
}

impl RecorderShadowDecisionKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::WaveLifecycle => "wave_lifecycle",
            Self::ReportWrite => "report_write",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecorderShadowDivergence {
    pub(crate) wave_id: WaveId,
    pub(crate) session_id: WorkerSessionId,
    pub(crate) decision_kind: RecorderShadowDecisionKind,
}

#[async_trait]
pub(crate) trait RecorderShadowProbe: Send + Sync {
    async fn record(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        decision_kind: RecorderShadowDecisionKind,
    ) -> Result<(), CalmError>;
}

pub(crate) fn emit_divergence(divergence: &RecorderShadowDivergence) {
    RECORDER_SHADOW_DIVERGENCES.fetch_add(1, Ordering::Relaxed);
    tracing::warn!(
        target: "neige::recorder_shadow",
        wave_id = %divergence.wave_id,
        session_id = %divergence.session_id,
        decision_kind = divergence.decision_kind.as_str(),
        "recorder shadow divergence: card-era write allowed but recorder gate would deny"
    );
}

#[cfg(test)]
pub(crate) fn divergence_count_for_test() -> u64 {
    RECORDER_SHADOW_DIVERGENCES.load(Ordering::Relaxed)
}
