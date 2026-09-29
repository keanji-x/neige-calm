//! The worker worktree facts a Planner can read instead of being woken for them.

use serde::Serialize;

use super::{Tx, WORKSPACE_LEASE_COLUMNS, row_to_workspace_lease, worker_branch_tx};
use crate::error::Result;

/// What `calm.plan.list` shows as `worktree` for the current attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct WorkerWorktreeFacts {
    /// The lease's worktree path, whatever the lease's `state`.
    pub path: String,
    /// `held` | `releasing` | `released` — the lease row's own column.
    pub state: String,
    /// The branch: from the latest `worktree.committed` event when there is one, otherwise the
    /// track's worker branch (#1830 S2 D4) when the lease is at the track's checkout. Omitted
    /// otherwise (a pre-S2 per-card lease).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// `commit_sha` of the latest `worktree.committed` event scoped to the worker card.
    /// A FAILED auto-commit changes nothing here (the previous sha, or the absence, stays).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_commit: Option<String>,
    /// The commit the attempt started from: the lease row's `base_sha`, the track checkout's HEAD
    /// when the attempt was prepared (#1830 S2). Absent for a lease taken before the kernel
    /// recorded it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
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

/// The latest `workspace_leases` row of `card_id`, in any state (the read surface after the
/// worker released it), the row `worker_worktree_facts_tx` derives its facts from. `None` when the
/// card never held a lease.
pub(crate) async fn latest_workspace_lease_for_card_tx(
    tx: &mut Tx<'_>,
    card_id: &str,
) -> Result<Option<super::WorkspaceLease>> {
    let sql = format!(
        "SELECT {WORKSPACE_LEASE_COLUMNS} FROM workspace_leases \
         WHERE card_id = ?1 ORDER BY created_at_ms DESC, lease_id DESC LIMIT 1"
    );
    let row = sqlx::query(&sql)
        .bind(card_id)
        .fetch_optional(&mut **tx)
        .await?;
    row.map(row_to_workspace_lease).transpose()
}

/// The latest `workspace_leases` row for `worker_card_id` (any state) joined with the latest
/// `worktree.committed` event. `None` when the card never held a lease.
pub(crate) async fn worker_worktree_facts_tx(
    tx: &mut Tx<'_>,
    worker_card_id: &str,
) -> Result<Option<WorkerWorktreeFacts>> {
    let Some(lease) = latest_workspace_lease_for_card_tx(tx, worker_card_id).await? else {
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
    let branch = match payload_string("branch") {
        Some(branch) => Some(branch),
        None => {
            // #1830 S2 D4: the track's worker branch names only a lease at the track's current
            // checkout; a per-card lease from before S2 ran on a branch its row does not record.
            let track_id = crate::ids::TrackId::from(lease.track_id.clone());
            let track = crate::db::sqlite::track_get_tx(tx, &track_id).await?;
            if track.workspace.agent_cwd() == lease.path {
                Some(worker_branch_tx(tx, &lease.track_id).await?)
            } else {
                None
            }
        }
    };
    Ok(Some(WorkerWorktreeFacts {
        path: lease.path,
        state: lease.state,
        branch,
        last_commit,
        base_sha,
    }))
}
