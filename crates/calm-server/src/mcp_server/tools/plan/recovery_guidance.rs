//! `calm.plan.list` only: the way out of a refused recovery, computed from the
//! typed refusal code plus what the failed attempt retained on disk. Not part
//! of the REST `TaskRecoveryView` wire type.
use crate::error::Result;
use crate::model::{Task, Track};
use crate::task_recovery::{AdmissionError, RecoveryRefusalCode, RefusedRecovery};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

type Tx<'a> = Transaction<'a, Sqlite>;

/// The single continuation the kernel supports for a refused recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SupportedContinuation {
    /// Declare a new task (new key); never retry the same key.
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

/// How the predecessor fence applies to this attempt: which stop proof is
/// missing decides both the continuation and what the sentence may claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PredecessorShape {
    /// The attempt selected the isolated codex route.
    pub isolated: bool,
    /// An ordinary worker card was prepared (`worker_card_id` is set).
    pub worker_prepared: bool,
}

/// Total over `RecoveryRefusalCode`: a new code cannot ship without naming
/// its continuation. `shape` distinguishes the predecessor fences.
pub(crate) fn continuation_for(
    code: RecoveryRefusalCode,
    shape: PredecessorShape,
    track: &Track,
) -> (SupportedContinuation, String) {
    match code {
        RecoveryRefusalCode::PredecessorNotQuiescent if shape.isolated => (
            SupportedContinuation::WaitForSettlement,
            "The failed isolated execution has no confirmed namespace stop; recovery re-opens only if the kernel later records the namespace stop and delivers its settlement briefing; some isolated denials are permanent (ambiguous operations or verification effects).".into(),
        ),
        RecoveryRefusalCode::PredecessorNotQuiescent if shape.worker_prepared => (
            SupportedContinuation::NewTask,
            "An ordinary worker was prepared for this key and has no stop proof; same-key recovery is permanently unavailable.".into(),
        ),
        RecoveryRefusalCode::PredecessorNotQuiescent => (
            SupportedContinuation::NewTask,
            "Verification effects or a post-preparation failure were recorded for this key with no worker stop proof; same-key recovery is permanently unavailable.".into(),
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
/// recovery of `task` (the failed current attempt). The Track comes from the
/// refusal: the row admission read under this transaction.
///
/// Admission checks the actor-dependent policy before the actor-independent
/// predecessor fence, so a policy refusal (`user_recovery`, lifecycle) can
/// mask a permanent same-key fence. Guidance re-runs that fence read-only
/// and, when it refuses, advertises the fence's continuation instead of a
/// User recovery the kernel would refuse next.
pub(crate) async fn guidance_tx(
    tx: &mut Tx<'_>,
    task: &Task,
    refused: &RefusedRecovery,
) -> Result<Value> {
    let shape = PredecessorShape {
        isolated: crate::isolated_codex::selected(task)?,
        worker_prepared: task.worker_card_id.is_some(),
    };
    let code = refused.refusal.code;
    let (mut continuation, mut blocking_condition) = continuation_for(code, shape, &refused.track);
    if matches!(
        code,
        RecoveryRefusalCode::UserAuthorizationRequired
            | RecoveryRefusalCode::RecoveryLimitReached
            | RecoveryRefusalCode::TrackNotReady
    ) {
        match crate::task_recovery::require_recoverable_predecessor_tx(tx, task).await {
            Ok(()) => {}
            Err(AdmissionError::Refused(fence))
                if fence.code == RecoveryRefusalCode::PredecessorNotQuiescent =>
            {
                let (fenced, fence_condition) = continuation_for(fence.code, shape, &refused.track);
                continuation = fenced;
                blocking_condition = format!(
                    "{blocking_condition} Independently of who asks, the same key is also blocked: {}",
                    lowercase_first(&fence_condition)
                );
            }
            Err(AdmissionError::Refused(_)) => {}
            Err(AdmissionError::Other(error)) => return Err(error),
        }
    }
    let retained = match task.worker_card_id.as_deref() {
        Some(card_id) => retained_tx(tx, card_id).await?,
        None => json!({}),
    };
    Ok(json!({
        "blocking_condition": blocking_condition,
        "supported_continuation": continuation.as_str(),
        "retained": retained,
    }))
}

fn lowercase_first(sentence: &str) -> String {
    let mut chars = sentence.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// What the failed attempt's worker card left behind, keyed by the card:
/// the latest workspace lease path (held or released — the directory may
/// still exist), the kernel commit recorded for that card, and the slice
/// branch the lease was named with when no commit was recorded. A
/// `worktree.removed` newer than the last `worktree.provisioned` means the
/// directory and slice branch are gone: only `removed` and the commit (the
/// object survives removal) are reported.
async fn retained_tx(tx: &mut Tx<'_>, card_id: &str) -> Result<Value> {
    let mut retained = json!({});
    let lease: Option<(String, String)> = sqlx::query_as(
        "SELECT path, track_id FROM workspace_leases WHERE card_id = ?1 \
         ORDER BY created_at_ms DESC, lease_id DESC LIMIT 1",
    )
    .bind(card_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((path, track_id)) = lease else {
        return Ok(retained);
    };
    let committed: Option<String> = sqlx::query_scalar(
        "SELECT payload FROM events WHERE kind = 'worktree.committed' \
         AND json_extract(payload, '$.card_id') = ?1 \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(card_id)
    .fetch_optional(&mut **tx)
    .await?;
    let committed = committed.and_then(|payload| serde_json::from_str::<Value>(&payload).ok());
    let removed = latest_worktree_event_id_tx(tx, "worktree.removed", card_id).await?;
    let provisioned = latest_worktree_event_id_tx(tx, "worktree.provisioned", card_id).await?;
    if removed.is_some_and(|removed| provisioned.is_none_or(|provisioned| removed > provisioned)) {
        retained["removed"] = json!(true);
        if let Some(payload) = committed {
            retained["last_commit"] = payload["commit_sha"].clone();
        }
        return Ok(retained);
    }
    retained["workspace_path"] = json!(path);
    match committed {
        Some(payload) => {
            retained["last_commit"] = payload["commit_sha"].clone();
            retained["branch"] = payload["branch"].clone();
        }
        None => {
            retained["branch"] = json!(
                crate::operation::workspace_lease::workspace_slice_branch_for(&track_id, card_id)?
            );
        }
    }
    Ok(retained)
}

async fn latest_worktree_event_id_tx(
    tx: &mut Tx<'_>,
    kind: &str,
    card_id: &str,
) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM events WHERE kind = ?1 \
         AND json_extract(payload, '$.card_id') = ?2 \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(kind)
    .bind(card_id)
    .fetch_optional(&mut **tx)
    .await?)
}
