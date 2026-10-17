//! Kernel workspace policy. Mechanisms live in the `workspace` package.
use crate::error::{CalmError, Result};
use std::path::{Path, PathBuf};
use workspace::command::neige_git_command;
pub(crate) mod execution_input;
pub(crate) mod execution_sources;
pub(crate) mod identity;
pub(crate) mod sweep;
pub(crate) mod track_worktree;
pub mod workspace_materialize;
pub mod workspace_recycle;
pub(crate) mod worktree_clean;
pub(crate) fn git_repo_root_for_track_cwd(track_id: &str, cwd: &str) -> Result<PathBuf> {
    let cwd_path = Path::new(cwd);
    if cwd.trim().is_empty() || !cwd_path.is_absolute() {
        return Err(CalmError::BadRequest(format!(
            "track {track_id} cwd must be an absolute git repository path for workspace leasing"
        )));
    }
    let output = neige_git_command()
        .arg("-C")
        .arg(cwd_path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| {
            CalmError::Internal(format!(
                "spawn git rev-parse --show-toplevel for track {track_id} cwd {}: {e}",
                cwd_path.display()
            ))
        })?;
    if !output.status.success() {
        return Err(CalmError::BadRequest(format!(
            "track {track_id} cwd {} is not a git repository: {}",
            cwd_path.display(),
            workspace::git::output_summary(&output)
        )));
    }
    let repo_root = workspace::base::printed_path(&output.stdout);
    if repo_root.as_os_str().is_empty() {
        return Err(CalmError::BadRequest(format!(
            "track {track_id} cwd {} did not resolve to a git repository root",
            cwd_path.display()
        )));
    }
    if !repo_root.is_absolute() {
        return Err(CalmError::BadRequest(format!(
            "track {track_id} git repository root must be absolute: {}",
            repo_root.display()
        )));
    }
    // Refused where it is read: `repo_root` is stored and frozen
    // through `json!` by both adapters, so it is UTF-8 by construction.
    workspace::base::utf8_path(&repo_root, "track git repository root")?;
    Ok(repo_root)
}
