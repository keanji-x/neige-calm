//! Exact own-pending mutation. Queue position never changes on edit/withdraw;
//! absence of an effective claim is necessary but never establishes pending.
use super::carrier::save_input_carrier_tx;
use super::{
    CurrentInputCarrier, append_unclaimed_human_revision_tx, erase_message_revisions_tx, message_tx,
};
use crate::error::{CalmError, Result};
use crate::model::now_ms;
use calm_types::conversation::{
    HumanPayload, MessageAuthor, MessageRevisionRef, PendingInput, PrivateHistoryOperationId,
};
use sqlx::{Sqlite, Transaction};

pub async fn edit_pending_human_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &mut CurrentInputCarrier,
    authenticated_user_id: &str,
    expected: &MessageRevisionRef,
    payload: HumanPayload,
) -> Result<MessageRevisionRef> {
    let position = carrier
        .checkpoint
        .pending
        .iter()
        .position(|entry| matches!(entry,PendingInput::Human { message,.. } if message==expected))
        .ok_or_else(|| CalmError::Conflict("exact human revision is not pending"))?;
    let current = message_tx(tx, &expected.message_id).await?;
    if current.card_id != carrier.card_id {
        return Err(CalmError::Conflict(
            "pending edit crosses canonical conversation ownership",
        ));
    }
    let revision =
        append_unclaimed_human_revision_tx(tx, expected, authenticated_user_id, payload).await?;
    let PendingInput::Human { message, .. } = &mut carrier.checkpoint.pending[position] else {
        unreachable!("validated human pending entry")
    };
    *message = revision.reference.clone();
    save_input_carrier_tx(tx, carrier).await?;
    Ok(revision.reference)
}

pub async fn withdraw_pending_human_tx(
    tx: &mut Transaction<'_, Sqlite>,
    carrier: &mut CurrentInputCarrier,
    authenticated_user_id: &str,
    expected: &MessageRevisionRef,
    operation_id: &PrivateHistoryOperationId,
) -> Result<()> {
    let position = carrier
        .checkpoint
        .pending
        .iter()
        .position(|entry| matches!(entry,PendingInput::Human { message,.. } if message==expected))
        .ok_or_else(|| CalmError::Conflict("exact human revision is not pending"))?;
    let current = message_tx(tx, &expected.message_id).await?;
    if current.card_id != carrier.card_id
        || current.current != *expected
        || current.withdrawn_at_ms.is_some()
    {
        return Err(CalmError::Conflict(
            "pending input changed before withdrawal",
        ));
    }
    if current.author
        != (MessageAuthor::AuthenticatedHuman {
            registered_user_id: authenticated_user_id.into(),
        })
    {
        return Err(CalmError::Forbidden(
            "only the original human author can withdraw this input".into(),
        ));
    }
    let claimed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM conversation_submission_inputs WHERE message_id=?1 AND effective=1)")
        .bind(expected.message_id.as_str()).fetch_one(&mut **tx).await?;
    if claimed {
        return Err(CalmError::Conflict("claimed input cannot be withdrawn"));
    }
    let changed=sqlx::query("UPDATE conversation_messages SET withdrawn_at_ms=?1 WHERE id=?2 AND current_revision=?3 AND withdrawn_at_ms IS NULL")
        .bind(now_ms()).bind(expected.message_id.as_str()).bind(i64::from(expected.revision.get())).execute(&mut **tx).await?;
    if changed.rows_affected() != 1 {
        return Err(CalmError::Conflict(
            "pending input changed during withdrawal",
        ));
    }
    erase_message_revisions_tx(tx, &expected.message_id, operation_id).await?;
    sqlx::query(
        "UPDATE conversation_presentations
         SET erased_by_operation_id=?1
         WHERE card_id=?2 AND message_id=?3 AND erased_by_operation_id IS NULL",
    )
    .bind(operation_id.as_str())
    .bind(&carrier.card_id)
    .bind(expected.message_id.as_str())
    .execute(&mut **tx)
    .await?;
    carrier.checkpoint.pending.remove(position);
    save_input_carrier_tx(tx, carrier).await
}
