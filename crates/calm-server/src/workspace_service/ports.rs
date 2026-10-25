//! Server adapters for workspace-runtime policy.
use crate::db::sqlite::{CheckoutOccupancy, checkout_occupancy};
use crate::error::{CalmError, Result};
use crate::operation::workspace_lease::upstream::{
    LeaseStart, choose_lease_start, diverged_refusal, head_upstream, set_branch_upstream,
};
use async_trait::async_trait;

use std::path::Path;
use workspace_runtime::track_worktree::TrackBranch;
use workspace_runtime::worktree_clean::{ClosedWorktree, ClosedWorktreeFacts};

#[derive(Clone, Copy)]
pub(crate) struct ServerWorkspacePorts;

#[async_trait]
impl TrackBranch for ServerWorkspacePorts {
    type Error = CalmError;

    async fn refresh_upstream<F, A>(&self, repo_root: &Path, admit: &F) -> Result<()>
    where
        F: Fn() -> A + Send + Sync,
        A: std::future::Future<Output = Result<()>> + Send,
    {
        crate::operation::workspace_lease::upstream_fetch::refresh_upstream_admitted(
            repo_root, admit,
        )
        .await?;
        Ok(())
    }

    fn prepare_branch(&self, repo_root: &Path, branch: &str) -> Result<String> {
        let base = track_worktree_base(repo_root)?;
        self.bind_branch_upstream(repo_root, branch)?;
        Ok(base)
    }

    fn bind_branch_upstream(&self, repo_root: &Path, branch: &str) -> Result<()> {
        let upstream = head_upstream(repo_root)?;
        set_branch_upstream(repo_root, branch, upstream.as_ref())
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
    {
        crate::test_seams::record_workspace_observation(point, repo_root);
        crate::test_seams::blocking_pause_point(point, repo_root.to_str().expect("test repo path"));
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = (point, repo_root);
}

#[derive(Clone)]
pub(crate) struct ServerClosedWorktreeFacts {
    pub(crate) receipts: super::reclaim::ServerReclaimPorts,
    pub(crate) runtime: std::sync::Arc<crate::operation::OperationRuntime>,
}

#[async_trait]
impl ClosedWorktreeFacts for ServerClosedWorktreeFacts {
    async fn kernel_clean_safe(
        &self,
        workspace: &calm_types::model::TrackWorkspace,
    ) -> Result<bool> {
        match super::reclaim::protect_delete(
            &mut *self.receipts.db.read_pool().acquire().await?,
            workspace,
        )
        .await
        {
            Ok(()) => Ok(true),
            Err(CalmError::Conflict(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }

    async fn extra_worktrees(
        &self,
        track: &str,
    ) -> Result<Vec<workspace_runtime::reclaim::ExtraWorktree>> {
        let mut tx = calm_truth::db::sqlite::begin_immediate_tx(&self.receipts.db).await?;
        let targets = super::reclaim::snapshot(&mut tx, track, &[]).await?;
        tx.commit().await?;
        Ok(targets)
    }

    async fn closed_idle_worktrees(&self) -> Result<Vec<ClosedWorktree>> {
        Ok(
            calm_truth::db::sqlite::closed_worktree_candidates(self.receipts.db.read_pool())
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

    async fn fence(&self, track: &str, closed_at: i64) -> Result<Option<Box<dyn Send>>> {
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("c1-clean-sampled", track).await;
        let drive = self.runtime.lock_for_track_delete().await;
        let guard = crate::per_card_lock::lock_key(self.runtime.track_delete_locks(), track).await;
        if !closed_idle_reclaim_safe(&self.receipts.db, track, Some(closed_at)).await? {
            return Ok(None);
        }
        drop(drive);
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("c1-clean-fenced", track).await;
        Ok(Some(Box::new(guard)))
    }
}

/// Sample only while the caller owns the track lifecycle fence. Recovery and the close tick
/// share the same eligibility policy; neither a parked row nor a closed flag is authority alone.
pub(super) async fn closed_idle_reclaim_safe(
    db: &calm_truth::db::sqlite::SqliteDb,
    track: &str,
    expected_close: Option<i64>,
) -> Result<bool> {
    let candidate =
        calm_truth::db::sqlite::closed_worktree_candidate(db.read_pool(), track).await?;
    let Some(candidate) = candidate else {
        return Ok(false);
    };
    let candidate = crate::model::Track::from(candidate);
    let targets =
        crate::operation::terminal_disposal::workspace_effect_targets(&candidate.workspace);
    let occupied = match calm_truth::db::sqlite::workspace_execution::location_occupancy(
        &mut *db.read_pool().acquire().await?,
        calm_truth::db::sqlite::workspace_execution::Scope::Track(track),
        &targets,
    )
    .await
    {
        Ok(ids) => !ids.is_empty(),
        Err(error) => {
            tracing::warn!(%error, track, "clean deferred: location occupancy unknown");
            true
        }
    };
    Ok(
        expected_close.is_none_or(|expected| candidate.closed_at == Some(expected))
            && !crate::operation::workspace_lease::track_has_active_forge_action(
                db.read_pool(),
                track,
            )
            .await?
            && checkout_occupancy(&mut *db.read_pool().acquire().await?, track, "").await?
                == CheckoutOccupancy::Free
            && !occupied,
    )
}

#[async_trait]
impl workspace_runtime::reclaim::ReclaimReceipts for ServerClosedWorktreeFacts {
    type Error = CalmError;
    async fn reclaim(
        &self,
        target: &workspace_runtime::reclaim::ExtraWorktree,
        observe: workspace_runtime::track_worktree::ObservePoint,
    ) -> Result<()> {
        self.receipts.reclaim(target, observe).await
    }
}
