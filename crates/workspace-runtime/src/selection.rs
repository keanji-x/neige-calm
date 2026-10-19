//! Workspace selection and attached admission policy.
use crate::error::{Result, WorkspaceError};
use calm_types::model::{TrackWorkspace, TrackWorkspaceKind};
use std::path::{Path, PathBuf};
use workspace::command::neige_git_command;

/// Materialize `workspace` if — and only if — it is `Managed`; attached directories are the user's and are
/// never created or written to. Attached validation lives at the user-facing entry points instead.
pub fn materialize_workspace(
    workspace: &TrackWorkspace,
    workspace_root: &Path,
    track_id: &str,
) -> Result<()> {
    match workspace.kind {
        TrackWorkspaceKind::Managed => {
            materialize_managed_workspace(workspace_root, Path::new(&workspace.path), track_id)
        }
        TrackWorkspaceKind::Attached => Ok(()),
    }
}

/// Attached admission: absolute, exists, inside a Git work tree (a subdirectory is fine — the worker derives
/// the root itself), and the slice-branch namespace is free. Every failure is `BadRequest`.
pub fn validate_attached_workspace(path: &Path) -> Result<()> {
    validate_borrowed_workspace(path)?;
    ensure_slice_branch_namespace_is_free(path)
}

/// None needs only an existing Git work tree, without any owned branch/root requirements.
pub fn validate_borrowed_workspace(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(WorkspaceError::BadRequest(format!(
            "attached workspace: path must be absolute (start with `/`); got `{}`",
            path.display()
        )));
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(WorkspaceError::BadRequest(format!(
                "attached workspace: `{}` is not a directory",
                path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(WorkspaceError::BadRequest(format!(
                "attached workspace: `{}` does not exist. Neige never creates an \
                 attached directory — it is yours, so it has to be there already.",
                path.display()
            )));
        }
        Err(error) => {
            return Err(WorkspaceError::BadRequest(format!(
                "attached workspace: cannot read `{}`: {error}",
                path.display()
            )));
        }
    }
    let output = neige_git_command()
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|error| {
            WorkspaceError::BadRequest(format!(
                "attached workspace: cannot run git in `{}`: {error}",
                path.display()
            ))
        })?;
    if !output.status.success() {
        return Err(WorkspaceError::BadRequest(format!(
            "attached workspace: `{}` is not inside a Git work tree, so no worker \
             could ever get a workspace lease there. git said: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

/// The ref every track branch lives under; a test ties it to `track_branch_for`.
const SLICE_BRANCH_NAMESPACE_REF: &str = "refs/heads/neige";

/// Refuse a repository that already holds `refs/heads/neige`: git cannot hold that file and
/// `refs/heads/neige/track-<id>` at once, so the track worktree could never be made.
/// Only the exact ref is checked; a `show-ref` exit other than 0/1 is refused (fail closed).
fn ensure_slice_branch_namespace_is_free(path: &Path) -> Result<()> {
    let output = neige_git_command()
        .arg("-C")
        .arg(path)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            SLICE_BRANCH_NAMESPACE_REF,
        ])
        .output()
        .map_err(|error| {
            WorkspaceError::BadRequest(format!(
                "attached workspace: cannot run git in `{}`: {error}",
                path.display()
            ))
        })?;
    match output.status.code() {
        Some(1) => Ok(()),
        Some(0) => Err(WorkspaceError::BadRequest(format!(
            "attached workspace: `{}` has a branch named `neige`, and \
             `{SLICE_BRANCH_NAMESPACE_REF}` collides with the branch namespace \
             every track gets its own branch under (`neige/track-<id>`). Git \
             cannot hold both, so this track's worktree could not be made. \
             Rename or delete that branch (`git branch -m neige \
             <another-name>`), then attach again.",
            path.display()
        ))),
        // Fail closed: the question was not answered.
        other => Err(WorkspaceError::BadRequest(format!(
            "attached workspace: cannot tell whether `{}` already holds \
             `{SLICE_BRANCH_NAMESPACE_REF}` (git show-ref exited with {}); \
             refusing rather than letting the track worktree find out. git said: \
             {}",
            path.display(),
            other
                .map(|code| code.to_string())
                .unwrap_or_else(|| "a signal".to_string()),
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

/// Materialize the stored managed directory at the existing worker admission point.
pub fn materialize_managed_workspace(root: &Path, path: &Path, track_id: &str) -> Result<()> {
    workspace::materialize::materialize_managed_workspace(root, path, track_id).map_err(Into::into)
}

pub fn git_repo_root_for_track_cwd(track_id: &str, cwd: &str) -> Result<PathBuf> {
    let cwd_path = Path::new(cwd);
    if cwd.trim().is_empty() || !cwd_path.is_absolute() {
        return Err(WorkspaceError::BadRequest(format!(
            "track {track_id} cwd must be an absolute git repository path for workspace leasing"
        )));
    }
    let output = neige_git_command()
        .arg("-C")
        .arg(cwd_path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| {
            WorkspaceError::Internal(format!(
                "spawn git rev-parse --show-toplevel for track {track_id} cwd {}: {e}",
                cwd_path.display()
            ))
        })?;
    if !output.status.success() {
        return Err(WorkspaceError::BadRequest(format!(
            "track {track_id} cwd {} is not a git repository: {}",
            cwd_path.display(),
            workspace::git::output_summary(&output)
        )));
    }
    let repo_root = workspace::base::printed_path(&output.stdout);
    if repo_root.as_os_str().is_empty() {
        return Err(WorkspaceError::BadRequest(format!(
            "track {track_id} cwd {} did not resolve to a git repository root",
            cwd_path.display()
        )));
    }
    if !repo_root.is_absolute() {
        return Err(WorkspaceError::BadRequest(format!(
            "track {track_id} git repository root must be absolute: {}",
            repo_root.display()
        )));
    }
    // Refused where it is read: `repo_root` is stored and frozen
    // through `json!` by both adapters, so it is UTF-8 by construction.
    workspace::base::utf8_path(&repo_root, "track git repository root")?;
    Ok(repo_root)
}

#[cfg(test)]
mod tests;
