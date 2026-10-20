//! Server compatibility reexport and runtime-header adapter; the sole codec is shared with Truth.
pub(crate) use calm_types::harness::snapshot::CanonicalHarnessSnapshot;

/// Keep the canonical wire metadata while leaving input facts with their owner.
pub(crate) fn canonical_runtime_header_value(
    snapshot: &CanonicalHarnessSnapshot,
) -> serde_json::Result<serde_json::Value> {
    let mut header: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::to_value(snapshot)?)?;
    header.remove(calm_types::conversation::CONVERSATION_INPUT_CHECKPOINT_KEY);
    Ok(serde_json::Value::Object(header))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::snapshot::HarnessSnapshot;
    use calm_types::conversation::ConversationInputCheckpoint;
    use serde_json::json;

    #[test]
    fn canonical_dormant_frame_round_trips_through_its_actual_typed_codec() {
        let frame = CanonicalHarnessSnapshot::dormant(73);
        let value = serde_json::to_value(&frame).unwrap();
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["phase"], "idle");
        assert_eq!(value["push_watermark"], 73);
        assert!(frame.header.last_thread_id.is_none());
        assert!(frame.header.last_turn_id.is_none());
        assert_eq!(frame.canonical_inputs, ConversationInputCheckpoint::empty());
        let restored: CanonicalHarnessSnapshot = serde_json::from_value(value).unwrap();
        assert_eq!(restored, frame);
    }

    #[test]
    fn canonical_codec_refuses_legacy_body_arrays_and_unknown_runtime_fields() {
        for key in [
            "pending_queue",
            "pending_envelope_ids",
            "pending_entry_meta",
            "pending_message_ids",
            "issued_input_segments",
            "projection_client_id",
            "unknown_runtime_field",
        ] {
            let mut value = serde_json::to_value(CanonicalHarnessSnapshot::dormant(1)).unwrap();
            value[key] = json!([]);
            assert!(
                serde_json::from_value::<CanonicalHarnessSnapshot>(value).is_err(),
                "{key}"
            );
        }
        let mut old = serde_json::to_value(HarnessSnapshot::initial(1, vec![])).unwrap();
        old["schema_version"] = json!(2);
        old["canonical_inputs"] = json!(ConversationInputCheckpoint::empty());
        assert!(serde_json::from_value::<CanonicalHarnessSnapshot>(old).is_err());
        let mut missing = serde_json::to_value(CanonicalHarnessSnapshot::dormant(1)).unwrap();
        missing.as_object_mut().unwrap().remove("turn_budget");
        assert!(serde_json::from_value::<CanonicalHarnessSnapshot>(missing).is_err());
    }

    #[test]
    fn shared_runtime_initializer_preserves_exact_v1_initial_wire_bytes() {
        // Frozen606 V1 struct field order/attributes, including the empty arrays.
        let expected = concat!(
            "{\"schema_version\":1,\"mode\":\"harness\",\"phase\":\"pending_thread_start\",",
            "\"turn_budget\":{\"kind\":\"legacy\"},\"push_watermark\":7,",
            "\"pending_queue\":[],\"pending_envelope_ids\":[],\"pending_entry_meta\":[],\"pending_message_ids\":[],",
            "\"last_thread_id\":null,\"last_turn_id\":null,\"last_report_body_sha256\":null,",
            "\"last_seen_head\":null,\"issued_turn_head\":null,\"projection_client_id\":null,",
            "\"wedged_reason\":null,\"interruption_intent\":null,\"token_usage\":null,",
            "\"compact_outcome\":null,\"context_usage_invalidated\":false,",
            "\"context_usage_invalidated_turn\":null,\"pending_rewind\":null,\"last_turn_base\":null}"
        );
        assert_eq!(
            serde_json::to_string(&HarnessSnapshot::initial(7, vec![])).unwrap(),
            expected
        );
    }
    #[test]
    fn v1_header_adapter_preserves_native_metadata_and_wire() {
        use calm_types::harness::budget::{FrozenBudget, PlannerTurnBudget};
        use calm_types::harness::snapshot::{
            CompactOutcome, CompactStatus, QueueEntryId, TokenUsage, TurnBase,
        };
        let mut original = HarnessSnapshot::initial(7, vec![]);
        original.turn_budget = FrozenBudget::Planner {
            budget: PlannerTurnBudget::try_from(15).unwrap(),
            start_attempt: QueueEntryId::from_wire("original-budget-attempt".into()),
            warning_attempted: true,
            context_usage: calm_types::harness::budget::FrozenContextUsage::RenderPlan,
        };
        original.token_usage = Some(TokenUsage {
            used_tokens: 123,
            total_tokens: 456,
            context_window: Some(789),
            at_ms: 10,
        });
        original.compact_outcome = Some(CompactOutcome {
            operation_id: "compact".into(),
            thread_id: "thread".into(),
            turn_id: None,
            status: CompactStatus::Unconfirmed,
        });
        original.pending_rewind = Some(serde_json::from_value(json!({
            "provider":"codex_v2", "thread_id":"thread", "before_turn_id":"turn", "reverted":true
        })).unwrap());
        original.last_turn_base = Some(TurnBase {
            turn_id: "turn".into(),
            seen_head: Some("head".into()),
        });
        original.interruption_intent = calm_types::harness::HarnessInterruptionIntent::from_request(
            "turn",
            "max_turn_duration",
        );
        let before = serde_json::to_string(&original).unwrap();
        let header = original.runtime_header();
        let mut canonical = CanonicalHarnessSnapshot::dormant(0);
        canonical.header = header.clone();
        let restored: CanonicalHarnessSnapshot =
            serde_json::from_value(serde_json::to_value(canonical).unwrap()).unwrap();
        assert_eq!(restored.header, header);
        let rebuilt = HarnessSnapshot::from_runtime_header(restored.header, vec![]);
        assert_eq!(serde_json::to_string(&rebuilt).unwrap(), before);
    }
}
