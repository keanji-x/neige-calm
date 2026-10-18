//! Kernel track creation/teardown and baseline selection policy.
use crate::operation::workspace_lease::upstream::{
    LeaseStart, choose_lease_start, diverged_refusal, head_upstream, set_branch_upstream,
};
use crate::{
    error::{CalmError, Result},
    model::Track,
};
use std::path::Path;
use workspace::worktree::track_worktree_target;
/// Make the track's worktree if it has one and it is not there yet. The upstream is refreshed
/// first (bounded, fail-soft), then the git work runs on a blocking thread: a registered
/// directory is done, an existing branch is checked out again, else a new branch starts by the
/// checkout's relation to its upstream (`choose_lease_start`) and keeps that upstream as its own
/// (#2112); git refuses whatever else is at the path. A diverged checkout is refused
/// (`attached-repo-diverged`) and a repository without a commit fails.
pub(crate) async fn ensure_track_worktree(track: &Track) -> Result<()> {
    ensure_track_worktree_admitted(track, || async { Ok(()) }).await
}

pub(crate) async fn ensure_track_worktree_admitted<F, A>(track: &Track, admit: F) -> Result<()>
where
    F: Fn() -> A + Send + Sync,
    A: std::future::Future<Output = Result<()>> + Send,
{
    let Some(worktree) = track.workspace.worktree.as_deref() else {
        return Ok(());
    };
    let target = track_worktree_target(track.id.as_str(), worktree)?;
    crate::operation::workspace_lease::upstream_fetch::refresh_upstream_admitted(
        &target.repo_root,
        &admit,
    )
    .await?;
    admit().await?;
    tokio::task::spawn_blocking(move || {
        workspace::worktree::ensure_track_worktree(&target, prepare_branch, observe)
    })
    .await
    .map_err(|error| CalmError::Internal(format!("track worktree task failed: {error}")))?
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

/// Discard the track worktree and its branch, a dirty worktree too.
/// Never fails the caller: the area delete sweeps every track with `?`, so an error here would
/// abort the other tracks' sweeps.
pub(crate) fn remove_track_worktree(track_id: &str, worktree: &str) {
    let removed = track_worktree_target(track_id, worktree)
        .map_err(CalmError::from)
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

fn prepare_branch(repo_root: &Path, branch: &str) -> Result<String> {
    let base = track_worktree_base(repo_root)?;
    let upstream = head_upstream(repo_root)?;
    set_branch_upstream(repo_root, branch, upstream.as_ref())?;
    Ok(base)
}
pub(super) fn observe(point: &str, repo_root: &Path) {
    #[cfg(feature = "fixtures")]
    crate::test_seams::blocking_pause_point(point, repo_root.to_str().expect("test repo path"));
    #[cfg(not(feature = "fixtures"))]
    let _ = (point, repo_root);
}
