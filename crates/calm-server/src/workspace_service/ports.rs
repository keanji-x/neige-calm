//! Server adapters for workspace-runtime policy.
use crate::db::sqlite::{CheckoutOccupancy, checkout_occupancy};
use crate::error::{CalmError, Result};
use crate::operation::workspace_lease::upstream::{
    LeaseStart, choose_lease_start, diverged_refusal, head_upstream, set_branch_upstream,
};
use async_trait::async_trait;
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use workspace_runtime::worktree_clean::{ClosedWorktree, ClosedWorktreeFacts};
use workspace_runtime::{sweep::CandidateRefs, track_worktree::TrackBranch};

#[derive(Clone, Copy)]
pub(crate) struct ServerWorkspacePorts;

#[async_trait]
impl TrackBranch for ServerWorkspacePorts {
    type Error = CalmError;

    async fn refresh_upstream(&self, repo_root: &Path) {
        crate::operation::workspace_lease::upstream_fetch::refresh_upstream(repo_root).await;
    }

    fn prepare_branch(&self, repo_root: &Path, branch: &str) -> Result<String> {
        let base = track_worktree_base(repo_root)?;
        let upstream = head_upstream(repo_root)?;
        set_branch_upstream(repo_root, branch, upstream.as_ref())?;
        Ok(base)
    }
}

impl CandidateRefs for ServerWorkspacePorts {
    fn delete_for_track(&self, track_id: &str, git_common_dirs: &[PathBuf]) {
        crate::git_candidate::refs::delete_candidate_refs_for_track(
            track_id,
            git_common_dirs.iter().map(PathBuf::as_path),
        );
    }
}

/// Where a new track branch starts: behind → upstream, ahead → HEAD, diverged → refused.
fn track_worktree_base(repo_root: &Path) -> Result<String> {
    match choose_lease_start(repo_root)? {
        LeaseStart::Head { sha } | LeaseStart::Upstream { sha } => Ok(sha),
        LeaseStart::Diverged {
            head,
            upstream,
            unpushed,
            ahead,
            behind,
        } => Err(diverged_refusal(
            repo_root, &head, &upstream, unpushed, ahead, behind,
        )),
    }
}

pub(crate) fn observe(point: &str, repo_root: &Path) {
    #[cfg(feature = "fixtures")]
    crate::test_seams::blocking_pause_point(point, repo_root.to_str().expect("test repo path"));
    #[cfg(not(feature = "fixtures"))]
    let _ = (point, repo_root);
}

/// Closed tracks with a worktree and no agent session that may be working in it: the Planner and
/// conversation agents run there too, and a closed track's idle Planner must not block the clean.
/// Tasks, leases and deliveries are [`checkout_occupancy`]'s to judge.
pub(crate) const CLOSED_TRACK_WORKTREES_SQL: &str = "SELECT t.* \
       FROM tracks t \
      WHERE t.closed_at IS NOT NULL AND t.workspace_worktree_path IS NOT NULL \
        AND NOT EXISTS (SELECT 1 FROM worker_sessions ws WHERE ws.track_id = t.id \
                         AND ws.state IN ('starting','running','turn_pending')) \
      ORDER BY t.id";

pub(crate) struct ServerClosedWorktreeFacts {
    pub(crate) pool: SqlitePool,
}

#[async_trait]
impl ClosedWorktreeFacts for ServerClosedWorktreeFacts {
    type Error = CalmError;

    async fn closed_idle_worktrees(&self) -> Result<Vec<ClosedWorktree>> {
        Ok(
            sqlx::query_as::<_, calm_truth::db::rows::TrackRow>(CLOSED_TRACK_WORKTREES_SQL)
                .fetch_all(&self.pool)
                .await?
                .into_iter()
                .map(|row| {
                    let track = crate::model::Track::from(row);
                    ClosedWorktree {
                        track_id: track.id.to_string(),
                        workspace: track.workspace,
                        closed_at: track.closed_at.expect("closed candidate"),
                    }
                })
                .collect(),
        )
    }

    async fn checkout_free(&self, track: &str) -> Result<bool> {
        Ok(
            checkout_occupancy(&mut *self.pool.acquire().await?, track, "").await?
                == CheckoutOccupancy::Free,
        )
    }

    async fn executions_settled(&self, track: &str) -> Result<bool> {
        Ok(
            calm_truth::db::sqlite::workspace_execution::unsettled_executions(
                &mut *self.pool.acquire().await?,
                calm_truth::db::sqlite::workspace_execution::Scope::Track(track),
            )
            .await?
            .is_empty(),
        )
    }
}
