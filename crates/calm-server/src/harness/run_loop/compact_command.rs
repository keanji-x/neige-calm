//! A manual provider compaction, serialized with sends, edits and shutdown.
use super::*;

pub(super) async fn handle_compact(inner: &Arc<Inner>) -> Result<()> {
    let _issuance = inner.issuance.lock().await;
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
    let previous = inner.state.lock().await.clone();
    *inner.state.lock().await = HarnessState::Compacting {
        since: Instant::now(),
    };
    match persist_snapshot_inner(inner, None, None).await {
        Ok(true) => {}
        result => {
            *inner.state.lock().await = previous;
            return Err(match result {
                Err(error) => error,
                _ => CalmError::PlannerHarnessRuntimeSuperseded(
                    "The conversation changed; compaction was not submitted.".into(),
                ),
            });
        }
    }
    match inner.backend.compact_start(&thread).await {
        Ok(()) => Ok(()),
        Err(error) => {
            *inner.state.lock().await = if matches!(
                error,
                CalmError::CodexRefused(_) | CalmError::BadRequest(_)
            ) {
                previous
            } else {
                HarnessState::Wedged { since: Instant::now(), reason: "compaction submission could not be confirmed; reset the conversation to continue".into() }
            };
            persist_snapshot(inner).await?;
            Err(error)
        }
    }
}
