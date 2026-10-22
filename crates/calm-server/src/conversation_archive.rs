//! Conversation-level archive admission; provider sessions keep their original identity.
use crate::db::RouteRepo;
use crate::error::{CalmError, Result};

const CONVERSATION_BUSY: &str =
    "Wait for the reply and queued messages to finish before archiving this conversation.";

/// Canonical acceptance does not take the live issuance fence. Its committed
/// carrier must be idle in the same transaction that writes archive metadata.
pub(crate) async fn require_canonical_idle_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
) -> Result<()> {
    use crate::db::sqlite::conversation::{
        card_has_canonical_storage, current_input_content_carrier_tx,
    };
    if card_has_canonical_storage(tx, card_id).await? {
        let carrier = current_input_content_carrier_tx(tx, card_id).await?;
        let snapshot: crate::harness::snapshot::CanonicalHarnessSnapshot =
            serde_json::from_value(carrier.snapshot().clone())?;
        if !snapshot.idle_for_lifecycle() {
            return Err(CalmError::Conflict(CONVERSATION_BUSY.into()));
        }
    }
    Ok(())
}

pub(crate) async fn require_unarchived(repo: &dyn RouteRepo, card_id: &str) -> Result<()> {
    require_not_archived(repo.conversation_archived_at(card_id).await?)
}

pub(crate) async fn require_unarchived_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
) -> Result<()> {
    Ok(crate::db::sqlite::require_conversation_unarchived_tx(tx, card_id).await?)
}

fn require_not_archived(archived: Option<i64>) -> Result<()> {
    if archived.is_some() {
        return Err(CalmError::Conflict(
            "This conversation is archived. Restore it before continuing.".into(),
        ));
    }
    Ok(())
}

pub(crate) fn require_idle(snapshot: &crate::harness::HarnessSnapshot) -> Result<()> {
    use crate::harness::HarnessPhaseTag;
    if !snapshot.pending_entries().is_empty()
        || !matches!(
            snapshot.phase,
            HarnessPhaseTag::Idle | HarnessPhaseTag::TurnCompleted | HarnessPhaseTag::Resumed
        )
    {
        return Err(CalmError::Conflict(CONVERSATION_BUSY.into()));
    }
    Ok(())
}
