//! Workspace selection and attached admission policy.
use crate::error::{CalmError, Result};
use crate::model::{TrackWorkspace, TrackWorkspaceKind};
use std::path::Path;
use workspace::command::neige_git_command;

/// Short digest of a workspace path for an idempotency key: a key that does not name the path turns any
/// re-point into a permanent 409, since operation rows are never deleted.
pub(crate) fn workspace_key_digest(path: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    hex::encode(hasher.finalize())[..16].to_string()
}

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
    if !path.is_absolute() {
        return Err(CalmError::BadRequest(format!(
            "attached workspace: path must be absolute (start with `/`); got `{}`",
            path.display()
        )));
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(CalmError::BadRequest(format!(
                "attached workspace: `{}` is not a directory",
                path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(CalmError::BadRequest(format!(
                "attached workspace: `{}` does not exist. Neige never creates an \
                 attached directory — it is yours, so it has to be there already.",
                path.display()
            )));
        }
        Err(error) => {
            return Err(CalmError::BadRequest(format!(
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
            CalmError::BadRequest(format!(
                "attached workspace: cannot run git in `{}`: {error}",
                path.display()
            ))
        })?;
    if !output.status.success() {
        return Err(CalmError::BadRequest(format!(
            "attached workspace: `{}` is not inside a Git work tree, so no worker \
             could ever get a workspace lease there. git said: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    ensure_slice_branch_namespace_is_free(path)?;
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
            CalmError::BadRequest(format!(
                "attached workspace: cannot run git in `{}`: {error}",
                path.display()
            ))
        })?;
    match output.status.code() {
        Some(1) => Ok(()),
        Some(0) => Err(CalmError::BadRequest(format!(
            "attached workspace: `{}` has a branch named `neige`, and \
             `{SLICE_BRANCH_NAMESPACE_REF}` collides with the branch namespace \
             every track gets its own branch under (`neige/track-<id>`). Git \
             cannot hold both, so this track's worktree could not be made. \
             Rename or delete that branch (`git branch -m neige \
             <another-name>`), then attach again.",
            path.display()
        ))),
        // Fail closed: the question was not answered.
        other => Err(CalmError::BadRequest(format!(
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
pub(crate) fn materialize_managed_workspace(
    root: &Path,
    path: &Path,
    track_id: &str,
) -> Result<()> {
    workspace::materialize::materialize_managed_workspace(root, path, track_id).map_err(Into::into)
}

#[cfg(test)]
mod tests;
