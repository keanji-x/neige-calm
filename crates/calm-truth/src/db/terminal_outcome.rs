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
            error: Option<Value>,
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
                diagnostic_code: (terminal.status
                    != calm_types::conversation_display::ConversationTerminalStatus::Completed)
                    .then_some(terminal.error)
                    .flatten()
                    .filter(|error| !error.is_null())
                    .map(|error| {
                        error["code"]
                            .as_str()
                            .and_then(calm_types::harness::diagnostics::terminal_diagnostic)
                            .map_or("provider_error", |(code, _)| code)
                            .to_owned()
                    }),
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

#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn display_keeps_only_closed_codes_and_never_native_error_text() {
        for (status, code, expected) in [
            (
                "failed",
                "acp_permission_denied",
                Some("acp_permission_denied"),
            ),
            (
                "failed",
                "acp_permission_rejected",
                Some("acp_permission_rejected"),
            ),
            ("failed", "acp_tool_failure", Some("acp_tool_failure")),
            (
                "failed",
                "acp_permission_unexpected",
                Some("acp_permission_unexpected"),
            ),
            ("failed", "private native code", Some("provider_error")),
            ("completed", "acp_permission_denied", None),
        ] {
            let row = HarnessTerminalOutcome {
                id: 1, worker_session_id: "worker".into(), card_id: "card".into(),
                track_id: "track".into(), thread_id: "thread".into(), turn_id: "turn".into(),
                params: json!({"id":"turn","status":status,"error":{"code":code,"message":"private native payload"}}).to_string(),
                created_at_ms: 1,
            };
            let display = row.display().unwrap();
            assert_eq!(display.diagnostic_code.as_deref(), expected);
            assert!(!serde_json::to_string(&display).unwrap().contains("private"));
        }
    }
}
