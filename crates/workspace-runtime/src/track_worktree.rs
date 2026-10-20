//! Kernel track creation/teardown and baseline selection policy.
use crate::error::WorkspaceError;
use async_trait::async_trait;
use calm_types::model::{Track, TrackWorkspace, TrackWorkspaceKind};
use std::path::Path;
use workspace::worktree::track_worktree_target;

pub type ObservePoint = fn(&str, &Path);

#[async_trait]
pub trait TrackBranch: Clone + Send + Sync + 'static {
    type Error: From<WorkspaceError> + From<workspace::error::Error> + Send + 'static;

    async fn refresh_upstream<F, A>(&self, repo_root: &Path, admit: &F) -> Result<(), Self::Error>
    where
        F: Fn() -> A + Send + Sync,
        A: std::future::Future<Output = Result<(), Self::Error>> + Send;
    fn prepare_branch(&self, repo_root: &Path, branch: &str) -> Result<String, Self::Error>;
}

/// Make the track's worktree if it has one and it is not there yet. The upstream is refreshed
/// first (bounded, fail-soft), then the git work runs on a blocking thread: a registered
/// directory is done, an existing branch is checked out again, else a new branch starts by the
/// checkout's relation to its upstream (`choose_lease_start`) and keeps that upstream as its own
/// (#2112); git refuses whatever else is at the path. A diverged checkout is refused
/// (`attached-repo-diverged`) and a repository without a commit fails.
pub async fn ensure_track_worktree<P: TrackBranch>(
    track: &Track,
    ports: P,
    observe: ObservePoint,
) -> Result<(), P::Error> {
    ensure_track_worktree_admitted(track, ports, observe, || async { Ok(()) }).await
}

/// Recheck caller admission during upstream refresh and before local git mutation.
pub async fn ensure_track_worktree_admitted<P, F, A>(
    track: &Track,
    ports: P,
    observe: ObservePoint,
    admit: F,
) -> Result<(), P::Error>
where
    P: TrackBranch,
    F: Fn() -> A + Send + Sync,
    A: std::future::Future<Output = Result<(), P::Error>> + Send,
{
    let Some(worktree) = track.workspace.worktree.as_deref() else {
        return Ok(());
    };
    let target = track_worktree_target(track.id.as_str(), worktree)?;
    ports.refresh_upstream(&target.repo_root, &admit).await?;
    admit().await?;
    tokio::task::spawn_blocking(move || {
        workspace::worktree::ensure_track_worktree(
            &target,
            |repo_root, branch| ports.prepare_branch(repo_root, branch),
            observe,
        )
    })
    .await
    .map_err(|error| WorkspaceError::Internal(format!("track worktree task failed: {error}")))?
}

/// Discard the track worktree and its branch, a dirty worktree too.
/// Never fails the caller: the area delete sweeps every track with `?`, so an error here would
/// abort the other tracks' sweeps.
pub fn remove_track_worktree(track_id: &str, workspace: &TrackWorkspace, observe: ObservePoint) {
    let worktree = match (workspace.kind, workspace.worktree.as_deref()) {
        (TrackWorkspaceKind::Managed, _) => return,
        // Guarantee G: never mutate a borrowed source directory.
        (TrackWorkspaceKind::Attached, None) => return,
        (TrackWorkspaceKind::Attached, Some(worktree)) => worktree,
    };
    let removed = track_worktree_target(track_id, worktree)
        .map_err(WorkspaceError::from)
        .and_then(|target| {
            workspace::teardown::remove_workspace_worktree(&target, observe).map_err(Into::into)
        });
    if let Err(error) = removed {
        tracing::warn!(
            track_id,
            worktree,
            error = %error,
            "track teardown could not remove the track worktree"
        );
    }
}
