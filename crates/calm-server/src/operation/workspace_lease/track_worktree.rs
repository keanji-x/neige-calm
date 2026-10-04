//! #1830 S1 — the track worktree: one kernel-made git worktree per attached track,
//! `<repo_root>/.claude/worktrees/track-<id>` on branch `neige/track-<id>`, where the track's
//! conversation agents run. The create route writes its path in the create transaction
//! (`TrackWorkspacePlan::AttachedWithTrackWorktree`) and [`ensure_track_worktree`] makes it after
//! the commit; track and area delete remove it ([`remove_track_worktree`]). Since #1830 S2 the
//! track's codex and claude workers run in it too (`super::worker`).

use std::path::{Path, PathBuf};

use crate::db::sqlite::track_worktree_path_for;
use crate::error::{CalmError, Result};
use crate::model::Track;
use crate::workspace_materialize::isolated_git_command;

use super::upstream::{
    LeaseStart, choose_lease_start, diverged_refusal, head_upstream, record_branch_upstream,
};
use super::{
    GitWorktreeRegistration, WorkspaceLeaseTarget, ensure_workspace_worktree_root_excluded,
    git_failed, git_ref_exists, git_worktree_registration, remove_workspace_worktree,
    validate_path_segment,
};

/// `neige/track-<track_id>`: a 32-hex track id is never `track-…`, so it cannot collide with a
/// pre-#1830-S2 per-card branch `neige/<track>/<card>` still in a repository.
pub(crate) fn track_branch_for(track_id: &str) -> Result<String> {
    validate_path_segment("track_id", track_id)?;
    Ok(format!("neige/track-{track_id}"))
}

/// The stored worktree path as a removal/ensure target. The repository root is the path's third
/// ancestor, and the path must be exactly [`track_worktree_path_for`] of it.
pub(crate) fn track_worktree_target(
    track_id: &str,
    worktree: &str,
) -> Result<WorkspaceLeaseTarget> {
    let path = PathBuf::from(worktree);
    let repo_root = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .filter(|root| root.is_absolute())
        .map(Path::to_path_buf);
    match repo_root {
        Some(repo_root) if track_worktree_path_for(&repo_root, track_id) == path => {
            Ok(WorkspaceLeaseTarget {
                branch: track_branch_for(track_id)?,
                repo_root,
                path,
            })
        }
        _ => Err(CalmError::Internal(format!(
            "track {track_id} worktree path {worktree} is not \
             `<repo_root>/.claude/worktrees/track-{track_id}`"
        ))),
    }
}

/// Make the track's worktree if it has one and it is not there yet. The upstream is refreshed
/// first (bounded, fail-soft), then the git work runs on a blocking thread: a registered
/// directory is done, an existing branch is checked out again, else a new branch starts by the
/// checkout's relation to its upstream (`choose_lease_start`) and keeps that upstream as its own
/// (#2112); git refuses whatever else is at the path. A diverged checkout is refused
/// (`attached-repo-diverged`) and a repository without a commit fails.
pub(crate) async fn ensure_track_worktree(track: &Track) -> Result<()> {
    let Some(worktree) = track.workspace.worktree.as_deref() else {
        return Ok(());
    };
    let target = track_worktree_target(track.id.as_str(), worktree)?;
    super::upstream_fetch::refresh_upstream(&target.repo_root).await;
    tokio::task::spawn_blocking(move || ensure_track_worktree_blocking(&target))
        .await
        .map_err(|error| CalmError::Internal(format!("track worktree task failed: {error}")))?
}

fn ensure_track_worktree_blocking(target: &WorkspaceLeaseTarget) -> Result<()> {
    ensure_workspace_worktree_root_excluded(&target.repo_root)?;
    // Registered and on disk: done. Anything else (a hand-removed directory, another
    // registration at the path) is left to `git worktree add` to refuse.
    if git_worktree_registration(target)? == GitWorktreeRegistration::Present
        && target.path.is_dir()
    {
        return Ok(());
    }
    // Isolated: the checkout runs the repository's own code (hooks, filters, fsmonitor).
    let mut command = isolated_git_command();
    command
        .arg("-C")
        .arg(&target.repo_root)
        .args(["worktree", "add"]);
    if git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch))? {
        command.arg(&target.path).arg(&target.branch);
    } else {
        // #2112: the new branch keeps the checkout's upstream as of now; publish and catch-up
        // read it from the branch, never from whatever the checkout is on later. Written first:
        // config for a branch `worktree add` then fails to make is rewritten by the retry.
        if let Some(upstream) = head_upstream(&target.repo_root)? {
            record_branch_upstream(&target.repo_root, &target.branch, &upstream)?;
        }
        let base = track_worktree_base(&target.repo_root)?;
        command
            .args(["-b", &target.branch])
            .arg(&target.path)
            .arg(base);
    }
    let output = command.output().map_err(|error| {
        CalmError::Internal(format!(
            "spawn git worktree add for {}: {error}",
            target.path.display()
        ))
    })?;
    if !output.status.success() {
        return Err(git_failed("git worktree add", &target.repo_root, &output));
    }
    Ok(())
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
        .and_then(|target| remove_workspace_worktree(&target));
    if let Err(error) = removed {
        tracing::warn!(
            track_id,
            worktree,
            error = %error,
            "track teardown could not remove the track worktree"
        );
    }
}
