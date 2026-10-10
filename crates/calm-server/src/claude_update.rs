//! #2516: Update of a Claude card. An owner-created card's live child is stopped through the
//! kernel's terminal reap, but only once a dry run of the restart's own prepare accepts; then,
//! once its exit is recorded and the card has no active runtime, the card is restarted with the
//! same `claude-restart` that resumes a card whose child is gone. One sequence per card at a
//! time.

use std::time::Duration;

use axum::extract::FromRef;

use crate::error::{CalmError, Result};
use crate::ids::ActorId;
use crate::operation::OperationOutcome;
use crate::operation::claude_restart_adapter::{
    claude_restart_payload, claude_runtime_is_live, run_claude_restart,
};
use crate::state::{AppState, RouteState};

/// An anti-hang bound, not a latency contract: the reap persists the exit before it returns, so
/// a sound stop meets this at once.
const EXIT_RECORDED_BUDGET: Duration = Duration::from_secs(5);
const EXIT_RECORDED_POLL: Duration = Duration::from_millis(50);

/// Stop the card's live child, if any, then resume its latest session. Serialized per card by the
/// card's `planner_recovery_locks` entry, taken before `track_delete_locks` and the drive mutex as
/// its lock order requires; two Updates give one child at a time. A refused dry run or a
/// timeout leaves the card as it is.
pub async fn update_claude_card(
    state: &AppState,
    actor: ActorId,
    card_id: String,
) -> Result<OperationOutcome> {
    let route = RouteState::from_ref(state);
    let _card_guard =
        crate::per_card_lock::lock_card(&route.planner_recovery_locks, &card_id).await;
    let card = state
        .repo
        .card_get(&card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    // Only an owner-created card has its live child stopped: any other Claude card (a Planner
    // task worker's) keeps the dead-child restart, which refuses a live child.
    if card_is_owner_created(&card) && has_live_runtime(state, &card_id).await? {
        // Never stop a child the restart then could not replace. The restart's own prepare
        // decides, dry-run with the same payload and actor on the state the stop would leave (the
        // exit recorded and the runtime ended, as the reader writes them) and rolled back; the
        // spawn's launch check, which prepare does not see, runs first.
        let payload = claude_restart_payload(actor.clone(), card_id.clone())?;
        let dry_run_card = card_id.clone();
        state
            .operation_runtime
            .dry_run_prepare("claude-restart", &payload, move |tx| {
                Box::pin(async move { leave_as_after_stop_tx(tx, &dry_run_card).await })
            })
            .await?;
        let term = state
            .repo
            .terminal_get_by_card(&card_id)
            .await?
            .ok_or_else(|| {
                CalmError::Conflict(format!("Claude card {card_id} has no terminal to stop"))
            })?;
        // The track fence held across the stop and the wait: no viewer reattach sets up a second
        // reader on the dying child, whose exit could then reach the replacement's runtime.
        let _track_guard =
            crate::per_card_lock::lock_key(state.track_delete_locks(), card.track_id.as_str())
                .await;
        let reaped = crate::terminal_sweeper::reap_live_terminal_under_track_fence(
            state,
            &term,
            card.track_id.as_str(),
        )
        .await?;
        if !reaped && !exit_recorded(state, &term.id).await? {
            // No running child to stop and no exit on the row: nothing will record one.
            return Err(exit_not_recorded(&card_id));
        }
        wait_exit_recorded(state, &card_id, &term.id).await?;
    }
    run_claude_restart(&state.operation_runtime, actor, card_id).await
}

async fn has_live_runtime(state: &AppState, card_id: &str) -> Result<bool> {
    Ok(state
        .repo
        .session_projection_active_for_card(&card_id.to_string())
        .await?
        .is_some_and(|runtime| claude_runtime_is_live(runtime.status)))
}

/// A Claude card the owner created through the claude-cards route: its creation-time
/// `OWNER_CREATED_PAYLOAD_KEY` proves it; a task worker's card never carries it.
pub(crate) fn card_is_owner_created(card: &crate::model::Card) -> bool {
    card.kind == "claude"
        && card
            .payload
            .get(crate::validation::OWNER_CREATED_PAYLOAD_KEY)
            .and_then(serde_json::Value::as_bool)
            == Some(true)
}

async fn wait_exit_recorded(state: &AppState, card_id: &str, terminal_id: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + EXIT_RECORDED_BUDGET;
    loop {
        if exit_recorded(state, terminal_id).await? && !has_live_runtime(state, card_id).await? {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(exit_not_recorded(card_id));
        }
        tokio::time::sleep(EXIT_RECORDED_POLL).await;
    }
}

async fn exit_recorded(state: &AppState, terminal_id: &str) -> Result<bool> {
    Ok(state
        .repo
        .terminal_get(terminal_id)
        .await?
        .is_some_and(|term| term.exit_code.is_some() || term.signal_killed))
}

fn exit_not_recorded(card_id: &str) -> CalmError {
    CalmError::Conflict(format!(
        "Claude card {card_id}: its child's exit was not recorded in time; left as it is"
    ))
}

/// The state Update's stop and wait leave, written in the dry run's transaction: the launch check
/// the spawn would make, then the terminal's exit and the ended runtime as the reader writes them
/// for a child that exits on SIGTERM.
async fn leave_as_after_stop_tx(tx: &mut crate::operation::Tx<'_>, card_id: &str) -> Result<()> {
    if !crate::operation::terminal_launch::card_launch_unbound_tx(tx, card_id).await? {
        return Err(CalmError::Conflict(format!(
            "Claude card {card_id}: a task owns its terminal, so a restart could only attach; \
             its child is not stopped"
        )));
    }
    let term = crate::db::sqlite::terminal_get_by_card_tx(tx, card_id)
        .await?
        .ok_or_else(|| {
            CalmError::Conflict(format!("Claude card {card_id} has no terminal to stop"))
        })?;
    crate::db::sqlite::terminal_set_exit_with_output_tx(tx, &term.id, Some(0), false, "", false)
        .await?;
    crate::terminal_sweeper::complete_ephemeral_session_for_terminal_tx(
        tx,
        &term.id,
        crate::session_projection_repo::WorkerSessionState::Exited,
    )
    .await
}
