//! One terminal-outcome projection for live notifications and history recovery.
use crate::db::Repo;
use crate::error::Result;
use serde_json::Value;

/// Recovery can synthesize an interrupted row without a provider terminal confirmation.
#[derive(Clone, Copy)]
pub(crate) enum Confirmation {
    ProviderTerminal,
    Unconfirmed,
}

/// Native error messages can echo accepted input or malformed request frames.
/// Canonical execution stores a fixed diagnostic, not those transport payloads.
pub(crate) fn canonical_diagnostic(turn: &mut Value) {
    if let Some(object) = turn.as_object_mut()
        && object.get("error").is_some_and(|error| !error.is_null())
    {
        object.insert(
            "error".into(),
            serde_json::json!({
                "message": "The provider ended this execution with an error."
            }),
        );
    }
}

// Keep the full terminal identity and confirmation explicit at this authority boundary.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record(
    repo: &dyn Repo,
    session_id: &str,
    card_id: &str,
    track_id: &str,
    thread_id: &str,
    turn_id: &str,
    turn: &Value,
    confirmation: Confirmation,
) -> Result<i64> {
    let mut outcome = turn.clone();
    if let Some(object) = outcome.as_object_mut() {
        object.remove("items");
        object.remove("itemsView");
        // This reserved field is written only from the kernel's durable interrupt intent.
        object.remove("harness_interruption_reason");
        object.remove("harness_timeout");
    }
    if matches!(confirmation, Confirmation::ProviderTerminal)
        && outcome.get("status").and_then(Value::as_str) == Some("interrupted")
        && outcome.get("id").and_then(Value::as_str) == Some(turn_id)
        && let Some(session) = repo.session_projection_by_id(session_id).await?
        && session.id == session_id
        && session.card_id == card_id
        && let Some(card) = repo.card_get(card_id).await?
        && card.track_id.as_str() == track_id
        && let Some(snapshot) = session
            .handle_state_json
            .and_then(super::HarnessSnapshot::parse_known)
        && snapshot.last_turn_id.as_deref() == Some(turn_id)
        && snapshot.last_thread_id.as_deref() == Some(thread_id)
        && let Some(intent) = snapshot.interruption_intent
        && intent.turn_id() == turn_id
    {
        use calm_types::harness::{
            HarnessInterruptionIntent, HarnessTimeoutEvidence, HarnessTimeoutIntent,
        };
        match intent {
            HarnessInterruptionIntent::Legacy(intent) => {
                outcome["harness_interruption_reason"] = serde_json::to_value(intent.reason)?;
            }
            HarnessInterruptionIntent::Versioned(HarnessTimeoutIntent::V1(request))
                if request.session_id == session_id
                    && request.card_id == card_id
                    && request.track_id == track_id
                    && request.thread_id == thread_id
                    && request.turn_id == turn_id =>
            {
                // This namespace was already stripped by released recorders. A new field
                // alone could contain provider data in historical rows and is not authority.
                outcome["harness_interruption_reason"] =
                    serde_json::to_value(HarnessTimeoutEvidence::V1 {
                        request,
                        ended_at_ms: crate::model::now_ms(),
                    })?;
            }
            _ => {}
        }
    }
    Ok(repo
        .harness_turn_outcome_put(
            session_id,
            card_id,
            track_id,
            thread_id,
            turn_id,
            &serde_json::to_string(&outcome)?,
        )
        .await?)
}
