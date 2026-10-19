//! A manual provider compaction, serialized with sends, edits and shutdown.
use super::*;
use provider::control::CompactSubmission;

pub(super) async fn handle_compact(inner: &Arc<Inner>) -> Result<()> {
    let _issuance = inner.issuance.lock().await;
    crate::conversation_archive::require_unarchived(inner.repo.as_ref(), inner.card_id.as_str())
        .await?;
    handoff_pause::require_unpaused(inner).await?;
    if let Some(reader) = inner.backend.issuance_hold() {
        return Err(CalmError::Conflict(reader));
    }
    if inner.shutting_down.load(Ordering::SeqCst) {
        return Err(CalmError::Conflict(
            "the conversation is shutting down".into(),
        ));
    }
    if !inner.state.lock().await.can_issue_turn() {
        return Err(CalmError::Conflict(
            "Wait for the current turn to finish before compacting.".into(),
        ));
    }
    let thread = inner.thread_id.read().await.clone().ok_or_else(|| {
        CalmError::Conflict("Send a message before compacting this conversation.".into())
    })?;
    if inner.backend.thread_sealed(&thread) {
        return Err(CalmError::Conflict(
            "the conversation is being deleted".into(),
        ));
    }
    if inner.backend.active_turn_id_for_thread(&thread).is_some()
        || !inner.pending_queue.lock().await.is_empty()
        || inner.projection_client_id.lock().await.is_some()
        || inner.pending_rewind.lock().await.is_some()
    {
        return Err(CalmError::Conflict(
            "Wait for pending messages and edits to finish before compacting.".into(),
        ));
    }
    if inner.last_turn_id.lock().await.is_none() {
        return Err(CalmError::Conflict(
            "Send a message before compacting this conversation.".into(),
        ));
    }
    use crate::harness::compact_outcome::{CompactOutcome, CompactStatus};
    let previous_outcome = inner.compact_outcome.lock().await.clone();
    *inner.compact_outcome.lock().await = Some(CompactOutcome {
        operation_id: crate::model::new_id(),
        thread_id: thread.clone(),
        turn_id: None,
        status: CompactStatus::Started,
    });
    let previous = inner.state.lock().await.clone();
    *inner.state.lock().await = HarnessState::Compacting {
        since: Instant::now(),
    };
    match persist_snapshot_inner(inner, None, None).await {
        Ok(true) => {}
        result => {
            *inner.state.lock().await = previous;
            *inner.compact_outcome.lock().await = previous_outcome;
            return Err(match result {
                Err(error) => error,
                _ => CalmError::PlannerHarnessRuntimeSuperseded(
                    "The conversation changed; compaction was not submitted.".into(),
                ),
            });
        }
    }
    let submission = match inner.backend.compact_request(inner.as_ref()).await {
        Ok(request) => {
            IssueTurnHandle::from_reconciliation(inner)
                .await
                .compact_start(request)
                .await
        }
        Err(error) => Err(error),
    };
    let (state, error) = match submission {
        Ok(CompactSubmission::Submitted { .. }) => return Ok(()),
        Err(error) | Ok(CompactSubmission::NotSubmitted { error, .. }) => {
            settle(inner, CompactStatus::Refused).await;
            (previous, error)
        }
        Ok(CompactSubmission::Unconfirmed { error, .. }) => {
            settle(inner, CompactStatus::Unconfirmed).await;
            (HarnessState::Wedged { since: Instant::now(), reason: "compaction submission could not be confirmed; reset the conversation to continue".into() }, error)
        }
    };
    *inner.state.lock().await = state;
    persist_snapshot(inner).await?;
    Err(error)
}

pub(super) async fn settle(
    inner: &Arc<Inner>,
    status: crate::harness::compact_outcome::CompactStatus,
) {
    if let Some(outcome) = inner.compact_outcome.lock().await.as_mut()
        && outcome.status == crate::harness::compact_outcome::CompactStatus::Started
    {
        outcome.status = status;
    }
}

/// The user watermark at context replacement, never a provider maintenance id.
#[derive(Clone, Debug)]
pub(super) struct UsageInvalidation {
    pub user_turn_id: Option<String>,
}

pub(super) async fn invalidate_usage(inner: &Arc<Inner>, turn_id: Option<String>) {
    *inner.context_usage_invalidation.lock().await = Some(UsageInvalidation {
        user_turn_id: turn_id,
    });
}
