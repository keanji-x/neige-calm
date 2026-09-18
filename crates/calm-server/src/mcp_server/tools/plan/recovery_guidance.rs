//! `calm.plan.list` only: the way out of a refused recovery. The refusing
//! site decided the continuation and the sentence; this module carries
//! them through and adds what the failed attempt retained on disk. Not
//! part of the REST `TaskRecoveryView` wire type.
use crate::error::Result;
use crate::model::Task;
use crate::operation::Tx;
use crate::operation::workspace_lease::facts::{WorkerWorktreeFacts, worker_worktree_facts_tx};
use crate::task_recovery::{AdmissionError, RefusalSite, RefusedRecovery};
use serde_json::{Value, json};

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
        Some(card_id) => worker_worktree_facts_tx(tx, card_id)
            .await?
            .map_or_else(|| json!({}), retained_from_facts),
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

/// What the failed attempt's worker card left behind, as `retained`: the
/// same facts `calm.plan.list` renders as `worktree`
/// (`operation::workspace_lease::facts`), keyed the way the Planner prompt
/// names them. `workspace_path` is the lease path (held or released — the
/// directory may still exist), `branch` the slice branch, `last_commit` the
/// kernel commit recorded for that card; every field is optional. A
/// `worktree.removed` newer than the last `worktree.provisioned` means the
/// directory and slice branch are gone: only `removed: true` and the commit
/// (the object survives removal) are reported. The lease `state` is not
/// carried — `retained` says what is left, not what the lease row says.
fn retained_from_facts(facts: WorkerWorktreeFacts) -> Value {
    let WorkerWorktreeFacts {
        path,
        state: _,
        branch,
        last_commit,
        removed,
    } = facts;
    let mut retained = json!({});
    if removed {
        retained["removed"] = json!(true);
    }
    if let Some(path) = path {
        retained["workspace_path"] = json!(path);
    }
    if let Some(branch) = branch {
        retained["branch"] = json!(branch);
    }
    if let Some(last_commit) = last_commit {
        retained["last_commit"] = json!(last_commit);
    }
    retained
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
