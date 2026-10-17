//! The sole V2 frame representation: runtime metadata and canonical references.
//! It cannot contain a second pending/body array or an issued prompt envelope.
use super::runtime_header::HarnessRuntimeHeader;
use super::*;
use calm_types::conversation::ConversationInputCheckpoint;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "CanonicalFrameWire")]
pub(crate) struct CanonicalHarnessSnapshot {
    schema_version: u32,
    mode: String,
    #[serde(flatten)]
    pub header: HarnessRuntimeHeader,
    pub canonical_inputs: ConversationInputCheckpoint,
}

#[derive(Deserialize)]
struct CanonicalFrameWire {
    schema_version: u32,
    mode: String,
    canonical_inputs: ConversationInputCheckpoint,
    // The typed header rejects every unrecognized field. This avoids a second,
    // manually maintained inventory of runtime keys or retired body fields.
    #[serde(flatten)]
    header: serde_json::Map<String, Value>,
}

impl TryFrom<CanonicalFrameWire> for CanonicalHarnessSnapshot {
    type Error = String;

    fn try_from(wire: CanonicalFrameWire) -> Result<Self, Self::Error> {
        if wire.schema_version != 2 || wire.mode != HARNESS_MODE {
            return Err("canonical frame requires its declared V2 harness format".into());
        }
        wire.canonical_inputs.validate()?;
        let header = serde_json::from_value(Value::Object(wire.header))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            schema_version: wire.schema_version,
            mode: wire.mode,
            header,
            canonical_inputs: wire.canonical_inputs,
        })
    }
}

impl CanonicalHarnessSnapshot {
    /// Accepted input has no native execution yet. No configuration, filesystem,
    /// environment or provider readiness is consulted by this pure constructor.
    pub(crate) fn dormant(push_watermark: i64) -> Self {
        Self {
            schema_version: 2,
            mode: HARNESS_MODE.into(),
            header: HarnessRuntimeHeader::from_state(
                &HarnessState::Idle,
                FrozenBudget::Legacy,
                push_watermark,
                None,
                None,
                None,
            ),
            canonical_inputs: ConversationInputCheckpoint::empty(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
