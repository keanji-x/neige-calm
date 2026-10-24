//! Conservative removal of an event-owned extra Git worktree. No force, prune, branch deletion,
//! or directory fallback. Lifecycle fences and event ownership are supplied by the caller.
use crate::{
    base,
    bounded_git::{BoundedOutput, GIT_BOUND},
    command::isolated_git_command,
    error::{Error, Result},
    git::{GitWorktreeRegistration, git_failed},
    metadata_lock,
};
use std::{
    path::{Path, PathBuf},
    process::Output,
    time::Instant,
};

#[derive(Clone, Debug)]
pub struct ReclaimRequest {
    pub repo: PathBuf,
    pub path: PathBuf,
    /// Live/default checkouts and all other outstanding claims. Ancestors are protected too.
    pub protected: Vec<PathBuf>,
    /// Ref names or prefixes scheduled for deletion by the same sweep.
    pub excluded_refs: Vec<String>,
    pub ownership_conflict: bool,
    /// Current cwd cannot be proven for an unsettled terminal, even in another track.
    pub unknown_terminal: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReclaimOutcome {
    Removed,
    Retained(&'static str),
}

/// Every probe and the final Git operation use a bounded, credential-isolated command.
/// The metadata lock coordinates Neige; external Git and filesystem writers remain outside it.
pub fn reclaim(request: &ReclaimRequest, observe: impl Fn(&str, &Path)) -> Result<ReclaimOutcome> {
    use ReclaimOutcome::Retained;
    let path = &request.path;
    if !path.is_absolute() {
        return Ok(Retained("relative path"));
    }
    if request.ownership_conflict {
        return Ok(Retained("ownership conflict"));
    }
    if request.unknown_terminal {
        return Ok(Retained("unsettled terminal"));
    }
    if request.protected.iter().any(|p| {
        !p.is_absolute()
            || p.starts_with(path)
            || std::fs::canonicalize(p)
                .ok()
                .is_some_and(|real| real.starts_with(path))
    }) {
        return Ok(Retained("protected checkout or descendant"));
    }
    // Resolving aliases is useful for identifying them, never permission to remove them.
    if std::fs::canonicalize(path).ok().as_ref() != Some(path)
        || base::is_symlink_leaf(&path.join(".git"))?
        || !path.join(".git").is_file()
    {
        return Ok(Retained("non-canonical or missing linked checkout"));
    }
    let deadline = Instant::now() + GIT_BOUND;
    let _lock = metadata_lock::GitMetadataLock::acquire(&request.repo, &observe)?;
    let listed = git(
        &request.repo,
        &["worktree", "list", "--porcelain", "-z"],
        deadline,
    )?;
    if base::worktree_registration_in(&listed.stdout, &request.repo, path)?
        != GitWorktreeRegistration::Present
        || base::lease_git_common_dir(path)? != base::lease_git_common_dir(&request.repo)?
    {
        return Ok(Retained("foreign registration"));
    }
    let top = git(path, &["rev-parse", "--show-toplevel"], deadline)?;
    if String::from_utf8_lossy(&top.stdout).trim_end() != path.to_string_lossy() {
        return Ok(Retained("checkout identity mismatch"));
    }
    let status = git(
        path,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
        ],
        deadline,
    )?;
    if !status.stdout.is_empty() {
        return Ok(Retained("dirty"));
    }
    let head = git(path, &["rev-parse", "--verify", "HEAD^{commit}"], deadline)?;
    let head = String::from_utf8(head.stdout).map_err(|e| Error::Internal(e.to_string()))?;
    let refs = git(
        path,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "--contains",
            head.trim(),
        ],
        deadline,
    )?;
    let refs = String::from_utf8(refs.stdout).map_err(|e| Error::Internal(e.to_string()))?;
    if !refs.lines().any(|name| {
        !request.excluded_refs.iter().any(|excluded| {
            if excluded.ends_with('/') {
                name.starts_with(excluded)
            } else {
                name == excluded
            }
        })
    }) {
        return Ok(Retained("no surviving ref"));
    }
    // This observation is deliberately immediately before the attempt, after every preflight.
    observe("extra-worktree-remove-attempt", path);
    git(
        &request.repo,
        &[
            "worktree",
            "remove",
            "--",
            base::utf8_path(path, "extra worktree")?,
        ],
        deadline,
    )?;
    Ok(ReclaimOutcome::Removed)
}

fn git(repo: &Path, args: &[&str], deadline: Instant) -> Result<Output> {
    let result = isolated_git_command()
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-C",
        ])
        .arg(repo)
        .args(args)
        .bounded_output(deadline)
        .map_err(|e| Error::Internal(e.to_string()))?;
    if !result.status.success() {
        return Err(git_failed("extra worktree reclaim", repo, &result));
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
