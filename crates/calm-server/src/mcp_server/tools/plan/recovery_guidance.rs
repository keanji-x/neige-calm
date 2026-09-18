//! `calm.plan.list` only: the way out of a refused recovery. The refusing
//! site decided the continuation and the sentence; this module carries
//! them through and adds what the failed attempt retained on disk. Not
//! part of the REST `TaskRecoveryView` wire type.
use crate::error::Result;
use crate::model::Task;
use crate::task_recovery::{AdmissionError, RefusalSite, RefusedRecovery};
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};

type Tx<'a> = Transaction<'a, Sqlite>;

/// `{ blocking_condition, supported_continuation, retained }` for a refused
/// recovery of `task` (the failed current attempt). `blocking_condition` is
/// the refusal's reason sentence and `supported_continuation` the
/// continuation its site decided; nothing is re-derived from the code or
/// the task shape. The Track comes from the refusal: the row admission read
/// under this transaction.
///
/// Admission checks the actor-dependent policy before the actor-independent
/// contract and predecessor checks, so a policy refusal (`user_recovery`,
/// lifecycle) can mask a refusal any actor would meet next. Behind one of
/// the three policy sites, guidance re-runs those checks read-only and, when
/// they refuse, advertises THAT refusal's continuation and appends its
/// sentence; `recovery.code` stays what admission returned.
pub(crate) async fn guidance_tx(
    tx: &mut Tx<'_>,
    task: &Task,
    refused: &RefusedRecovery,
) -> Result<Value> {
    let mut continuation = refused.refusal.continuation;
    let mut blocking_condition = refused.refusal.reason.clone();
    if matches!(
        refused.refusal.site,
        RefusalSite::PlannerOutsideAutoDeclare
            | RefusalSite::PlannerRetryLimit
            | RefusalSite::TrackNotReady
    ) {
        match crate::task_recovery::admit_contract_and_predecessor_tx(tx, &refused.track, task)
            .await
        {
            Ok(_) => {}
            Err(AdmissionError::Refused(tail)) => {
                continuation = tail.continuation;
                blocking_condition = join_independent(&blocking_condition, &tail.reason);
            }
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

/// `{policy}. Independently of who asks: {tail}` — the policy reason ends
/// its sentence first: a full stop is added unless it already ends with `.`
/// or `;`.
fn join_independent(policy: &str, tail: &str) -> String {
    let stop = if policy.ends_with(['.', ';']) {
        ""
    } else {
        "."
    };
    format!("{policy}{stop} Independently of who asks: {tail}")
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

#[cfg(test)]
mod tests {
    use super::join_independent;

    #[test]
    fn join_independent_ends_the_policy_sentence_before_the_tail() {
        assert_eq!(
            join_independent("an explicit User recovery is required", "tail"),
            "an explicit User recovery is required. Independently of who asks: tail"
        );
        assert_eq!(
            join_independent("already ended.", "tail"),
            "already ended. Independently of who asks: tail"
        );
        assert_eq!(
            join_independent("clause;", "tail"),
            "clause; Independently of who asks: tail"
        );
    }
}
