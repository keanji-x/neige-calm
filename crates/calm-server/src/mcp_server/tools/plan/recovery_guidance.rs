//! `calm.plan.list` only: the way out of a refused recovery, computed from the
//! typed refusal code plus what the failed attempt retained on disk. Not part
//! of the REST `TaskRecoveryView` wire type.
use crate::error::Result;
use crate::model::{Task, Track};
use crate::task_recovery::{RecoveryRefusal, RecoveryRefusalCode};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

type Tx<'a> = Transaction<'a, Sqlite>;

/// The single continuation the kernel supports for a refused recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SupportedContinuation {
    /// Declare a new task (new key) that starts from the retained worktree;
    /// never retry the same key.
    NewTask,
    /// Only an explicit User recovery can allocate another execution.
    UserRecovery,
    /// The isolated predecessor is still stopping; its settlement briefing
    /// re-opens the decision.
    WaitForSettlement,
    /// No kernel-supported continuation for this refusal.
    None,
}

impl SupportedContinuation {
    fn as_str(self) -> &'static str {
        match self {
            Self::NewTask => "new_task",
            Self::UserRecovery => "user_recovery",
            Self::WaitForSettlement => "wait_for_settlement",
            Self::None => "none",
        }
    }
}

/// Total over `RecoveryRefusalCode`: a new code cannot ship without naming
/// its continuation. `isolated` distinguishes the two predecessor fences.
pub(crate) fn continuation_for(
    code: RecoveryRefusalCode,
    isolated: bool,
    track: &Track,
) -> (SupportedContinuation, String) {
    match code {
        RecoveryRefusalCode::PredecessorNotQuiescent if isolated => (
            SupportedContinuation::WaitForSettlement,
            "The failed isolated execution has no confirmed namespace stop yet; recovery becomes available once the kernel records the stop and delivers its settlement briefing.".into(),
        ),
        RecoveryRefusalCode::PredecessorNotQuiescent => (
            SupportedContinuation::NewTask,
            "An ordinary worker was prepared for this key and has no stop proof; same-key recovery is permanently unavailable.".into(),
        ),
        RecoveryRefusalCode::UserAuthorizationRequired => (
            SupportedContinuation::UserRecovery,
            "Planner recovery is not authorized for this task; an explicit User recovery is required.".into(),
        ),
        RecoveryRefusalCode::RecoveryLimitReached => (
            SupportedContinuation::UserRecovery,
            "The bounded Planner recovery for this key was consumed; only an explicit User recovery can allocate another execution.".into(),
        ),
        RecoveryRefusalCode::TrackNotReady => (
            SupportedContinuation::None,
            format!(
                "Track lifecycle is {:?}; it does not schedule work, so recovery cannot be admitted until the Track is working again.",
                track.lifecycle
            ),
        ),
        RecoveryRefusalCode::NotAuthorized => (
            SupportedContinuation::None,
            "The requesting actor may not recover this task.".into(),
        ),
        RecoveryRefusalCode::UnsupportedSpawn => (
            SupportedContinuation::None,
            "Child-task routes are not recoverable.".into(),
        ),
        RecoveryRefusalCode::DeclarationWithdrawn => (
            SupportedContinuation::None,
            "The task declaration was withdrawn, is not ready, or is invalid; same-contract recovery has nothing current to honour.".into(),
        ),
        RecoveryRefusalCode::MissingFrozenContract => (
            SupportedContinuation::None,
            "The failed execution carries no complete frozen contract; same-contract recovery is unavailable.".into(),
        ),
        RecoveryRefusalCode::ContractChanged => (
            SupportedContinuation::None,
            "The frozen contract no longer matches the current report or its inputs; same-contract recovery is unavailable.".into(),
        ),
        RecoveryRefusalCode::RecoveryLineageMissing => (
            SupportedContinuation::None,
            "The accepted recovery lost its allocation or predecessor row.".into(),
        ),
    }
}

/// `{ blocking_condition, supported_continuation, retained }` for a refused
/// recovery of `task` (the failed current attempt).
pub(crate) async fn guidance_tx(
    tx: &mut Tx<'_>,
    track: &Track,
    task: &Task,
    refusal: &RecoveryRefusal,
) -> Result<Value> {
    let isolated = crate::isolated_codex::selected(task)?;
    let (continuation, blocking_condition) = continuation_for(refusal.code, isolated, track);
    Ok(json!({
        "blocking_condition": blocking_condition,
        "supported_continuation": continuation.as_str(),
        "retained": retained_tx(tx, task).await?,
    }))
}

/// What the failed attempt's worker card left behind: the latest workspace
/// lease path (held or released — the directory may still exist), the kernel
/// commit recorded for that card, and the slice branch the lease was named
/// with when no commit was recorded.
async fn retained_tx(tx: &mut Tx<'_>, task: &Task) -> Result<Value> {
    let mut retained = json!({});
    let Some(card_id) = task.worker_card_id.as_deref() else {
        return Ok(retained);
    };
    let lease: Option<(String, String)> = sqlx::query_as(
        "SELECT path, state FROM workspace_leases WHERE card_id = ?1 AND track_id = ?2 \
         ORDER BY created_at_ms DESC, lease_id DESC LIMIT 1",
    )
    .bind(card_id)
    .bind(&task.track_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((path, _state)) = lease else {
        return Ok(retained);
    };
    retained["workspace_path"] = json!(path);
    let committed: Option<String> = sqlx::query_scalar(
        "SELECT payload FROM events WHERE kind = 'worktree.committed' \
         AND json_extract(payload, '$.card_id') = ?1 \
         AND json_extract(payload, '$.track_id') = ?2 \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(card_id)
    .bind(&task.track_id)
    .fetch_optional(&mut **tx)
    .await?;
    let committed = committed.and_then(|payload| serde_json::from_str::<Value>(&payload).ok());
    match committed {
        Some(payload) => {
            retained["last_commit"] = payload["commit_sha"].clone();
            retained["branch"] = payload["branch"].clone();
        }
        None => {
            retained["branch"] = json!(
                crate::operation::workspace_lease::workspace_slice_branch_for(
                    &task.track_id,
                    card_id
                )?
            );
        }
    }
    Ok(retained)
}
