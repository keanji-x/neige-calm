//! Typed terminal evidence shared by next-input delivery and Track projection.
use calm_types::harness::HarnessTimeoutEvidence;
use serde_json::Value;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct HarnessTerminalOutcome {
    pub id: i64,
    pub worker_session_id: String,
    pub card_id: String,
    pub track_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub params: String,
    pub created_at_ms: i64,
}

impl HarnessTerminalOutcome {
    /// Reserved metadata is authoritative only with its complete persisted row identity.
    pub fn timeout(&self) -> Option<HarnessTimeoutEvidence> {
        let params: Value = serde_json::from_str(&self.params).ok()?;
        if params["status"] != "interrupted" || params["id"] != self.turn_id {
            return None;
        }
        let evidence: HarnessTimeoutEvidence =
            serde_json::from_value(params.get("harness_interruption_reason")?.clone()).ok()?;
        let HarnessTimeoutEvidence::V1 { request, .. } = &evidence;
        // UTC clocks can step backwards; elapsed/budget use the monotonic watchdog clock.
        (request.session_id == self.worker_session_id
            && request.card_id == self.card_id
            && request.track_id == self.track_id
            && request.thread_id == self.thread_id
            && request.turn_id == self.turn_id
            && request.budget_ms > 0
            && request.effective_elapsed_ms >= request.budget_ms)
            .then_some(evidence)
    }
}
