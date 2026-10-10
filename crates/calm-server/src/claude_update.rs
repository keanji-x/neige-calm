//! #2516: Update of a Claude card. An owner-created card's live child is stopped through the
//! kernel's terminal reap, but only once the restart is proven possible; then, once its exit is
//! recorded and the card has no active runtime, the card is restarted with the same
//! `claude-restart` that resumes a card whose child is gone. One sequence per card at a time.

use std::time::Duration;

use axum::extract::FromRef;

use crate::error::{CalmError, Result};
use crate::ids::ActorId;
use crate::operation::OperationOutcome;
use crate::operation::claude_restart_adapter::{
    claude_runtime_is_live, restart_prerequisites_tx, run_claude_restart,
};
use crate::state::{AppState, RouteState};

/// An anti-hang bound, not a latency contract: the reap persists the exit before it returns, so
/// a sound stop meets this at once.
const EXIT_RECORDED_BUDGET: Duration = Duration::from_secs(5);
const EXIT_RECORDED_POLL: Duration = Duration::from_millis(50);

/// Stop the card's live child, if any, then resume its latest session. Serialized per card by the
/// card's `planner_recovery_locks` entry, taken before `track_delete_locks` and the drive mutex as
/// its lock order requires; two Updates give one child at a time. A refused preflight or a
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
        // Never stop a child the restart then could not replace: the restart's own checks, and a
        // terminal its spawn would start rather than only attach to, hold first.
        let repo = state.repo.clone();
        let preflight_card = card_id.clone();
        crate::db::write_in_tx_typed(state.repo.as_ref(), move |tx| {
            Box::pin(async move {
                restart_prerequisites_tx(tx, repo.as_ref(), &preflight_card).await?;
                if !crate::operation::terminal_launch::card_launch_unbound_tx(tx, &preflight_card)
                    .await?
                {
                    return Err(CalmError::Conflict(format!(
                        "Claude card {preflight_card}: a task owns its terminal, so a restart \
                         could only attach; its child is not stopped"
                    )));
                }
                Ok(())
            })
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
        crate::terminal_sweeper::reap_live_terminal_under_track_fence(
            state,
            &term,
            card.track_id.as_str(),
        )
        .await?;
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
        let exited = state
            .repo
            .terminal_get(terminal_id)
            .await?
            .is_some_and(|term| term.exit_code.is_some() || term.signal_killed);
        if exited && !has_live_runtime(state, card_id).await? {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(CalmError::Conflict(format!(
                "Claude card {card_id}: its child's exit was not recorded in time; left as it is"
            )));
        }
        tokio::time::sleep(EXIT_RECORDED_POLL).await;
    }
}
