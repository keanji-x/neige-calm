//! The declared V4 persisted frame. Both Truth and the server use this exact codec.
use super::budget::FrozenBudget;
use super::{HARNESS_MODE, HarnessPhaseTag};
use crate::conversation::ConversationInputCheckpoint;
use serde::{Deserialize, Serialize};
use serde_json::Value;
mod v2;
mod v3;
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
        if wire.schema_version != 4 || wire.mode != HARNESS_MODE {
            return Err("canonical frame requires its declared V4 harness format".into());
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
            Some(4) => serde_json::from_value(value).map_err(|error| error.to_string()),
            Some(_) | None => Err("unsupported canonical frame version".into()),
        }
    }

    /// A deliberate new dormant frame, never a fallback for malformed stored data.
    pub fn dormant(push_watermark: i64) -> Self {
        Self {
            schema_version: 4,
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
}
