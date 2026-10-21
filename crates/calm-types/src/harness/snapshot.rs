//! The declared V5 persisted frame. Both Truth and the server use this exact codec.
use super::budget::FrozenBudget;
use super::{HARNESS_MODE, HarnessPhaseTag};
use crate::conversation::ConversationInputCheckpoint;
use serde::{Deserialize, Serialize};
use serde_json::Value;
mod released_pending;
mod v2;
mod v3;
mod v4;
mod values;
pub use values::{
    CompactOutcome, CompactStatus, PersistedRewind, QueueEntryId, TokenUsage, TurnBase,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessRuntimeHeader {
    pub phase: HarnessPhaseTag,
    pub turn_budget: FrozenBudget,
    pub push_watermark: i64,
    pub last_thread_id: Option<String>,
    pub last_turn_id: Option<String>,
    pub last_report_body_sha256: Option<String>,
    pub last_seen_head: Option<String>,
    pub issued_turn_head: Option<String>,
    pub wedged_reason: Option<String>,
    pub interruption_intent: Option<crate::harness::HarnessInterruptionIntent>,
    pub token_usage: Option<TokenUsage>,
    pub compact_outcome: Option<CompactOutcome>,
    pub context_usage_invalidated: bool,
    pub context_usage_invalidated_turn: Option<String>,
    pub pending_rewind: Option<PersistedRewind>,
    pub last_turn_base: Option<TurnBase>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "CanonicalFrameWire<ConversationInputCheckpoint>")]
pub struct CanonicalHarnessSnapshot {
    schema_version: u32,
    mode: String,
    #[serde(flatten)]
    pub header: HarnessRuntimeHeader,
    pub canonical_inputs: ConversationInputCheckpoint,
}

#[derive(Deserialize)]
struct CanonicalFrameWire<C> {
    schema_version: u32,
    mode: String,
    canonical_inputs: C,
    // The typed header rejects every unrecognized field. This avoids a second,
    // manually maintained inventory of runtime keys or retired body fields.
    #[serde(flatten)]
    header: serde_json::Map<String, Value>,
}

impl TryFrom<CanonicalFrameWire<ConversationInputCheckpoint>> for CanonicalHarnessSnapshot {
    type Error = String;

    fn try_from(
        wire: CanonicalFrameWire<ConversationInputCheckpoint>,
    ) -> Result<Self, Self::Error> {
        if wire.schema_version != 5 || wire.mode != HARNESS_MODE {
            return Err("canonical frame requires its declared V5 harness format".into());
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

impl HarnessRuntimeHeader {
    pub fn new(
        phase: HarnessPhaseTag,
        turn_budget: FrozenBudget,
        push_watermark: i64,
        last_thread_id: Option<String>,
        last_turn_id: Option<String>,
        last_report_body_sha256: Option<String>,
    ) -> Self {
        Self {
            phase,
            turn_budget,
            push_watermark,
            last_thread_id,
            last_turn_id,
            last_report_body_sha256,
            last_seen_head: None,
            issued_turn_head: None,
            wedged_reason: None,
            interruption_intent: None,
            token_usage: None,
            compact_outcome: None,
            context_usage_invalidated: false,
            context_usage_invalidated_turn: None,
            pending_rewind: None,
            last_turn_base: None,
        }
    }
}

impl CanonicalHarnessSnapshot {
    /// Read adaptation for released immutable outputs and the boot-time migration.
    /// Live carrier reads deliberately use Deserialize instead.
    pub fn from_released(value: Value) -> Result<Self, String> {
        match value.get("schema_version").and_then(Value::as_u64) {
            Some(2) => v2::upgrade(value),
            Some(3) => v3::upgrade(value),
            Some(4) => v4::upgrade(value),
            Some(5) => serde_json::from_value(value).map_err(|error| error.to_string()),
            Some(_) | None => Err("unsupported canonical frame version".into()),
        }
    }

    /// A deliberate new dormant frame, never a fallback for malformed stored data.
    pub fn dormant(push_watermark: i64) -> Self {
        Self {
            schema_version: 5,
            mode: HARNESS_MODE.into(),
            header: HarnessRuntimeHeader::new(
                HarnessPhaseTag::Idle,
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
    fn v4_upgrade_preserves_keep_and_invocation_without_context_backfill() {
        let mut old = serde_json::to_value(CanonicalHarnessSnapshot::dormant(7)).unwrap();
        old["schema_version"] = json!(4);
        let request = json!({"carrier_id":"carrier", "submission_id":"11111111111111111111111111111111",
            "attempt_id":"22222222222222222222222222222222", "revision":"7"});
        let result = json!({"carrier_id":"carrier", "submission_id":request["submission_id"],
            "attempt_id":request["attempt_id"], "revision":"8", "waiting_for_settlement":true});
        for invocation in [
            json!({"kind":"prior_invocation_not_recorded"}),
            json!({"kind":"recorded","configuration":{"model":null,"effort":null,"permission":"never"}}),
            json!({"kind":"recorded","configuration":{"model":"frozen","effort":"high","permission":"never"}}),
        ] {
            old["canonical_inputs"]["attempts"] = json!([{
                "revision":8, "invocation":invocation,
                "attempt":{"submission_id":request["submission_id"],"attempt_id":request["attempt_id"]},
                "correlation":{"provider":"codex","thread_id":"thread","native_session_id":null,
                    "local_turn_id":null,"native_turn_id":null,"rpc_request_id":null},
                "delivery":{"kind":"unknown"}
            }]);
            old["canonical_inputs"]["receipts"] = json!([{
                "key":"keep", "principal":"human", "fingerprint":"a".repeat(64),
                "request":request, "result":result
            }]);
            let upgraded = CanonicalHarnessSnapshot::from_released(old.clone()).unwrap();
            let mut expected = old.clone();
            expected["schema_version"] = json!(5);
            expected["canonical_inputs"]["receipts"][0] = json!({
                "key":"keep", "principal":"human", "fingerprint":"a".repeat(64),
                "decision":{"kind":"keep","request":request,"result":result}
            });
            assert_eq!(serde_json::to_value(upgraded).unwrap(), expected);
            assert!(serde_json::from_value::<CanonicalHarnessSnapshot>(old.clone()).is_err());
            assert!(serde_json::from_value::<CanonicalHarnessSnapshot>(expected.clone()).is_ok());
            for field in ["invocation", "revision"] {
                let mut invalid = old.clone();
                invalid["canonical_inputs"]["attempts"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
                assert!(CanonicalHarnessSnapshot::from_released(invalid).is_err());
            }
            for field in ["key", "principal", "fingerprint", "request", "result"] {
                let mut invalid = old.clone();
                invalid["canonical_inputs"]["receipts"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
                assert!(CanonicalHarnessSnapshot::from_released(invalid).is_err());
            }
            let mut untagged_v5 = old.clone();
            untagged_v5["schema_version"] = json!(5);
            assert!(CanonicalHarnessSnapshot::from_released(untagged_v5).is_err());
            let mut tagged_v4 = expected.clone();
            tagged_v4["schema_version"] = json!(4);
            assert!(CanonicalHarnessSnapshot::from_released(tagged_v4).is_err());
            let mut invalid = expected;
            invalid["canonical_inputs"]["receipts"][0]["decision"]["kind"] = json!("retry");
            assert!(CanonicalHarnessSnapshot::from_released(invalid).is_err());
        }
    }

    #[test]
    fn v3_frame_requires_declared_header_and_nested_value_shapes() {
        let original = serde_json::to_value(CanonicalHarnessSnapshot::dormant(7)).unwrap();
        for field in ["turn_budget", "push_watermark", "context_usage_invalidated"] {
            let mut missing = original.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<CanonicalHarnessSnapshot>(missing).is_err());
        }
        let mut unknown = original.clone();
        unknown["unrecognized_runtime_authority"] = json!(true);
        assert!(serde_json::from_value::<CanonicalHarnessSnapshot>(unknown).is_err());
        let mut invalid_budget = original.clone();
        invalid_budget["turn_budget"] = json!({
            "kind":"planner", "budget":0, "start_attempt":"attempt", "warning_attempted":false
        });
        assert!(serde_json::from_value::<CanonicalHarnessSnapshot>(invalid_budget).is_err());
        let mut invalid_rewind = original;
        invalid_rewind["pending_rewind"] = json!({
            "provider":"claude", "resume_at":"not-a-uuid", "drops_turn":"not-a-uuid"
        });
        assert!(serde_json::from_value::<CanonicalHarnessSnapshot>(invalid_rewind).is_err());
    }

    #[test]
    fn v3_frame_round_trip_keeps_typed_runtime_metadata() {
        let mut frame = CanonicalHarnessSnapshot::dormant(7);
        frame.header.turn_budget = FrozenBudget::Planner {
            budget: super::super::budget::PlannerTurnBudget::try_from(15).unwrap(),
            start_attempt: QueueEntryId::from_wire("original-attempt".into()),
            warning_attempted: true,
            context_usage: crate::harness::budget::FrozenContextUsage::RenderPlan,
        };
        frame.header.pending_rewind = Some(PersistedRewind::CodexV2 {
            thread_id: "thread".into(),
            before_turn_id: "turn".into(),
            reverted: false,
        });
        let encoded = serde_json::to_value(&frame).unwrap();
        let restored: CanonicalHarnessSnapshot = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(restored, frame);
        assert_eq!(serde_json::to_value(restored).unwrap(), encoded);
    }

    #[test]
    fn canonical_released_frames_reject_v5_retry_pending_kind() {
        use crate::conversation::{PendingInput, RetryIntentId};
        let mut frame = CanonicalHarnessSnapshot::dormant(0);
        frame
            .canonical_inputs
            .pending
            .push(PendingInput::ExplicitSuccessor {
                retry_intent_id: RetryIntentId::parse(&"a".repeat(32)).unwrap(),
                queue_position: 0,
                queued_at_ms: 0,
            });
        frame.canonical_inputs.next_queue_position = 1;
        for version in [2, 3, 4] {
            let mut value = serde_json::to_value(&frame).unwrap();
            value["schema_version"] = serde_json::json!(version);
            if version < 4 {
                value["canonical_inputs"]
                    .as_object_mut()
                    .unwrap()
                    .remove("receipts");
            }
            assert!(
                CanonicalHarnessSnapshot::from_released(value).is_err(),
                "released V{version} must reject the unreleased successor kind"
            );
        }
    }
}
