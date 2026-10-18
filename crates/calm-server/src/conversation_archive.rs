//! Conversation-level archive admission; provider sessions keep their original identity.
use crate::db::RouteRepo;
use crate::error::{CalmError, Result};

pub(crate) async fn require_unarchived(repo: &dyn RouteRepo, card_id: &str) -> Result<()> {
    if repo.conversation_archived_at(card_id).await?.is_some() {
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
        return Err(CalmError::Conflict(
            "Wait for the reply and queued messages to finish before archiving this conversation."
                .into(),
        ));
    }
    Ok(())
}
