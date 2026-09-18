//! #1727 S1 — the worker worktree facts a Planner can read instead of being
//! woken for them.
//!
//! `workspace.leased` / `worktree.provisioned` / `worktree.committed` no longer
//! push a Planner turn (`dispatcher::event_warrants_planner_push_with_role`),
//! and nothing else Planner-readable carried a worker's lease path, slice
//! branch or the commit the kernel recorded for it (the track-fs projection
//! drops those kinds, `track_vcs/delta.rs`). A Codex worker reports
//! `task.completed` BEFORE the kernel's auto `git.commit` runs, so it cannot
//! self-report that sha either. This is the one lookup that turns those rows
//! back into facts; today `calm.plan.list` renders it as `worktree`, and
//! #1727 S3 reuses it for `recovery.guidance.retained`.
//!
//! `last_commit` is exactly "the latest `worktree.committed` event scoped to
//! the card": the auto-commit after a Codex worker's `task.complete`
//! (`mcp_server::tools::emit::submit_worker_success_commit`) or a git-forge
//! `git.commit` action — both land through the forge action adapter. Commits a
//! worker makes with plain `git` are never recorded; Claude / isolated /
//! terminal workers get no auto-commit, so for them it is set only when a
//! git-forge `git.commit` ran; and a failed auto-commit after an earlier
//! successful kernel commit leaves the earlier sha in place — it is
//! indistinguishable from success here (KNOWN GAP, #1615 A).

use serde::Serialize;

use super::{Tx, row_to_workspace_lease, workspace_lease_target_from_lease};
use crate::error::Result;

/// What `calm.plan.list` shows as `worktree` for the current attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct WorkerWorktreeFacts {
    /// The lease's worktree path, whatever the lease's `state`.
    pub path: String,
    /// `held` | `releasing` | `released` — the lease row's own column.
    pub state: String,
    /// The slice branch: from the latest `worktree.committed` event when there
    /// is one, otherwise the lease's own naming (`workspace_lease_target_from_lease`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// `commit_sha` of the latest `worktree.committed` event scoped to the
    /// worker card — the latest commit the KERNEL recorded (auto-commit after
    /// a Codex worker's `task.complete`, or a git-forge `git.commit` action).
    /// Absent when the kernel never recorded one. Plain-`git` commits by the
    /// worker are never recorded, and a FAILED auto-commit changes nothing
    /// here (the previous successful sha, or the absence, stays — #1615 A).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_commit: Option<String>,
}

/// The latest `workspace_leases` row for `worker_card_id` (by `created_at_ms`,
/// any state) joined with the latest `worktree.committed` event scoped to that
/// card. `None` when the card never held a lease.
pub(crate) async fn worker_worktree_facts_tx(
    tx: &mut Tx<'_>,
    worker_card_id: &str,
) -> Result<Option<WorkerWorktreeFacts>> {
    let row = sqlx::query(
        r#"SELECT lease_id, card_id, track_id, path, state, boot_id
           FROM workspace_leases
           WHERE card_id = ?1
           ORDER BY created_at_ms DESC, lease_id DESC
           LIMIT 1"#,
    )
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
    let branch = match payload_string("branch") {
        Some(branch) => Some(branch),
        None => workspace_lease_target_from_lease(&lease)?.map(|target| target.branch),
    };
    Ok(Some(WorkerWorktreeFacts {
        path: lease.path,
        state: lease.state,
        branch,
        last_commit,
    }))
}
