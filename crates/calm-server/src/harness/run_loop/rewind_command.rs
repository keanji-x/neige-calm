//! `HarnessObservationCommand::Rewind` (#1923): remove the conversation's latest turn and hand its
//! user input back. Runs on the run loop under the issuance lock, so no turn starts, no notification
//! lands and no shutdown persists while it decides, commits and adopts.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::{Inner, RewoundTurn, clear_interruption_intent, harness_event_scope, snapshot_for};
use crate::db::write_with_event_typed;
use crate::error::{CalmError, Result};
use crate::event::Event;
use crate::harness::backend::RewindTarget;
use crate::harness::rewind::plan;
use crate::harness::snapshot::HarnessPhaseTag;
use crate::harness::state::{HarnessState, run_status_for};
use crate::ids::ActorId;
use crate::state::WriteContext;

fn refused(reason: impl std::fmt::Display) -> CalmError {
    CalmError::Conflict(format!("{reason}; nothing was changed"))
}

/// Check, prepare with the provider, commit in one transaction, then adopt the committed values
/// in memory so no later snapshot rebuild overwrites them.
pub(super) async fn handle_rewind(inner: &Arc<Inner>, turn_id: &str) -> Result<RewoundTurn> {
    let _issuance = inner.issuance.lock().await;
    if inner.shutting_down.load(Ordering::SeqCst) {
        return Err(refused("the conversation is shutting down"));
    }
    if !inner.state.lock().await.can_issue_turn() {
        return Err(refused(
            "a turn is still running or being sent; edit once it has finished",
        ));
    }
    let Some(thread_id) = inner.thread_id.read().await.clone() else {
        return Err(refused("this conversation has no turn yet"));
    };
    if inner.backend.thread_sealed(&thread_id) {
        return Err(refused("this conversation is being deleted"));
    }
    // The phase alone does not prove the provider is quiet: `Resumed` falls back to `Idle` on a timer.
    if inner
        .backend
        .active_turn_id_for_thread(&thread_id)
        .is_some()
    {
        return Err(refused(
            "the provider still reports a running turn; edit once it has finished",
        ));
    }
    if !inner.pending_queue.lock().await.is_empty()
        || inner.projection_client_id.lock().await.is_some()
    {
        return Err(refused(
            "messages are still waiting to be sent; edit once they have gone out",
        ));
    }
    if inner.last_turn_id.lock().await.as_deref() != Some(turn_id) {
        return Err(refused("only the latest turn can be edited"));
    }
    if inner.pending_rewind.lock().await.is_some() {
        return Err(refused(
            "the previous edit has not been sent yet; send the edited message first",
        ));
    }
    let rows = inner
        .repo
        .transcript_rows_of_thread(inner.card_id.as_str(), &thread_id)
        .await?;
    let plan = plan(&rows, turn_id).map_err(refused)?;
    let prepared = inner
        .backend
        .prepare_rewind(
            &thread_id,
            RewindTarget {
                turn_id,
                prompt_client_id: plan.prompt_client_id.as_deref(),
                previous_turn: plan
                    .previous_turn_id
                    .as_ref()
                    .map(|_| plan.previous_turn_rows.as_slice()),
            },
        )
        .await
        .map_err(|error| match error {
            CalmError::Conflict(reason) => refused(reason),
            other => other,
        })?;

    let state = match &plan.previous_turn_id {
        Some(previous) => HarnessState::TurnCompleted {
            last_turn_id: previous.clone(),
        },
        None => HarnessState::Idle,
    };
    let mut snapshot = snapshot_for(inner).await;
    snapshot.phase = HarnessPhaseTag::from(&state);
    snapshot.last_turn_id = plan.previous_turn_id.clone();
    snapshot.pending_rewind = Some(prepared.clone());
    // The provider forgets the turn's since-last-turn block with the turn, so the watermark goes
    // back to where that block started. A turn issued before `last_turn_base` existed has none:
    // its watermark stays advanced (transitional gap, one turn per upgraded conversation).
    let restored_head = inner
        .last_turn_base
        .lock()
        .await
        .clone()
        .filter(|base| base.turn_id == turn_id)
        .map(|base| base.seen_head);
    if let Some(head) = &restored_head {
        snapshot.last_seen_head = head.clone();
    }
    snapshot.last_turn_base = None;
    if snapshot
        .interruption_intent
        .as_ref()
        .is_some_and(|intent| intent.turn_id == turn_id)
    {
        snapshot.interruption_intent = None;
    }
    let snapshot_value = serde_json::to_value(&snapshot)?;
    let status = run_status_for(&state);
    let worker_session_id = inner.worker_session_id.clone();
    let card_id = inner.card_id.clone();
    let track_id = inner.track_id.clone();
    let removed_turn = turn_id.to_string();
    let committed_thread = thread_id.clone();
    let (boundary, last_row, planned_rows) = (plan.boundary, plan.last_row, plan.row_count);
    write_with_event_typed(
        inner.repo.as_ref(),
        ActorId::Kernel,
        harness_event_scope(inner, "harness.transcript.rewound"),
        None,
        &inner.events,
        &WriteContext::new(
            inner.card_role_cache.clone(),
            inner.track_area_cache.clone(),
        ),
        move |tx| {
            Box::pin(async move {
                let removed_item_count = crate::db::sqlite::transcript_delete_thread_suffix_tx(
                    tx,
                    card_id.as_str(),
                    &committed_thread,
                    boundary,
                    last_row,
                )
                .await?;
                if removed_item_count != planned_rows {
                    return Err(refused(
                        "the conversation changed while the edit was prepared",
                    ));
                }
                // Conditional on this runtime still being the card's carrier: zero rows is a
                // refusal, and returning it rolls the deletion back.
                if !crate::db::sqlite::session_set_handle_state_tx(
                    tx,
                    &worker_session_id,
                    Some(snapshot_value),
                )
                .await?
                {
                    return Err(refused(
                        "this conversation is no longer the card's active session",
                    ));
                }
                crate::db::sqlite::session_set_harness_observation_runtime_tx(
                    tx,
                    &worker_session_id,
                    status,
                    Some(&committed_thread),
                    None,
                )
                .await?;
                Ok((
                    (),
                    Event::HarnessTranscriptRewound {
                        worker_session_id,
                        card_id,
                        track_id,
                        turn_id: removed_turn,
                        removed_item_count,
                    },
                ))
            })
        },
    )
    .await?;

    *inner.state.lock().await = state;
    *inner.last_turn_id.lock().await = plan.previous_turn_id;
    *inner.pending_rewind.lock().await = Some(prepared);
    if let Some(head) = restored_head {
        *inner.last_seen_head.lock().await = head;
    }
    *inner.last_turn_base.lock().await = None;
    clear_interruption_intent(inner, turn_id).await;
    inner.rewound_turns.lock().await.insert(turn_id.to_string());
    // The ordinary writer announces a phase change (rewinding a first turn leaves `idle`); the
    // rewind itself is already committed, so a failure here is only logged.
    if let Err(error) = super::persist_snapshot(inner).await {
        tracing::warn!(
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            turn_id,
            %error,
            "planner harness rewound a turn but could not persist the snapshot after it"
        );
    }
    Ok(RewoundTurn {
        turn_id: turn_id.to_string(),
        input: plan.input,
    })
}
