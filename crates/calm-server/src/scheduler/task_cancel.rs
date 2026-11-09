//! `neige_task_cancel` past `pending` (#1785 S1): a `running` codex/claude Track worker is
//! canceled and its worker marked for the same reap as a liveness timeout. Everything else past
//! `pending` is refused with the current status.

use crate::db::sqlite::{task_cancel_running_tx, task_get_tx};
use crate::error::CalmError;
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::model::{Task, TaskStatus, now_ms};
use crate::operation::Tx;
use crate::scheduler::{
    PLANNER_CANCELED, WorkerCleanupReason, mark_running_timeout_cleanup_tx,
    task_has_running_liveness_deadline,
};

/// Keyed refusal sentences (`prompts/plan-cancel/refusals.md`); Rust only maps a refusal to a key.
const REFUSALS: &str = include_str!("../../prompts/plan-cancel/refusals.md");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    Dispatched,
    Verifying,
    Ended,
    Changed,
    Route,
    Unbound,
}

impl Refusal {
    #[cfg(test)]
    const ALL: [Self; 6] = [
        Self::Dispatched,
        Self::Verifying,
        Self::Ended,
        Self::Changed,
        Self::Route,
        Self::Unbound,
    ];

    const fn key(self) -> &'static str {
        match self {
            Self::Dispatched => "dispatched",
            Self::Verifying => "verifying",
            Self::Ended => "ended",
            Self::Changed => "changed",
            Self::Route => "route",
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

pub(crate) fn status_str(status: TaskStatus) -> String {
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
/// Returns rows moved — `0` means a concurrent cancel already canceled it (the caller's idempotent
/// path) — and the lease events when no live session was left to mark and the lease was released
/// here. Any other state the CAS did not move is refused with its current status.
pub(crate) async fn cancel_running_in_tx(
    tx: &mut Tx<'_>,
    current: &Task,
    key: &str,
) -> Result<(u64, Vec<(ActorId, EventScope, Event)>), CalmError> {
    if !task_has_running_liveness_deadline(current) {
        return Err(refused_for(key, current, Refusal::Route));
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
            return Ok((0, Vec::new()));
        }
        return Err(refused(key, status, Refusal::for_status(status)));
    }
    let mark = mark_running_timeout_cleanup_tx(
        tx,
        card_id,
        &current.id,
        now,
        WorkerCleanupReason::PlannerCanceled,
    )
    .await?;
    if mark.marked == 0 {
        tracing::warn!(
            task_id = %current.id,
            card_id,
            "neige_task_cancel: no live worker session to mark; the canceled worker is not reaped"
        );
    }
    Ok((rows, mark.released))
}

pub(crate) async fn require_worker_binding_in_tx(
    tx: &mut Tx<'_>,
    task: &crate::model::Task,
) -> Result<(), CalmError> {
    if task.spawn != calm_types::task_recovery::TASK_IN_TRACK_ROUTE {
        return Err(CalmError::Conflict(
            "close-subtree-uncancellable-work".into(),
        ));
    }
    let Some(card) = task.worker_card_id.as_deref() else {
        return Err(CalmError::Conflict("close-subtree-unbound-worker".into()));
    };
    let provider = match task.kind {
        crate::model::TaskKind::Codex => "codex",
        crate::model::TaskKind::Claude => "claude",
        _ => {
            return Err(CalmError::Conflict(
                "close-subtree-uncancellable-work".into(),
            ));
        }
    };
    let session: Option<String> = sqlx::query_scalar(
        "SELECT ws.id FROM cards c JOIN worker_sessions ws ON ws.id=c.session_id \
         WHERE c.id=?1 AND c.track_id=?2 AND c.role='worker' AND ws.card_id=c.id AND ws.track_id=c.track_id \
           AND ws.contract='executor' AND ws.provider=?3 AND ws.state IN ('starting','running','idle','turn_pending')",
    ).bind(card).bind(&task.track_id).bind(provider).fetch_optional(&mut **tx).await?;
    let Some(session_id) = session else {
        return Err(CalmError::Conflict("close-subtree-unbound-worker".into()));
    };
    let session = crate::db::sqlite::session_get_tx(tx, &session_id.into())
        .await?
        .ok_or_else(|| CalmError::Conflict("close-subtree-unbound-worker".into()))?;
    let bound = calm_truth::worker_binding::session_owns_attempt_tx(tx, &session, &task.id, card)
        .await?
        == Some(true);
    if !bound {
        return Err(CalmError::Conflict("close-subtree-unbound-worker".into()));
    }
    let runtime = crate::db::sqlite::session_projection_by_id_tx(tx, &session.id.0)
        .await?
        .ok_or_else(|| CalmError::Conflict("close-subtree-unbound-worker".into()))?;
    let terminal = crate::db::sqlite::terminal_get_by_card_tx(tx, card).await?;
    crate::harness::backend::factory::PlannerFactory::require_worker_stop_binding(
        &runtime,
        terminal.as_ref(),
    )?;
    Ok(())
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
