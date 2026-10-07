//! Human-send recovery, sharing the reset and deletion fences. There is no
//! transcript-clearing fallback: failure always leaves the old conversation.
use crate::error::{CalmError, Result};
use crate::harness::{HarnessPhaseTag, HarnessSnapshot};
use crate::ids::CardId;
use crate::per_card_lock::lock_key;
use crate::session_projection_repo::{WorkerSessionProjection, WorkerSessionState};
use crate::state::{CodexShellState, RouteState, WorkerState};
use calm_types::harness::HARNESS_SYSTEM_ERROR_REASON;

pub(super) const RECOVERY_NOTICE: &str = "Conversation paused after a Codex error. Resolve the error shown in the conversation, then send \
    a message to resume. Your history and queued messages are retained.";

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
    let Some(snapshot) = failed_wedged_snapshot(runtime) else {
        return Ok(None);
    };
    if snapshot.wedged_reason.as_deref() != Some(HARNESS_SYSTEM_ERROR_REASON) {
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

/// Read-only projection of an unconfirmed stop. This does not grant send recovery.
pub(super) fn unconfirmed_stop_snapshot(
    runtime: &WorkerSessionProjection,
) -> Option<HarnessSnapshot> {
    let snapshot = failed_wedged_snapshot(runtime)?;
    (snapshot.wedged_reason.as_deref()
        == Some(calm_types::harness::HARNESS_INTERRUPT_TIMEOUT_REASON))
    .then_some(snapshot)
}

fn failed_wedged_snapshot(runtime: &WorkerSessionProjection) -> Option<HarnessSnapshot> {
    if runtime.status != WorkerSessionState::Failed || runtime.completed_at_ms.is_some() {
        return None;
    }
    let snapshot = HarnessSnapshot::parse_known(runtime.handle_state_json.clone()?)?;
    (snapshot.phase == HarnessPhaseTag::Wedged).then_some(snapshot)
}

/// Owner retries already-queued conversations, using the same recovery owner as human Send.
/// Normal live queues resume on their own. Paused carriers are attempted only on their original
/// thread and report a retained-history notice when the existing contract refuses recovery.
#[allow(deprecated)]
pub(super) async fn retry_provider_conversations(
    s: &RouteState,
    w: &WorkerState,
    cs: &CodexShellState,
) -> Result<Vec<super::agent_providers::ConversationRecoveryNotice>> {
    use crate::operation::planner_start_fence::CardStartFence;
    use crate::session_projection_repo::AgentProvider;
    let mut notices = Vec::new();
    for area in s.repo.areas_list().await? {
        for track in s.repo.tracks_by_area(area.id.as_str()).await? {
            if track.closed_at.is_some() {
                continue;
            }
            let cards = s.repo.cards_by_track(track.id.as_str()).await?;
            let ids = cards
                .iter()
                .map(|card| card.id.to_string())
                .collect::<Vec<_>>();
            let runtimes = s
                .repo
                .session_projection_projectable_for_cards(&ids)
                .await?;
            for card in cards {
                let Some(runtime) = runtimes.get(card.id.as_str()) else {
                    continue;
                };
                if runtime.agent_provider != Some(AgentProvider::Codex)
                    || runtime.status != WorkerSessionState::Failed
                {
                    continue;
                }
                let Some(snapshot) = failed_wedged_snapshot(runtime) else {
                    continue;
                };
                if snapshot.pending_entries().is_empty() {
                    continue;
                }
                let _fence = CardStartFence::lock(
                    &s.planner_recovery_locks,
                    &s.repo,
                    &s.operation_runtime,
                    &card.id,
                )
                .await;
                // The row found by the sweep is never permission to replace a newer carrier.
                let current = s
                    .repo
                    .session_projection_projectable_for_card(&card.id.to_string())
                    .await?;
                let Some(current) = current.filter(|current| {
                    current.id == runtime.id && current.status == WorkerSessionState::Failed
                }) else {
                    continue;
                };
                let resumable = recoverable_snapshot(s, &current).await?.is_some();
                let restored =
                    if resumable && cs.shared_codex_appserver.authentication_hold().is_none() {
                        super::planner_session::restore_failed_session(s, w, cs, current)
                            .await
                            .is_ok()
                    } else {
                        false
                    };
                if !restored {
                    notices.push(super::agent_providers::ConversationRecoveryNotice {
                        card_id:card.id.to_string(),
                        track_id:track.id.to_string(),
                        title:card.title.unwrap_or_else(||"Conversation".into()),
                        text:"This conversation still needs its recovery action. Its history and queued messages are retained.".into(),
                    });
                }
            }
        }
    }
    Ok(notices)
}
