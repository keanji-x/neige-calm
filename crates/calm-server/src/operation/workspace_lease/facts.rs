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
    /// row's `base_sha` (the attached repository's HEAD when the attempt was
    /// prepared). A failed attempt has one too — it never delivers, so this
    /// is the fact that can be given where `last_commit` cannot. Absent for
    /// a lease taken before the kernel recorded it. Survives removal like
    /// `last_commit` (the commit object is not the worktree's).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
    /// `true` when the kernel removed the worktree after its last provisioning, whatever the lease `state`.
    pub removed: bool,
}

/// The latest `workspace_leases` row for `worker_card_id` (any state) joined with the latest
/// `worktree.committed` event and the removed/provisioned ordering. `None` when the card never held a lease.
pub(crate) async fn worker_worktree_facts_tx(
    tx: &mut Tx<'_>,
    worker_card_id: &str,
) -> Result<Option<WorkerWorktreeFacts>> {
    let sql = format!(
        "SELECT {WORKSPACE_LEASE_COLUMNS} FROM workspace_leases \
         WHERE card_id = ?1 ORDER BY created_at_ms DESC, lease_id DESC LIMIT 1"
    );
    let row = sqlx::query(&sql)
        .bind(worker_card_id)
        .fetch_optional(&mut **tx)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let lease = row_to_workspace_lease(row)?;
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
