//! Private released V2 shape; used only by explicit evidence adaptation.
use super::released_pending::ReleasedPendingInput;
use super::*;
use crate::conversation::{
    ActiveInputSubmissions, InputAttemptCheckpoint, InputAttemptFact, InputInvocation,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointV2 {
    next_queue_position: u64,
    pending: Vec<ReleasedPendingInput>,
    active: Option<ActiveInputSubmissions>,
    attempts: Vec<InputAttemptFact>,
}

pub(super) fn upgrade(value: Value) -> Result<CanonicalHarnessSnapshot, String> {
    let old: CanonicalFrameWire<CheckpointV2> =
        serde_json::from_value(value).map_err(|error| error.to_string())?;
    if old.schema_version != 2 || old.mode != HARNESS_MODE {
        return Err("released canonical frame requires V2 harness format".into());
    }
    let checkpoint = ConversationInputCheckpoint {
        next_queue_position: old.canonical_inputs.next_queue_position,
        pending: old
            .canonical_inputs
            .pending
            .into_iter()
            .map(Into::into)
            .collect(),
        active: old.canonical_inputs.active,
        receipts: vec![],
        attempts: old
            .canonical_inputs
            .attempts
            .into_iter()
            .map(|fact| InputAttemptCheckpoint {
                revision: 0,
                invocation: InputInvocation::PriorInvocationNotRecorded,
                attempt: fact.attempt,
                correlation: fact.correlation,
                delivery: fact.delivery,
            })
            .collect(),
    };
    CanonicalHarnessSnapshot::try_from(CanonicalFrameWire {
        schema_version: 5,
        mode: old.mode,
        canonical_inputs: checkpoint,
        header: old.header,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn old() -> Value {
        let mut old = serde_json::to_value(CanonicalHarnessSnapshot::dormant(7)).unwrap();
        old["schema_version"] = json!(2);
        old["canonical_inputs"]
            .as_object_mut()
            .unwrap()
            .remove("receipts");
        old["canonical_inputs"]["attempts"] = json!([{
            "attempt":{"submission_id":"11111111111111111111111111111111","attempt_id":"22222222222222222222222222222222"},
            "correlation":{"provider":"codex","thread_id":"thread","native_session_id":null,"local_turn_id":null,"native_turn_id":null,"rpc_request_id":null},
            "delivery":{"kind":"unknown"}
        }]);
        old
    }
    #[test]
    fn canonical_v2_upgrade_preserves_attempt_evidence_all_states() {
        for delivery in [
            json!({"kind":"crossing_unknown"}),
            json!({"kind":"unknown"}),
            json!({"kind":"not_dispatched","basis":"local_preflight"}),
            json!({"kind":"accepted","basis":"codex_turn_start","evidence":{"kind":"native_turn","native_turn_id":"native"}}),
        ] {
            let mut old = old();
            old["canonical_inputs"]["attempts"][0]["delivery"] = delivery;
            if old["canonical_inputs"]["attempts"][0]["delivery"]["kind"] == "accepted" {
                old["canonical_inputs"]["attempts"][0]["correlation"]["native_turn_id"] =
                    json!("native");
            }
            let converted =
                serde_json::to_value(CanonicalHarnessSnapshot::from_released(old.clone()).unwrap())
                    .unwrap();
            assert_eq!(converted["schema_version"], 5);
            assert_eq!(converted["canonical_inputs"]["attempts"][0]["revision"], 0);
            assert_eq!(
                converted["canonical_inputs"]["attempts"][0]["invocation"],
                json!({"kind":"prior_invocation_not_recorded"})
            );
            let mut restored = converted;
            restored["schema_version"] = json!(2);
            restored["canonical_inputs"]
                .as_object_mut()
                .unwrap()
                .remove("receipts");
            let attempt = restored["canonical_inputs"]["attempts"][0]
                .as_object_mut()
                .unwrap();
            attempt.remove("revision");
            attempt.remove("invocation");
            assert_eq!(restored, old);
        }
    }
    #[test]
    fn canonical_v3_refuses_missing_revision_and_future_version() {
        let current =
            serde_json::to_value(CanonicalHarnessSnapshot::from_released(old()).unwrap()).unwrap();
        for field in ["revision", "invocation"] {
            let mut missing = current.clone();
            missing["canonical_inputs"]["attempts"][0]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(serde_json::from_value::<CanonicalHarnessSnapshot>(missing.clone()).is_err());
            assert!(CanonicalHarnessSnapshot::from_released(missing).is_err());
        }
        let mut recorded = current.clone();
        recorded["canonical_inputs"]["attempts"][0]["invocation"] =
            json!({"kind":"recorded","configuration":{"model":null,"permission":"never"}});
        assert!(serde_json::from_value::<CanonicalHarnessSnapshot>(recorded).is_err());
        let mut future = current;
        future["schema_version"] = json!(6);
        assert!(CanonicalHarnessSnapshot::from_released(future).is_err());
        for (field, value) in [
            ("schema_version", json!(1)),
            ("mode", json!("other")),
            ("pending_queue", json!([])),
        ] {
            let mut invalid = old();
            invalid[field] = value;
            assert!(CanonicalHarnessSnapshot::from_released(invalid).is_err());
        }
        let mut conflict = old();
        conflict["canonical_inputs"]["attempts"][0]["revision"] = json!(5);
        assert!(CanonicalHarnessSnapshot::from_released(conflict).is_err());
    }
}
