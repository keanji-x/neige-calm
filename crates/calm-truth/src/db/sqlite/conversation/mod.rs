//! Canonical fact storage. These primitives borrow the owner's transaction: the
//! acceptance/claim owner must commit the sole harness queue, authority checks,
//! receipts and events with these facts. They neither enqueue nor drive a retry.
mod acceptance;
mod attempts;
mod carrier;
mod claim_storage;
mod claim_validation;
mod claims;
mod creation;
pub(super) mod messages;
mod pending_edit;
mod plans;
pub(super) mod presentation;
mod release;
mod runtime_frame;
mod storage_contract;

pub use acceptance::{AcceptedPendingInput, accept_pending_human_tx};
pub use attempts::{begin_input_attempt_tx, record_input_attempt_fact_tx};
pub use carrier::{
    CurrentInputCarrier, current_input_carrier_tx, current_input_content_carrier_tx,
    current_input_fact_carrier_tx,
};
pub use claims::{
    ClaimedSubmission, claim_initial_submission_tx, claim_supplemental_submission_tx,
};
pub use creation::{
    HumanTrackCreationAcceptance, HumanTrackCreationAck, HumanTrackCreationRecord,
    human_track_creation_get_tx, human_track_creation_insert_tx,
};
pub use messages::{
    StoredMessage, StoredMessageRevision, append_unclaimed_human_revision_tx,
    erase_message_revisions_tx, insert_message_tx, message_tx, revision_tx,
};
pub use pending_edit::{edit_pending_human_tx, withdraw_pending_human_tx};
pub use plans::{
    OriginalInputExecution, StoredSubmissionIdentity, StoredSubmissionPlan,
    erase_submission_render_payloads_tx, human_plan_for_pending_prefix_tx,
    original_input_execution_tx, submission_identity_tx, submission_plan_tx,
};
pub use presentation::{
    PresentationSource, StoredNativePresentation, allocate_presentation_tx, observe_native_item_tx,
};
pub use release::release_submission_tx;
pub use runtime_frame::{
    RuntimeExecutionFence, RuntimeFrameAfterWrite, complete_input_execution_tx,
    persist_runtime_frame_tx,
};
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
    match contract.as_deref() {
        Some("private") => Ok(ConversationHistoryContract::Private),
        Some("shared_history") => Ok(ConversationHistoryContract::SharedHistory),
        None => Err(CalmError::NotFound(format!("conversation {card_id}"))),
        Some(_) => Err(CalmError::Internal(
            "unknown conversation history contract".into(),
        )),
    }
}

#[cfg(test)]
mod end_transition_tests;
#[cfg(test)]
mod relationship_tests;
#[cfg(test)]
mod runtime_frame_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod transaction_tests;
