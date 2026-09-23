//! The worker worktree facts a Planner can read instead of being woken for them.

use serde::Serialize;

use super::{
    Tx, WORKSPACE_LEASE_COLUMNS, row_to_workspace_lease, workspace_lease_target_from_lease,
};
use crate::error::Result;

/// What `calm.plan.list` shows as `worktree` for the current attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct WorkerWorktreeFacts {
    /// The lease's worktree path, whatever the lease's `state`; omitted once
    /// the kernel has removed the worktree (`removed: true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `held` | `releasing` | `released` — the lease row's own column.
    pub state: String,
    /// The slice branch: from the latest `worktree.committed` event when there is one,
    /// otherwise the lease's own naming. Omitted once the kernel has removed the worktree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// `commit_sha` of the latest `worktree.committed` event scoped to the worker card.
    /// A FAILED auto-commit changes nothing here (the previous sha, or the absence, stays).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_commit: Option<String>,
    /// The commit the worktree started from: the lease
    /// row's `base_sha`, decided when the attempt was prepared
    /// (`base_source`): the last known upstream of the attached repository's
    /// HEAD branch when HEAD is at or behind it (`upstream`); HEAD when HEAD
    /// is ahead of it, when a shallow history leaves the relation unknown, or
    /// when the branch has no upstream (`head`). A failed attempt has one
    /// too — it never delivers, so this is the fact that can be given where
    /// `last_commit` cannot. Absent for
    /// a lease taken before the kernel recorded it. Survives removal like
    /// `last_commit` (the commit object is not the worktree's).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
    /// `true` when the kernel removed the worktree after its last provisioning, whatever the lease `state`.
    pub removed: bool,
}

/// The `workspace_leases` row `lease_id` names, in whatever state it is: the
/// delivery hand-off names the lease by id (`task_git_deliveries.lease_id`)
/// and the settlement runs after the worker released it, so the active-state
/// filter of the op-runtime readers does not apply here. `None` when no row.
pub(crate) async fn workspace_lease_by_id_tx(
    tx: &mut Tx<'_>,
    lease_id: &str,
) -> Result<Option<super::WorkspaceLease>> {
    let sql = format!("SELECT {WORKSPACE_LEASE_COLUMNS} FROM workspace_leases WHERE lease_id = ?1");
    let row = sqlx::query(&sql)
        .bind(lease_id)
        .fetch_optional(&mut **tx)
        .await?;
    row.map(row_to_workspace_lease).transpose()
}

/// Which lease rows of a card a reader wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LeaseStates {
    /// `held` or `releasing` — the lease a report transaction sees (the release runs after it).
    Active,
    /// Any state — the read surface after the worker released it.
    Any,
}

/// The latest `workspace_leases` row of `card_id` among `states`, the row `worker_worktree_facts_tx`
/// derives its facts from. `None` when the card holds no such lease.
pub(crate) async fn latest_workspace_lease_for_card_tx(
    tx: &mut Tx<'_>,
    card_id: &str,
    states: LeaseStates,
) -> Result<Option<super::WorkspaceLease>> {
    let state_filter = match states {
        LeaseStates::Active => " AND state IN ('held','releasing')",
        LeaseStates::Any => "",
    };
    let sql = format!(
        "SELECT {WORKSPACE_LEASE_COLUMNS} FROM workspace_leases \
         WHERE card_id = ?1{state_filter} ORDER BY created_at_ms DESC, lease_id DESC LIMIT 1"
    );
    let row = sqlx::query(&sql)
        .bind(card_id)
        .fetch_optional(&mut **tx)
        .await?;
    row.map(row_to_workspace_lease).transpose()
}

/// The latest `workspace_leases` row for `worker_card_id` (any state) joined with the latest
/// `worktree.committed` event and the removed/provisioned ordering. `None` when the card never held a lease.
pub(crate) async fn worker_worktree_facts_tx(
    tx: &mut Tx<'_>,
    worker_card_id: &str,
) -> Result<Option<WorkerWorktreeFacts>> {
    let Some(lease) =
        latest_workspace_lease_for_card_tx(tx, worker_card_id, LeaseStates::Any).await?
    else {
        return Ok(None);
    };
    let committed: Option<String> = sqlx::query_scalar(
        r#"SELECT payload FROM events
           WHERE scope_card = ?1 AND kind = 'worktree.committed'
           ORDER BY id DESC
           LIMIT 1"#,
    )
    .bind(worker_card_id)
    .fetch_optional(&mut **tx)
    .await?;
    let committed = committed
        .map(|payload| serde_json::from_str::<serde_json::Value>(&payload))
        .transpose()?;
    let payload_string = |field: &str| {
        committed
            .as_ref()
            .and_then(|payload| payload.get(field))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let last_commit = payload_string("commit_sha");
    let base_sha = lease.base.as_ref().map(|base| base.base_sha.clone());
    let removed = worktree_removed_after_last_provision_tx(tx, worker_card_id).await?;
    if removed {
        return Ok(Some(WorkerWorktreeFacts {
            path: None,
            state: lease.state,
            branch: None,
            last_commit,
            base_sha,
            removed: true,
        }));
    }
    let branch = match payload_string("branch") {
        Some(branch) => Some(branch),
        None => workspace_lease_target_from_lease(&lease)?.map(|target| target.branch),
    };
    Ok(Some(WorkerWorktreeFacts {
        path: Some(lease.path),
        state: lease.state,
        branch,
        last_commit,
        base_sha,
        removed: false,
    }))
}

async fn worktree_removed_after_last_provision_tx(
    tx: &mut Tx<'_>,
    worker_card_id: &str,
) -> Result<bool> {
    let latest_id = |kind: &'static str| {
        sqlx::query_scalar::<_, i64>(
            r#"SELECT id FROM events
               WHERE scope_card = ?1 AND kind = ?2
               ORDER BY id DESC
               LIMIT 1"#,
        )
        .bind(worker_card_id.to_string())
        .bind(kind)
    };
    let removed_id = latest_id("worktree.removed")
        .fetch_optional(&mut **tx)
        .await?;
    let Some(removed_id) = removed_id else {
        return Ok(false);
    };
    let provisioned_id = latest_id("worktree.provisioned")
        .fetch_optional(&mut **tx)
        .await?;
    Ok(match provisioned_id {
        Some(provisioned_id) => removed_id > provisioned_id,
        None => true,
    })
}
