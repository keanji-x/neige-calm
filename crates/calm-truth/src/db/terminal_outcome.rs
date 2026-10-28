//! Typed terminal evidence shared by next-input delivery and Track projection.
use calm_types::harness::HarnessTimeoutEvidence;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
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
    /// A malformed or unrelated payload is not terminal evidence. Identity comes
    /// from the durable row; the provider payload must name that same turn.
    pub fn display(&self) -> Option<calm_types::conversation_display::ConversationTerminalOutcome> {
        #[derive(serde::Deserialize)]
        struct Terminal {
            id: String,
            status: calm_types::conversation_display::ConversationTerminalStatus,
            #[serde(rename = "durationMs")]
            duration_ms: Option<f64>,
        }
        let terminal: Terminal = serde_json::from_str(&self.params).ok()?;
        if terminal.id != self.turn_id
            || [
                &self.worker_session_id,
                &self.card_id,
                &self.track_id,
                &self.thread_id,
                &self.turn_id,
            ]
            .iter()
            .any(|id| id.is_empty())
        {
            return None;
        }
        Some(
            calm_types::conversation_display::ConversationTerminalOutcome {
                worker_session_id: self.worker_session_id.clone(),
                card_id: self.card_id.clone(),
                track_id: self.track_id.clone(),
                thread_id: self.thread_id.clone(),
                turn_id: self.turn_id.clone(),
                status: terminal.status,
                created_at_ms: self.created_at_ms,
                duration_ms: terminal
                    .duration_ms
                    .filter(|ms| ms.is_finite() && *ms >= 0.0),
            },
        )
    }

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
