//! Git worktree registration and path mechanisms.
use crate::{
    base,
    command::neige_git_command,
    error::{Error, Result},
};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::Output,
};
/// A kernel-made worktree as a git target: the track worktree (#1830 S1) that
/// [`crate::teardown::remove_workspace_worktree`] and [`crate::worktree::ensure_track_worktree`] act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceLeaseTarget {
    pub repo_root: PathBuf,
    pub path: PathBuf,
    pub branch: String,
}

impl WorkspaceLeaseTarget {
    pub(crate) fn path_string(&self) -> String {
        self.path.to_string_lossy().to_string()
    }
}

pub(crate) fn remove_workspace_dir_if_exists(path: &str) -> Result<bool> {
    let path = Path::new(path);
    // A symlink leaf is unlinked, never followed.
    if base::unlink_symlink_leaf(path)? {
        return Ok(true);
    }
    let existed = path.exists();
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(existed),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::Internal(format!(
            "remove workspace lease directory {}: {e}",
            path.display()
        ))),
    }
}

const WORKTREE_EXCLUDE: &str = ".claude/worktrees/";

pub(crate) fn ensure_workspace_worktree_root_excluded(repo_root: &Path) -> Result<()> {
    ensure_git_exclude_entry(repo_root, WORKTREE_EXCLUDE)
}

/// Append `entry` to `<git-dir>/info/exclude` unless a line already equals it.
/// The match is exact on purpose: a near-miss (`.neige` vs `.neige/`) would still hide the directory but append a second line on every call.
pub fn ensure_git_exclude_entry(repo_root: &Path, entry: &str) -> Result<()> {
    let exclude_path = git_exclude_path(repo_root)?;
    ensure_exclude_entry_at(&exclude_path, entry)
}

/// Append at Git's already reported location. No Git command is executed here.
pub(crate) fn ensure_exclude_entry_at(exclude_path: &Path, entry: &str) -> Result<()> {
    let existing = read_exclude(exclude_path)?;
    if has_exclude_entry(&existing, entry) {
        return Ok(());
    }
    if let Some(parent) = exclude_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            Error::Internal(format!(
                "create git exclude directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(exclude_path)
        .map_err(|error| {
            Error::Internal(format!(
                "open git exclude {}: {error}",
                exclude_path.display()
            ))
        })?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        file.write_all(b"\n").map_err(|error| {
            Error::Internal(format!(
                "write git exclude {}: {error}",
                exclude_path.display()
            ))
        })?;
    }
    file.write_all(format!("{entry}\n").as_bytes())
        .map_err(|error| {
            Error::Internal(format!(
                "write git exclude {}: {error}",
                exclude_path.display()
            ))
        })?;
    Ok(())
}

fn read_exclude(exclude_path: &Path) -> Result<String> {
    match std::fs::read_to_string(exclude_path) {
        Ok(existing) => Ok(existing),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(Error::Internal(format!(
            "read git exclude {}: {error}",
            exclude_path.display()
        ))),
    }
}
fn has_exclude_entry(existing: &str, entry: &str) -> bool {
    existing.lines().any(|line| line.trim() == entry)
}
pub(crate) fn exclude_entry_present(exclude_path: &Path, entry: &str) -> Result<bool> {
    Ok(has_exclude_entry(&read_exclude(exclude_path)?, entry))
}

pub(crate) fn git_exclude_path(repo_root: &Path) -> Result<PathBuf> {
    // Env-isolated: an inherited `GIT_DIR` would send the exclude file into a different repository.
    let output = neige_git_command()
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--git-path", "info/exclude"])
        .output()
        .map_err(|e| {
            Error::Internal(format!(
                "spawn git rev-parse --git-path info/exclude in {}: {e}",
                repo_root.display()
            ))
        })?;
    if !output.status.success() {
        return Err(git_failed(
            "git rev-parse --git-path info/exclude",
            repo_root,
            &output,
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let exclude_path = stdout.trim_end_matches(&['\r', '\n'][..]);
    if exclude_path.is_empty() {
        return Err(Error::Internal(format!(
            "git rev-parse --git-path info/exclude in {} returned an empty path",
            repo_root.display()
        )));
    }
    let exclude_path = PathBuf::from(exclude_path);
    if exclude_path.is_absolute() {
        Ok(exclude_path)
    } else {
        Ok(repo_root.join(exclude_path))
    }
}

pub fn validate_path_segment(label: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
    {
        return Err(Error::Internal(format!(
            "invalid workspace lease {label} path segment {value:?}"
        )));
    }
    Ok(())
}

pub(crate) fn git_repo_available(repo_root: &Path) -> bool {
    neige_git_command()
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--git-dir"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub(crate) fn git_ref_exists(repo_root: &Path, full_ref: &str) -> Result<bool> {
    let status = neige_git_command()
        .arg("-C")
        .arg(repo_root)
        .args(["show-ref", "--verify", "--quiet", full_ref])
        .status()
        .map_err(|e| {
            Error::Internal(format!(
                "spawn git show-ref {full_ref} in {}: {e}",
                repo_root.display()
            ))
        })?;
    Ok(status.success())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GitWorktreeRegistration {
    Absent,
    Present,
    Prunable,
    /// A registration at the lease path's realpath recorded under another
    /// path (`registered_as`; `branch` is its `refs/heads/..`, `None` when
    /// detached): an alias of someone else's worktree, never reused, pruned
    /// or removed.
    Foreign {
        registered_as: PathBuf,
        branch: Option<String>,
    },
}

pub(crate) fn git_worktree_registered(target: &WorkspaceLeaseTarget) -> Result<bool> {
    Ok(git_worktree_registration(target)? != GitWorktreeRegistration::Absent)
}

pub(crate) fn git_worktree_registration(
    target: &WorkspaceLeaseTarget,
) -> Result<GitWorktreeRegistration> {
    let output = neige_git_command()
        .arg("-C")
        .arg(&target.repo_root)
        .args(["worktree", "list", "--porcelain", "-z"])
        .output()
        .map_err(|e| {
            Error::Internal(format!(
                "spawn git worktree list in {}: {e}",
                target.repo_root.display()
            ))
        })?;
    if !output.status.success() {
        return Err(git_failed("git worktree list", &target.repo_root, &output));
    }
    base::worktree_registration_in(&output.stdout, &target.repo_root, &target.path)
}

/// Drop registrations whose directory is gone; touches no files.
pub(crate) fn git_worktree_prune(repo_root: &Path) -> Result<()> {
    let output = neige_git_command()
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "prune", "--expire", "now"])
        .output()
        .map_err(|e| {
            Error::Internal(format!(
                "spawn git worktree prune in {}: {e}",
                repo_root.display()
            ))
        })?;
    if !output.status.success() {
        return Err(git_failed("git worktree prune", repo_root, &output));
    }
    Ok(())
}

pub fn git_failed(action: &str, repo_root: &Path, output: &Output) -> Error {
    Error::Internal(format!(
        "{action} failed in {}: {}",
        repo_root.display(),
        output_summary(output)
    ))
}

pub fn output_summary(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return stdout;
    }
    format!("exit status {}", output.status)
}
