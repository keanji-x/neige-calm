//! Released V4: preserve invocation evidence and explicitly tag existing Keep decisions.
use super::released_pending::ReleasedPendingInput;
use super::*;
use crate::conversation::{
    ActiveInputSubmissions, HumanInputResolutionDecision, HumanInputResolutionReceipt,
    InputAttemptCheckpoint, KeepConversationInputRequest, KeepConversationInputResult,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeepReceiptV4 {
    key: String,
    principal: String,
    fingerprint: String,
    request: KeepConversationInputRequest,
    result: KeepConversationInputResult,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointV4 {
    next_queue_position: u64,
    pending: Vec<ReleasedPendingInput>,
    active: Option<ActiveInputSubmissions>,
    attempts: Vec<InputAttemptCheckpoint>,
    receipts: Vec<KeepReceiptV4>,
}

pub(super) fn upgrade(value: Value) -> Result<CanonicalHarnessSnapshot, String> {
    let old: CanonicalFrameWire<CheckpointV4> =
        serde_json::from_value(value).map_err(|error| error.to_string())?;
    if old.schema_version != 4 || old.mode != HARNESS_MODE {
        return Err("released canonical frame requires V4 harness format".into());
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
            receipts: old
                .canonical_inputs
                .receipts
                .into_iter()
                .map(|receipt| HumanInputResolutionReceipt {
                    key: receipt.key,
                    principal: receipt.principal,
                    fingerprint: receipt.fingerprint,
                    decision: HumanInputResolutionDecision::Keep {
                        request: receipt.request,
                        result: receipt.result,
                    },
                })
                .collect(),
        },
    })
}
