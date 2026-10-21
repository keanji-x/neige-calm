//! Released V3 carries no Human decisions; explicit conversion starts unresolved.
use super::released_pending::ReleasedPendingInput;
use super::*;
use crate::conversation::{ActiveInputSubmissions, InputAttemptCheckpoint};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointV3 {
    next_queue_position: u64,
    pending: Vec<ReleasedPendingInput>,
    active: Option<ActiveInputSubmissions>,
    attempts: Vec<InputAttemptCheckpoint>,
}

pub(super) fn upgrade(value: Value) -> Result<CanonicalHarnessSnapshot, String> {
    let old: CanonicalFrameWire<CheckpointV3> =
        serde_json::from_value(value).map_err(|e| e.to_string())?;
    if old.schema_version != 3 || old.mode != HARNESS_MODE {
        return Err("released canonical frame requires V3 harness format".into());
    }
    CanonicalHarnessSnapshot::try_from(CanonicalFrameWire {
        schema_version: 5,
        mode: old.mode,
        header: old.header,
        canonical_inputs: ConversationInputCheckpoint {
            next_queue_position: old.canonical_inputs.next_queue_position,
            pending: old
                .canonical_inputs
                .pending
                .into_iter()
                .map(Into::into)
                .collect(),
            active: old.canonical_inputs.active,
            attempts: old.canonical_inputs.attempts,
            receipts: vec![],
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_v3_resolution_upgrade_is_explicit_and_v4_requires_receipts() {
        let current = serde_json::to_value(CanonicalHarnessSnapshot::dormant(9)).unwrap();
        let mut old = current.clone();
        old["schema_version"] = serde_json::json!(3);
        old["canonical_inputs"]
            .as_object_mut()
            .unwrap()
            .remove("receipts");
        assert!(serde_json::from_value::<CanonicalHarnessSnapshot>(old.clone()).is_err());
        assert_eq!(
            serde_json::to_value(CanonicalHarnessSnapshot::from_released(old).unwrap()).unwrap(),
            current
        );
        let mut missing = current;
        missing["canonical_inputs"]
            .as_object_mut()
            .unwrap()
            .remove("receipts");
        assert!(CanonicalHarnessSnapshot::from_released(missing).is_err());
    }
}
