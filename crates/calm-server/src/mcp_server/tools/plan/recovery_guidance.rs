//! `calm.plan.list` only: the way out of a refused recovery.
use crate::error::Result;
use crate::model::Task;
use crate::operation::Tx;
use crate::operation::workspace_lease::facts::WorkerWorktreeFacts;
use crate::task_recovery::{AdmissionError, RefusalSite, RefusedRecovery};
use serde_json::{Value, json};

/// A policy refusal can mask a refusal any actor would meet next; behind a policy
/// site, guidance re-runs the contract/predecessor checks read-only and advertises
/// that refusal's continuation, while `recovery.code` stays what admission returned.
pub(crate) async fn guidance_tx(
    tx: &mut Tx<'_>,
    task: &Task,
    refused: &RefusedRecovery,
    worktree: Option<WorkerWorktreeFacts>,
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
    let retained = worktree.map_or_else(|| json!({}), retained_from_facts);
    Ok(json!({
        "blocking_condition": blocking_condition,
        "supported_continuation": continuation.as_str(),
        "retained": retained,
    }))
}

fn join_independent(policy: &str, tail: &str) -> String {
    let stop = if policy.ends_with(['.', ';']) {
        ""
    } else {
        "."
    };
    format!("{policy}{stop} Independently of who asks: {tail}")
}

/// The lease `state` is not carried — `retained` says what is left, not what the lease row says.
fn retained_from_facts(facts: WorkerWorktreeFacts) -> Value {
    let WorkerWorktreeFacts {
        path,
        state: _,
        branch,
        last_commit,
        base_sha,
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
    if let Some(base_sha) = base_sha {
        retained["base_sha"] = json!(base_sha);
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
