//! Server adapters for workspace-runtime policy.
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
    pub(crate) db: calm_truth::db::sqlite::SqliteDb,
    pub(crate) runtime: std::sync::Arc<crate::operation::OperationRuntime>,
}

#[async_trait]
impl ClosedWorktreeFacts for ServerClosedWorktreeFacts {
    type Error = CalmError;

    async fn kernel_clean_safe(
        &self,
        workspace: &calm_types::model::TrackWorkspace,
    ) -> Result<bool> {
        let _ = workspace;
        Ok(false)
    }

    async fn retire_extra_worktrees(&self, track: &str, closed_at: i64) -> Result<()> {
        self.db
            .writer()
            .with_writer(|session| {
                Box::pin(async move {
                    let mut tx = session.begin_immediate().await?;
                    let current: Option<Option<i64>> =
                        sqlx::query_scalar("SELECT closed_at FROM tracks WHERE id=?")
                            .bind(track)
                            .fetch_optional(&mut **tx)
                            .await?;
                    if current == Some(Some(closed_at)) {
                        let _ =
                            calm_truth::db::sqlite::workspace_retirement::retire_track_extras_tx(
                                &mut tx, track,
                            )
                            .await?;
                    }
                    tx.commit().await?;
                    Ok::<_, CalmError>(())
                })
            })
            .await
            .map_err(calm_truth::TruthError::from)?
    }

    async fn closed_idle_worktrees(&self) -> Result<Vec<ClosedWorktree>> {
        let evidence = self
            .runtime
            .reclaim_context()
            .planner_clean_evidence()
            .await?;
        Ok(
            calm_truth::db::sqlite::closed_worktree_candidates(self.db.read_pool(), evidence)
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
        let targets = &self.runtime.location_admission()?.teardown_targets;
        if targets
            .snapshot_with_owners()
            .iter()
            .any(|(owner, _)| owner == track)
        {
            return Ok(None);
        }
        let sampled: Option<(Option<i64>, Option<String>)> =
            sqlx::query_as("SELECT closed_at,workspace_worktree_path FROM tracks WHERE id=?")
                .bind(track)
                .fetch_optional(self.db.read_pool())
                .await?;
        let Some((Some(at), path)) = sampled else {
            return Ok(None);
        };
        if at != closed_at {
            return Ok(None);
        }
        let registration = targets.registration();
        if let Some(path) = path {
            registration.register_location(track, path.into());
        }
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("c5d-clean-before-track", track).await;
        let Some(guard) =
            crate::per_card_lock::try_lock_key(self.runtime.track_delete_locks(), track)
        else {
            return Ok(None);
        };
        let current: Option<Option<i64>> =
            sqlx::query_scalar("SELECT closed_at FROM tracks WHERE id=?")
                .bind(track)
                .fetch_optional(self.db.read_pool())
                .await?;
        if current != Some(Some(closed_at)) {
            return Ok(None);
        }
        #[cfg(feature = "fixtures")]
        crate::test_seams::pause_point("c1-clean-fenced", track).await;
        Ok(Some(Box::new((registration, guard))))
    }
}
