//! Synchronous Track Git teardown, run by the owned postcommit sweep's blocking bridge.
use super::{
    GitWorktreeRegistration, WorkspaceLeaseTarget, base, git_failed, git_ref_exists,
    git_repo_available, git_worktree_prune, git_worktree_registered, git_worktree_registration,
    metadata_lock, remove_workspace_dir_if_exists,
};
use crate::error::{CalmError, Result};
use crate::workspace_materialize::{isolated_git_command, neige_git_command};

/// Discard a kernel-made worktree and its branch, whatever the checkout holds: a symlink leaf is
/// unlinked (never followed) and what git registered there pruned; someone else's registration
/// at the path's realpath is refused; else `worktree remove --force`, `branch -D`, and a plain
/// directory removal. A repository that is gone leaves only the directory to remove. `true` when
/// anything was removed. The track worktree teardown (#1830 S1) is the one caller.
pub(crate) fn remove_workspace_worktree(target: &WorkspaceLeaseTarget) -> Result<bool> {
    if !git_repo_available(&target.repo_root) {
        return remove_workspace_dir_if_exists(&target.path_string());
    }
    #[cfg(feature = "fixtures")]
    super::metadata_test_pause("track-metadata-before", &target.repo_root);
    let _metadata_lock = metadata_lock::GitMetadataLock::acquire(&target.repo_root)?;
    #[cfg(feature = "fixtures")]
    super::metadata_test_pause("track-metadata-entered", &target.repo_root);

    // A symlink leaf is never a registration of ours: unlink it and prune
    // what git registered at the now-missing path (a worktree moved away and
    // linked back would otherwise keep its branch checked out and fail the
    // `branch -D` below). `worktree remove --force` through the link would
    // delete the link's target — an external directory, the main checkout.
    let link_removed = base::unlink_symlink_leaf(&target.path)?;
    if link_removed {
        git_worktree_prune(&target.repo_root)?;
    }
    let registration = if link_removed {
        GitWorktreeRegistration::Absent
    } else {
        git_worktree_registration(target)?
    };
    // Someone else's worktree at our realpath: `worktree remove --force`
    // would delete it through the alias.
    if let GitWorktreeRegistration::Foreign {
        registered_as,
        branch,
    } = registration
    {
        return Err(base::foreign_registration_refusal(
            target,
            &registered_as,
            branch.as_deref(),
        ));
    }
    let registered = registration != GitWorktreeRegistration::Absent;
    let path_existed = !link_removed && target.path.exists();
    if registered || path_existed {
        let output = neige_git_command()
            .arg("-C")
            .arg(&target.repo_root)
            .args(["worktree", "remove", "--force"])
            .arg(&target.path)
            .output()
            .map_err(|e| {
                CalmError::Internal(format!(
                    "spawn git worktree remove for {}: {e}",
                    target.path.display()
                ))
            })?;
        if !output.status.success() && registered && git_worktree_registered(target)? {
            return Err(git_failed(
                "git worktree remove --force",
                &target.repo_root,
                &output,
            ));
        }
    }

    let branch_ref = format!("refs/heads/{}", target.branch);
    let branch_existed = git_ref_exists(&target.repo_root, &branch_ref)?;
    if branch_existed {
        // Isolated: a ref deletion runs the repository's `reference-transaction` hook.
        let output = isolated_git_command()
            .arg("-C")
            .arg(&target.repo_root)
            .args(["branch", "-D", &target.branch])
            .output()
            .map_err(|e| {
                CalmError::Internal(format!(
                    "spawn git branch -D {} in {}: {e}",
                    target.branch,
                    target.repo_root.display()
                ))
            })?;
        if !output.status.success() && git_ref_exists(&target.repo_root, &branch_ref)? {
            return Err(git_failed("git branch -D", &target.repo_root, &output));
        }
    }

    let dir_removed = remove_workspace_dir_if_exists(&target.path_string())?;
    Ok(link_removed || registered || path_existed || branch_existed || dir_removed)
}
