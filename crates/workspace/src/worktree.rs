//! Worktree creation and ignored-file cleanup mechanisms.
use crate::bounded_git::{BoundedOutput, GIT_BOUND};
use crate::{
    base,
    command::isolated_git_command,
    error::{Error, Result},
    git::{self, *},
    metadata_lock,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
/// `neige/track-<track_id>`: a 32-hex track id is never `track-…`, so it cannot collide with a
/// pre-#1830-S2 per-card branch `neige/<track>/<card>` still in a repository.
pub fn track_branch_for(track_id: &str) -> Result<String> {
    validate_path_segment("track_id", track_id)?;
    Ok(format!("neige/track-{track_id}"))
}

/// The stored worktree path as a removal/ensure target. The repository root is the path's third
/// ancestor, and the path must be exactly [`track_worktree_path_for`] of it.
pub fn track_worktree_target(track_id: &str, worktree: &str) -> Result<WorkspaceLeaseTarget> {
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
        _ => Err(Error::Internal(format!(
            "track {track_id} worktree path {worktree} is not \
             `<repo_root>/.claude/worktrees/track-{track_id}`"
        ))),
    }
}

/// Ensure under the metadata lock. `prepare_branch` runs only for a new branch, after the
/// registration/ref probes, so the kernel selects the base and records upstream at that instant.
/// `observe` preserves the kernel's existing fixture observations of lock ordering.
pub fn ensure_track_worktree<E: From<crate::error::Error>>(
    target: &WorkspaceLeaseTarget,
    prepare_branch: impl FnOnce(&Path, &str) -> std::result::Result<String, E>,
    observe: impl Fn(&str, &Path),
) -> std::result::Result<(), E> {
    observe("track-metadata-before", &target.repo_root);
    let _metadata_lock =
        crate::metadata_lock::GitMetadataLock::acquire(&target.repo_root, &observe)?;
    observe("track-metadata-entered", &target.repo_root);
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
        let base = prepare_branch(&target.repo_root, &target.branch)?;
        command
            .args(["-b", &target.branch])
            .arg(&target.path)
            .arg(base);
    }
    let output = command.output().map_err(|error| {
        Error::Internal(format!(
            "spawn git worktree add for {}: {error}",
            target.path.display()
        ))
    })?;
    if !output.status.success() {
        return Err(git_failed("git worktree add", &target.repo_root, &output).into());
    }
    Ok(())
}

/// Remove the ignored files under the worktree; tracked and untracked-unignored files stay. A
/// single `-f`: git keeps an ignored nested repository (someone's worktree inside this one).
/// `Ok(false)` when there is nothing of ours to clean. Under the metadata lock, so a track delete's
/// `worktree remove` is not half done; only a registered, non-symlink worktree is cleaned, and
/// `GIT_CEILING_DIRECTORIES` stops git from finding the main checkout when the worktree's `.git`
/// is missing. A track reopened after the read loses its build output, which is regenerable.
pub fn clean_ignored(
    track_id: &str,
    worktree: &str,
    observe: impl Fn(&str, &Path),
) -> Result<bool> {
    let target = track_worktree_target(track_id, worktree)?;
    if !git_repo_available(&target.repo_root) {
        return Ok(false);
    }
    let _metadata_lock = metadata_lock::GitMetadataLock::acquire(&target.repo_root, observe)?;
    if base::is_symlink_leaf(&target.path)? || !target.path.is_dir() {
        return Ok(false);
    }
    if git_worktree_registration(&target)? != GitWorktreeRegistration::Present {
        tracing::warn!(
            track_id,
            worktree,
            "track worktree clean skipped: the path is not this track's registered worktree"
        );
        return Ok(false);
    }
    let ceiling = target.path.parent().unwrap_or(&target.path);
    let output = isolated_git_command()
        .env("GIT_CEILING_DIRECTORIES", ceiling)
        .args(["-c", "core.fsmonitor=false", "-C"])
        .arg(&target.path)
        .args(["clean", "-fdXq"])
        .output()
        .map_err(|error| {
            Error::Internal(format!(
                "spawn git clean for {}: {error}",
                target.path.display()
            ))
        })?;
    if !output.status.success() {
        return Err(git::git_failed("git clean", &target.path, &output));
    }
    Ok(true)
}

/// `<repo_root>/.claude/worktrees/track-<track_id>`: one segment below `worktrees/`, so it never
/// parses as a lease path (`<track>/<card>`) and no lease sweep enumerates it.
pub fn track_worktree_path_for(repo_root: &Path, track_id: &str) -> PathBuf {
    repo_root
        .join(".claude")
        .join("worktrees")
        .join(format!("track-{track_id}"))
}

/// Create an independent detached checkout at a commit that exists in this repository.
/// The caller owns the destination's lifecycle; no track branch is moved.
pub fn detached_worktree(repo: &Path, path: &Path, revision: &str) -> Result<()> {
    detached_worktree_with_timeout(repo, path, revision, GIT_BOUND)
}

/// One deadline for Git checkout commands. Metadata lock acquisition has its own
/// 30s bound; repository probes also use bounded process groups.
pub fn detached_worktree_with_timeout(
    repo: &Path,
    path: &Path,
    revision: &str,
    bound: Duration,
) -> Result<()> {
    let deadline = Instant::now() + bound;
    if revision.len() != 40
        || !revision
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Internal(
            "detached worktree revision must be a full commit id".into(),
        ));
    }
    let _lock = metadata_lock::GitMetadataLock::acquire(repo, |_, _| {})?;
    let exists = checkout_git_command()
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "-e", &format!("{revision}^{{commit}}")])
        .bounded_output(deadline)
        .map_err(|e| Error::Internal(e.to_string()))?;
    if !exists.status.success() {
        return Err(git::git_failed("git cat-file", repo, &exists));
    }
    if path.exists() {
        if crate::base::is_symlink_leaf(path)?
            || !registered_worktree(repo, path, deadline)?
            || crate::base::lease_git_common_dir(path)? != crate::base::lease_git_common_dir(repo)?
        {
            return Err(Error::Internal(
                "detached checkout identity mismatch".into(),
            ));
        }
        let head = checkout_git_command()
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "HEAD"])
            .bounded_output(deadline)
            .map_err(|e| Error::Internal(e.to_string()))?;
        let branch = checkout_git_command()
            .arg("-C")
            .arg(path)
            .args(["symbolic-ref", "-q", "HEAD"])
            .bounded_output(deadline)
            .map_err(|e| Error::Internal(e.to_string()))?;
        if !head.status.success()
            || branch.status.code() != Some(1)
            || String::from_utf8_lossy(&head.stdout).trim() != revision
        {
            return Err(Error::Internal(
                "existing checkout is not detached at the requested revision".into(),
            ));
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Internal(e.to_string()))?;
    }
    let output = checkout_git_command()
        .arg("-C")
        .arg(repo)
        .args(["worktree", "add", "--detach"])
        .arg(path)
        .arg(revision)
        .bounded_output(deadline)
        .map_err(|e| Error::Internal(e.to_string()))?;
    if !output.status.success() {
        return Err(git::git_failed("git worktree add --detach", repo, &output));
    }
    Ok(())
}

/// Remove a caller-owned disposable checkout, including changes made by its command.
/// Git owns registration removal; this never prunes unrelated worktrees.
pub fn remove_worktree(repo: &Path, path: &Path) -> Result<()> {
    let deadline = Instant::now() + GIT_BOUND;
    let _lock = metadata_lock::GitMetadataLock::acquire(repo, |_, _| {})?;
    if !registered_worktree(repo, path, deadline)? {
        if crate::base::is_symlink_leaf(path)? {
            return Err(Error::Internal("refusing a symlink checkout".into()));
        }
        if path.exists() {
            std::fs::remove_dir_all(path).map_err(|e| Error::Internal(e.to_string()))?;
        }
        return Ok(());
    }
    let output = checkout_git_command()
        .arg("-C")
        .arg(repo)
        .args(["worktree", "remove", "--force", "--force"])
        .arg(path)
        .bounded_output(deadline)
        .map_err(|e| Error::Internal(e.to_string()))?;
    if !output.status.success() {
        return Err(git::git_failed("git worktree remove", repo, &output));
    }
    Ok(())
}

fn registered_worktree(repo: &Path, path: &Path, deadline: Instant) -> Result<bool> {
    let listed = checkout_git_command()
        .arg("-C")
        .arg(repo)
        .args(["worktree", "list", "--porcelain", "-z"])
        .bounded_output(deadline)
        .map_err(|e| Error::Internal(e.to_string()))?;
    if !listed.status.success() {
        return Err(git::git_failed("git worktree list", repo, &listed));
    }
    match base::worktree_registration_in(&listed.stdout, repo, path)? {
        GitWorktreeRegistration::Present | GitWorktreeRegistration::Prunable => Ok(true),
        GitWorktreeRegistration::Absent => Ok(false),
        GitWorktreeRegistration::Foreign { .. } => Err(Error::Internal(
            "refusing another worktree's registration alias".into(),
        )),
    }
}

fn checkout_git_command() -> std::process::Command {
    let mut cmd = isolated_git_command();
    cmd.args([
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.fsmonitor=false",
    ]);
    cmd
}
