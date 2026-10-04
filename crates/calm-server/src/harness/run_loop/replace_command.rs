//! `HarnessObservationCommand::Replace` (#1923, #2043): remove the conversation's latest turn and
//! queue the message that replaces it, in one transaction that also binds the send's key. Runs on
//! the run loop under the issuance lock, so no turn starts, no notification lands and no shutdown
//! persists while it decides, commits and adopts. The provider drops the turn when the next turn
//! starts (`pending_rewind`), so the commit is the whole of the op's own effect.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex as StdMutex};

use super::{
    DurableAck, DurableSendBinding, HarnessObservationDelivery, Inner, SendKey,
    clear_interruption_intent, harness_event_scope, restore_durable_user_message, snapshot_for,
    stage_durable_entries,
};
use crate::db::write_with_event_typed;
use crate::error::{CalmError, Result};
use crate::event::Event;
use crate::harness::backend::RewindTarget;
use crate::harness::queue::QueueEntry;
use crate::harness::rewind::plan;
use crate::harness::snapshot::HarnessPhaseTag;
use crate::harness::state::{HarnessState, run_status_for};
use crate::ids::ActorId;
use crate::state::WriteContext;

/// Decided before anything was written, or rolled back with the transaction: nothing changed and
/// no key was bound, so the reader is told why.
fn refused(reason: impl std::fmt::Display) -> CalmError {
    CalmError::PlannerTurnNotReplaceable(format!("{reason}; nothing was changed"))
}

/// Check, prepare with the provider, queue `entry` in memory, commit the removal, the message and
/// the key in one transaction, then adopt the committed values in memory so no later snapshot
/// rebuild overwrites them. Any failure puts the queue back as it was.
pub(super) async fn handle_replace(
    inner: &Arc<Inner>,
    turn_id: &str,
    entry: QueueEntry,
    key: &SendKey,
) -> Result<DurableAck> {
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
    // An empty queue is also what lets the message below be accepted whole: no fold, no eviction.
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
    // Reached only when a replacement left the queue without starting a turn (deleted by its
    // reader): its cut still waits for the next turn, and two cuts do not combine.
    if inner.pending_rewind.lock().await.is_some() {
        return Err(refused(
            "an earlier edit removed a turn that the next message has not replaced yet; send a \
             message first",
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

    let (ack, checkpoint) =
        stage_durable_entries(inner, vec![HarnessObservationDelivery { entry }]).await?;

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
    let committed = match serde_json::to_value(&snapshot) {
        Ok(snapshot_value) => {
            commit(
                inner,
                Commit {
                    thread_id,
                    turn_id: turn_id.to_string(),
                    boundary: plan.boundary,
                    last_row: plan.last_row,
                    planned_rows: plan.row_count,
                    snapshot_value,
                    status: run_status_for(&state),
                    send: DurableSendBinding::new(inner, key, ack.entry_id.as_ref()),
                },
            )
            .await
        }
        Err(error) => Err(error.into()),
    };
    if let Err(error) = committed {
        restore_durable_user_message(inner, checkpoint).await;
        return Err(error);
    }

    *inner.state.lock().await = state;
    *inner.last_turn_id.lock().await = plan.previous_turn_id;
    *inner.pending_rewind.lock().await = Some(prepared);
    if let Some(head) = restored_head {
        *inner.last_seen_head.lock().await = head;
    }
    *inner.last_turn_base.lock().await = None;
    clear_interruption_intent(inner, turn_id).await;
    inner.rewound_turns.lock().await.insert(turn_id.to_string());
    // The ordinary writer announces a phase change (replacing a first turn leaves `idle`); the
    // replace itself is already committed, so a failure here is only logged.
    if let Err(error) = super::persist_snapshot(inner).await {
        tracing::warn!(
            worker_session_id = %inner.worker_session_id,
            card_id = %inner.card_id,
            turn_id,
            %error,
            "planner harness replaced a turn but could not persist the snapshot after it"
        );
    }
    Ok(ack)
}

/// What the one transaction writes.
struct Commit {
    thread_id: String,
    turn_id: String,
    boundary: i64,
    last_row: i64,
    planned_rows: i64,
    snapshot_value: serde_json::Value,
    status: crate::session_projection_repo::WorkerSessionState,
    send: DurableSendBinding,
}

/// The turn's rows go, the snapshot holding the cut and the message is written, and the key is
/// bound to the message, or none of it happens. A refusal inside the transaction rolls it back; its
/// reason travels beside the error, since the storage layer keeps only the message of an error it
/// has no kind for.
async fn commit(inner: &Arc<Inner>, commit: Commit) -> Result<()> {
    let Commit {
        thread_id,
        turn_id,
        boundary,
        last_row,
        planned_rows,
        snapshot_value,
        status,
        send,
    } = commit;
    let worker_session_id = inner.worker_session_id.clone();
    let card_id = inner.card_id.clone();
    let track_id = inner.track_id.clone();
    let refusal = Arc::new(StdMutex::new(None::<&'static str>));
    let refuse = {
        let refusal = Arc::clone(&refusal);
        move |reason: &'static str| {
            *refusal.lock().expect("refusal slot") = Some(reason);
            refused(reason)
        }
    };
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
                    &thread_id,
                    boundary,
                    last_row,
                )
                .await?;
                if removed_item_count != planned_rows {
                    return Err(refuse(
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
                    return Err(refuse(
                        "this conversation is no longer the card's active session",
                    ));
                }
                crate::db::sqlite::session_set_harness_observation_runtime_tx(
                    tx,
                    &worker_session_id,
                    status,
                    Some(&thread_id),
                    None,
                )
                .await?;
                crate::db::sqlite::planner_input_bind_tx(
                    tx,
                    card_id.as_str(),
                    &send.idempotency_key,
                    &send.binding,
                )
                .await?;
                Ok((
                    (),
                    Event::HarnessTranscriptRewound {
                        worker_session_id,
                        card_id,
                        track_id,
                        turn_id,
                        removed_item_count,
                    },
                ))
            })
        },
    )
    .await
    .map(|((), _event_id)| ())
    .map_err(|error| match refusal.lock().expect("refusal slot").take() {
        Some(reason) => refused(reason),
        None => error,
    })
}
