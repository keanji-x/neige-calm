//! Canonical fact storage. These primitives borrow the owner's transaction: the
//! acceptance/claim owner must commit the sole harness queue, authority checks,
//! receipts and events with these facts. They neither enqueue nor drive a retry.
mod acceptance;
mod assistant_creation;
mod attempts;
mod carrier;
mod claim_storage;
mod claim_validation;
mod claims;
mod creation;
pub use assistant_creation::{
    AssistantCreationRecord, assistant_creation_get_tx, assistant_creation_insert_tx,
};
pub(super) mod imported;
pub(super) mod messages;
mod plans;
pub(super) mod presentation;
pub use imported::import_item_tx;
mod release;
mod retry;
pub(in crate::db::sqlite) use retry::retry_availability_tx;
pub use retry::{
    RetryUnavailableReason, StoredRetryIntent, retry_intent_tx, submission_retry_intent_tx,
};
mod resolution;
pub use resolution::{
    keep_conversation_input_tx, replay_retry_conversation_input_tx, retry_conversation_input_tx,
};
mod runtime_frame;
mod storage_contract;

pub use acceptance::{
    AcceptedPendingInput, accept_pending_human_tx, accept_pending_system_tx,
    event_system_observation_id_tx,
};
pub use attempts::{begin_input_attempt_tx, record_input_attempt_fact_tx};
pub use carrier::{
    CurrentCarrier, CurrentInputCarrier, current_input_carrier_tx,
    current_input_content_carrier_tx, current_input_fact_carrier_tx, current_native_fact_tx,
};
pub use claims::{ClaimedSubmission, InitialClaim, claim_initial_submission_tx};
pub use creation::{
    HumanTrackCreationAcceptance, HumanTrackCreationAck, HumanTrackCreationRecord,
    human_track_creation_get_tx, human_track_creation_insert_tx,
};
pub use messages::{
    StoredMessage, StoredMessageRevision, insert_message_tx, message_tx, revision_tx,
};
pub use plans::{
    OriginalInputExecution, StoredSubmissionIdentity, StoredSubmissionPlan, current_turn_input_tx,
    human_plan_for_pending_prefix_tx, original_initial_submission_tx, original_input_execution_tx,
    pending_plan_for_prefix_tx, submission_dispatch_proof_tx, submission_identity_tx,
    submission_plan_tx,
};
pub use presentation::{
    PresentationSource, StoredNativePresentation, allocate_presentation_tx, observe_native_item_tx,
};
pub use release::release_submission_tx;
pub use runtime_frame::{
    RuntimeExecutionFence, RuntimeFrameAfterWrite, complete_input_execution_tx,
    persist_runtime_frame_tx,
};
pub(in crate::db::sqlite) use storage_contract::CANONICAL_STORAGE_PREDICATE;
pub use storage_contract::card_has_canonical_storage;

use crate::error::{CalmError, Result};
use calm_types::conversation::ConversationHistoryContract;
use sha2::{Digest, Sha256};
use sqlx::{Sqlite, Transaction};

pub(super) fn digest(value: &impl serde::Serialize) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

/// Current containment facts, including System Areas, without inventing a
/// membership decision. The kernel's source owner checks current authority.
pub async fn conversation_area_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
) -> Result<crate::model::Area> {
    let area_id: String = sqlx::query_scalar(
        "SELECT t.area_id FROM cards c JOIN tracks t ON t.id=c.track_id WHERE c.id=?1",
    )
    .bind(card_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| CalmError::NotFound(format!("conversation {card_id}")))?;
    super::area_membership::area_get(tx, &area_id).await
}

pub async fn history_contract_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
) -> Result<ConversationHistoryContract> {
    let contract: Option<String> = sqlx::query_scalar(
        "SELECT a.conversation_history_contract FROM cards c
         JOIN tracks t ON t.id=c.track_id JOIN areas a ON a.id=t.area_id WHERE c.id=?1",
    )
    .bind(card_id)
    .fetch_optional(&mut **tx)
    .await?;
    decode_history_contract(contract, &format!("conversation {card_id}"))
}

pub async fn area_history_contract_tx(
    tx: &mut Transaction<'_, Sqlite>,
    area_id: &str,
) -> Result<ConversationHistoryContract> {
    let contract =
        sqlx::query_scalar("SELECT conversation_history_contract FROM areas WHERE id=?1")
            .bind(area_id)
            .fetch_optional(&mut **tx)
            .await?;
    decode_history_contract(contract, &format!("area {area_id}"))
}

fn decode_history_contract(
    contract: Option<String>,
    resource: &str,
) -> Result<ConversationHistoryContract> {
    match contract.as_deref() {
        Some("private") => Ok(ConversationHistoryContract::Private),
        Some("shared_history") => Ok(ConversationHistoryContract::SharedHistory),
        None => Err(CalmError::NotFound(resource)),
        Some(_) => Err(CalmError::Internal(
            "unknown conversation history contract".into(),
        )),
    }
}

#[cfg(test)]
mod end_transition_tests;
#[cfg(test)]
mod held_projection_tests;
#[cfg(test)]
mod relationship_tests;
#[cfg(test)]
mod runtime_frame_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod transaction_tests;

#[cfg(test)]
mod resolution_tests;

#[cfg(test)]
mod retry_storage_tests;

#[cfg(test)]
mod retry_tests;

#[cfg(test)]
mod retry_lifecycle_tests;

mod carrier_replacement;
pub use carrier_replacement::{
    CallerRevision, CarrierReplacement, CarrierReplacementGuard, ExpectedCarrier,
    ReplacementRefusal, check_idle_carrier_tx, replace_idle_carrier_tx,
};
#[cfg(test)]
mod carrier_replacement_tests;

mod carrier_revision;
pub use carrier_revision::{CarrierIngressRevision, carrier_ingress_revision_tx};

mod acp_cleanup;
pub use acp_cleanup::settle_kept_acp_receipt_tx;

#[cfg(test)]
mod planner_switch_tests;

#[cfg(test)]
mod u9_canonical_turn_owner_tests;

#[cfg(test)]
mod imported_tests;

mod system_assistant;
pub use system_assistant::{
    assistant_key_used_tx, system_assistant_accept_tx, system_assistant_entry_insert_tx,
    system_assistant_receipt_replay_tx, system_assistant_receipt_tx,
};
