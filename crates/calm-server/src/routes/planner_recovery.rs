//! Human-send recovery, sharing the reset and deletion fences. There is no
//! transcript-clearing fallback: failure always leaves the old conversation.
use crate::error::{CalmError, Result};
use crate::harness::{HarnessPhaseTag, HarnessSnapshot};
use crate::ids::CardId;
use crate::per_card_lock::lock_key;
use crate::session_projection_repo::{WorkerSessionProjection, WorkerSessionState};
use crate::state::{CodexShellState, RouteState, WorkerState};
use calm_types::harness::HARNESS_SYSTEM_ERROR_REASON;

pub(super) const RECOVERY_NOTICE: &str = "Conversation paused after a Codex error. Resolve the error shown in the conversation, then send a message to resume. Your history and queued messages are retained.";

pub(super) async fn candidate(
    s: &RouteState,
    card_id: &CardId,
    human_send: bool,
) -> Result<Option<WorkerSessionProjection>> {
    let runtime = s
        .repo
        .session_projection_projectable_for_card(&card_id.to_string())
        .await?;
    let Some(runtime) = runtime else {
        return Ok(None);
    };
    if runtime.status.is_active_authority()
        || (human_send && recoverable_snapshot(s, &runtime).await?.is_some())
    {
        Ok(Some(runtime))
    } else {
        Ok(None)
    }
}

#[allow(deprecated)] // Legacy event persistence requires the raw cache handles.
pub(super) async fn recover(
    s: &RouteState,
    w: &WorkerState,
    cs: &CodexShellState,
    runtime: WorkerSessionProjection,
) -> Result<WorkerSessionProjection> {
    let card = s
        .repo
        .card_get(&runtime.card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound("conversation no longer exists".into()))?;
    let _delete = lock_key(&s.track_delete_locks, card.track_id.as_str()).await;
    let track = s
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound("conversation track no longer exists".into()))?;
    if !crate::workspace_recycle::workspace_allows_runtime_recovery(&track) {
        return Err(CalmError::Conflict(
            "Restore this conversation's workspace before resuming; history is retained.".into(),
        ));
    }
    if !cs.shared_codex_appserver.is_running() {
        return Err(CalmError::ServiceUnavailable(
            cs.shared_codex_appserver.not_running_message(),
        ));
    }
    let thread = crate::harness::effective_runtime_thread_id(&runtime).ok_or_else(|| {
        CalmError::Conflict("original conversation thread is missing; history is retained".into())
    })?;
    if let Some(old) = s.harness.get(&runtime.id) {
        old.quiesce_system_error_for_recovery().await?;
        s.harness.remove(&runtime.id);
    }
    // Quiescence may have waited for a completion to restore dropped steers.
    // Use that settled durable queue, not the earlier candidate's snapshot.
    let runtime = w
        .repo
        .session_projection_by_id(&runtime.id)
        .await?
        .ok_or_else(|| CalmError::Conflict("conversation changed during recovery".into()))?;
    let outcome = cs
        .shared_codex_appserver
        .resume_system_error_conversation(&runtime, &thread)
        .await?;
    if let Some((item_db_id, turn_id)) = outcome {
        s.repo
            .log_pure_event(
                crate::ids::ActorId::Kernel,
                crate::event::EventScope::Card {
                    card: card.id.clone(),
                    track: track.id.clone(),
                    area: track.area_id.clone(),
                },
                None,
                &s.events,
                s.write.role_cache(),
                s.write.area_cache(),
                crate::event::Event::HarnessItemAdded {
                    worker_session_id: runtime.id.clone(),
                    card_id: card.id.clone(),
                    track_id: track.id.clone(),
                    item_db_id,
                    item_uuid: None,
                    item_type: None,
                    turn_id: Some(turn_id),
                    method: "turn/completed".into(),
                },
            )
            .await?;
    }
    // The ordinary spawn boundary rechecks deletion and current ownership after
    // this guard drops. The caller retains its per-card recovery lock through
    // registration AND durable enqueue, as it does for lazy boot recovery.
    w.repo
        .session_projection_by_id(&runtime.id)
        .await?
        .ok_or_else(|| CalmError::Conflict("conversation changed during recovery".into()))
}

pub(super) async fn recoverable_snapshot(
    s: &RouteState,
    runtime: &WorkerSessionProjection,
) -> Result<Option<HarnessSnapshot>> {
    if runtime.status != WorkerSessionState::Failed || runtime.completed_at_ms.is_some() {
        return Ok(None);
    }
    let Some(snapshot) = runtime
        .handle_state_json
        .clone()
        .and_then(HarnessSnapshot::parse_known)
    else {
        return Ok(None);
    };
    if snapshot.phase != HarnessPhaseTag::Wedged
        || snapshot.wedged_reason.as_deref() != Some(HARNESS_SYSTEM_ERROR_REASON)
    {
        return Ok(None);
    }
    let Some(thread) = crate::harness::effective_runtime_thread_id(runtime) else {
        return Ok(None);
    };
    if !s
        .repo
        .session_projection_system_error_recovery_matches(runtime, &thread)
        .await?
    {
        return Ok(None);
    }
    Ok(Some(snapshot))
}
