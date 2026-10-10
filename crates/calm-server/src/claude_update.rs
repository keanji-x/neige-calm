//! #2516: Update of a Claude card. An owner-created card's live child is stopped through the
//! kernel's terminal reap,
//! then, once its exit is recorded and the card has no active runtime, the card is restarted
//! with the same `claude-restart` that resumes a card whose child is gone. One sequence per card
//! at a time.

use std::time::Duration;

use axum::extract::FromRef;

use crate::error::{CalmError, Result};
use crate::ids::ActorId;
use crate::operation::OperationOutcome;
use crate::operation::claude_restart_adapter::{claude_runtime_is_live, run_claude_restart};
use crate::state::{AppState, RouteState};

/// An anti-hang bound, not a latency contract: the reap persists the exit before it returns, so
/// a sound stop meets this at once.
const EXIT_RECORDED_BUDGET: Duration = Duration::from_secs(5);
const EXIT_RECORDED_POLL: Duration = Duration::from_millis(50);

/// Stop the card's live child, if any, then resume its latest session. Serialized per card by the
/// card's `planner_recovery_locks` entry, taken before the drive mutex as its lock order requires;
/// two Updates give one child at a time. On a timeout the card is left as it is.
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
    if card.kind != "claude" {
        return Err(CalmError::Forbidden(format!(
            "card {card_id} is not a Claude card"
        )));
    }
    // Only an owner-created card has its live child stopped: any other Claude card (a Planner
    // task worker's) keeps the dead-child restart, which refuses a live child.
    if card_is_owner_created(&card) && has_live_runtime(state, &card_id).await? {
        let term = state
            .repo
            .terminal_get_by_card(&card_id)
            .await?
            .ok_or_else(|| {
                CalmError::Conflict(format!("Claude card {card_id} has no terminal to stop"))
            })?;
        crate::terminal_sweeper::reap_live_terminal(state, &term).await?;
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
