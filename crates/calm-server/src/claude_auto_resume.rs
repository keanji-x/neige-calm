//! #2516: after the boot reconcile, each owner-created Claude card whose PTY was lost is resumed
//! through the card's Update entry point, as its restart route resumes it. A Planner task
//! worker's card is left to its Planner, which owns that recovery.

use std::collections::HashSet;

use crate::error::Result;
use crate::ids::ActorId;
use crate::operation::OperationOutcome;
use crate::state::AppState;

/// Run off the boot path: the restarts start PTYs, and their Claude hooks need the server
/// listening. The handle is detached on purpose.
pub fn spawn_on_boot(
    state: &AppState,
    stale_terminal_ids: Vec<String>,
) -> tokio::task::JoinHandle<()> {
    let state = state.clone();
    tokio::spawn(async move { resume_owner_claude_cards(&state, &stale_terminal_ids).await })
}

/// Run the card's Update (`update_claude_card`) once per owner-created Claude card among
/// `stale_terminal_ids` (the terminals the boot reconcile just marked exited); with no child to
/// stop it is the dead-child restart. A failure is logged and leaves the card exited, to be
/// resumed by hand.
pub async fn resume_owner_claude_cards(state: &AppState, stale_terminal_ids: &[String]) {
    let mut resumed = HashSet::new();
    for terminal_id in stale_terminal_ids {
        let card_id = match owner_claude_card_of(state, terminal_id).await {
            Ok(Some(card_id)) => card_id,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(
                    terminal_id,
                    %error,
                    "claude auto-resume could not classify a stale terminal's card; left exited"
                );
                continue;
            }
        };
        if !resumed.insert(card_id.clone()) {
            continue;
        }
        // Operation recovery may have finished a restart the crash interrupted: resume only a
        // terminal that is still exited, at submit time.
        match terminal_still_exited(state, terminal_id).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::info!(
                    card_id,
                    terminal_id,
                    "claude card already running again; not resumed"
                );
                continue;
            }
            Err(error) => {
                tracing::warn!(
                    card_id,
                    %error,
                    "claude auto-resume could not re-read the card's terminal; left as it is"
                );
                continue;
            }
        }
        // The card's Update entry point: under its per-card lock, and with no child to stop.
        match crate::claude_update::update_claude_card(state, ActorId::Kernel, card_id.clone())
            .await
        {
            Ok(OperationOutcome::Succeeded { .. })
            | Ok(OperationOutcome::SucceededViaCollision { .. }) => {
                tracing::info!(card_id, "claude card resumed after its PTY was lost");
            }
            Ok(outcome) => tracing::warn!(
                card_id,
                ?outcome,
                "claude auto-resume did not succeed; the card stays exited"
            ),
            Err(error) => tracing::warn!(
                card_id,
                %error,
                "claude auto-resume was refused; the card stays exited"
            ),
        }
    }
}

/// Whether `terminal_id`'s row still records an exit and no child runs on it.
async fn terminal_still_exited(state: &AppState, terminal_id: &str) -> Result<bool> {
    let Some(term) = state.repo.terminal_get(terminal_id).await? else {
        return Ok(false);
    };
    if term.exit_code.is_none() && !term.signal_killed {
        return Ok(false);
    }
    // An unanswered or failed probe resumes nothing: a restart cannot stop a child it cannot see.
    let probe = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        crate::probe_supervisor_for_terminal(state, terminal_id),
    )
    .await;
    Ok(matches!(probe, Ok(Ok(false))))
}

/// The card of `terminal_id` when it is a Claude card created through the claude-cards route: the
/// creation-time `OWNER_CREATED_PAYLOAD_KEY` proves it, so a task worker's card never qualifies.
async fn owner_claude_card_of(state: &AppState, terminal_id: &str) -> Result<Option<String>> {
    let Some(term) = state.repo.terminal_get(terminal_id).await? else {
        return Ok(None);
    };
    let card_id = term.card_id.to_string();
    let Some(card) = state.repo.card_get(&card_id).await? else {
        return Ok(None);
    };
    let owner_created = crate::claude_update::card_is_owner_created(&card);
    Ok(owner_created.then_some(card_id))
}

#[cfg(test)]
mod tests {
    /// Boot resumes from the reconcile's own stale set, and only once operation recovery has run.
    #[test]
    fn main_resumes_after_operation_recovery_from_the_reconcile_result() {
        let main_rs = include_str!("main.rs");
        let reconcile = main_rs
            .find("let stale_terminals = calm_server::reconcile_supervisor_on_boot(&state).await;")
            .expect("main boot keeps the reconcile's stale terminals");
        let recover = main_rs
            .find("recover_operations_on_boot(&state).await?")
            .expect("main boot recovers operations");
        let resume = main_rs
            .find("claude_auto_resume::spawn_on_boot(")
            .expect("main boot resumes owner-created Claude cards");
        assert!(reconcile < recover && recover < resume);
        let call = &main_rs[resume..];
        let call = &call[..call.find(')').expect("call closes")];
        assert!(call.contains("stale_terminals"), "{call}");
    }
}
