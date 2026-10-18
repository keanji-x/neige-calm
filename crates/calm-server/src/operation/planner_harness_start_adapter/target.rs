//! Read the target of an original start through its owning persisted contract.
//! Lifecycle callers do not interpret format tags or reconstruct request fields.
use super::{CalmError, CardId, Result, RetainedPayload, Value};

pub(in crate::operation) fn operation_card_id(input: &Value) -> Result<CardId> {
    let card = match RetainedPayload::decode(input)? {
        Some(retained) => CardId::from(retained.card_id().to_owned()),
        None => {
            super::super::assistant_start_wire::StartInput::decode(input)?
                .payload
                .planner_card_id
        }
    };
    if card.as_str().is_empty() {
        return Err(CalmError::Conflict(
            "start operation has no target card".into(),
        ));
    }
    Ok(card)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retained_target_uses_its_complete_declared_contract() {
        assert_eq!(
            operation_card_id(&json!({
                "canonical_activation_version":2,"card_id":"original-card",
                "worker_session_id":"original-worker",
            }))
            .unwrap(),
            CardId::from("original-card")
        );
    }

    #[test]
    fn unsupported_or_incomplete_retained_targets_never_fall_back() {
        for input in [
            json!({"canonical_activation_version":3,"card_id":"card","worker_session_id":"worker"}),
            json!({"canonical_activation_version":2,"card_id":"card"}),
            json!({"canonical_activation_version":2,"card_id":"","worker_session_id":"worker"}),
            json!({"canonical_activation_version":2,"card_id":"card","worker_session_id":""}),
            // A second historical target cannot reinterpret a declared retained request.
            json!({"canonical_activation_version":2,"card_id":"card","worker_session_id":"worker",
                "actor":"user","track_id":"track","spec_card_id":"other","cwd":"/tmp",
                "reset_harness_items":false,"force_new_thread":true}),
        ] {
            assert!(operation_card_id(&input).is_err(), "{input}");
        }
    }
}
