//! `calm.plan.cancel` past `pending` (#1785 S1): a `running` codex/claude Track worker is
//! canceled and its worker marked for the same reap as a liveness timeout. Everything else past
//! `pending` is refused with the current status.

use crate::db::sqlite::{task_cancel_running_tx, task_get_tx};
use crate::error::CalmError;
use crate::model::{Task, TaskStatus, now_ms};
use crate::operation::Tx;
use crate::scheduler::{
    PLANNER_CANCELED, WorkerCleanupReason, mark_running_timeout_cleanup_tx,
    task_has_running_liveness_deadline,
};

/// Keyed refusal sentences (`prompts/plan-cancel/refusals.md`); Rust only maps a refusal to a key.
const REFUSALS: &str = include_str!("../../../../prompts/plan-cancel/refusals.md");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    Dispatched,
    Verifying,
    Ended,
    Changed,
    Route,
    Isolated,
    Unbound,
}

impl Refusal {
    #[cfg(test)]
    const ALL: [Self; 7] = [
        Self::Dispatched,
        Self::Verifying,
        Self::Ended,
        Self::Changed,
        Self::Route,
        Self::Isolated,
        Self::Unbound,
    ];

    const fn key(self) -> &'static str {
        match self {
            Self::Dispatched => "dispatched",
            Self::Verifying => "verifying",
            Self::Ended => "ended",
            Self::Changed => "changed",
            Self::Route => "route",
            Self::Isolated => "isolated",
            Self::Unbound => "unbound",
        }
    }

    /// The refusal for a row the running CAS did not move, by its current status.
    const fn for_status(status: TaskStatus) -> Self {
        match status {
            TaskStatus::Dispatched => Self::Dispatched,
            TaskStatus::Verifying => Self::Verifying,
            TaskStatus::Done | TaskStatus::Failed | TaskStatus::Canceled => Self::Ended,
            TaskStatus::Pending | TaskStatus::Running => Self::Changed,
        }
    }

    fn sentence(self) -> &'static str {
        REFUSALS
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.split_once('\t'))
            .find(|(key, _)| *key == self.key())
            .map_or(self.key(), |(_, sentence)| sentence)
    }
}

pub(super) fn status_str(status: TaskStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn refused(key: &str, status: TaskStatus, refusal: Refusal) -> CalmError {
    CalmError::Conflict(format!(
        "task {key} is {}: {}",
        status_str(status),
        refusal.sentence()
    ))
}

/// A refusal whose sentence matches the row's status: only a `running` row is refused for its
/// route, backend or card; any other status is refused as that status.
fn refused_for(key: &str, current: &Task, running_refusal: Refusal) -> CalmError {
    let refusal = if current.status == TaskStatus::Running {
        running_refusal
    } else {
        Refusal::for_status(current.status)
    };
    refused(key, current.status, refusal)
}

/// Cancel the current execution `current` (read in this tx) if it is a running Track worker the
/// sweep can reap: CAS `running → canceled` pinned to its card, plus the card's cleanup marker.
/// Returns rows moved; `0` means a concurrent cancel already canceled it (the caller's idempotent
/// path). Any other state the CAS did not move is refused with its current status.
pub(super) async fn cancel_running_in_tx(
    tx: &mut Tx<'_>,
    current: &Task,
    key: &str,
) -> Result<u64, CalmError> {
    if !task_has_running_liveness_deadline(current) {
        return Err(refused_for(key, current, Refusal::Route));
    }
    if crate::isolated_codex::lookup::is_isolated_task_tx(tx, &current.id).await? {
        return Err(refused_for(key, current, Refusal::Isolated));
    }
    let Some(card_id) = current.worker_card_id.as_deref() else {
        return Err(refused_for(key, current, Refusal::Unbound));
    };
    let now = now_ms();
    let rows = task_cancel_running_tx(tx, &current.id, card_id, PLANNER_CANCELED, now).await?;
    if rows == 0 {
        let status = task_get_tx(tx, &current.id)
            .await?
            .map_or(current.status, |row| row.status);
        if status == TaskStatus::Canceled {
            return Ok(0);
        }
        return Err(refused(key, status, Refusal::for_status(status)));
    }
    let marked = mark_running_timeout_cleanup_tx(
        tx,
        card_id,
        &current.id,
        now,
        WorkerCleanupReason::PlannerCanceled,
    )
    .await?;
    if marked == 0 {
        tracing::warn!(
            task_id = %current.id,
            card_id,
            "plan_cancel: no live worker session to mark; the canceled worker is not reaped"
        );
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_refusal_has_a_sentence_without_the_old_scope_note() {
        for refusal in Refusal::ALL {
            let sentence = refusal.sentence();
            assert_ne!(
                sentence,
                refusal.key(),
                "{refusal:?} has no line in refusals.md"
            );
            assert!(!sentence.contains("#644"), "{refusal:?}: {sentence}");
        }
    }
}
