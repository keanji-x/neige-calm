//! Candidate ref cleanup on Track deletion (D9, #1727 S4 slice 3).
//!
//! `refs/neige/candidates/<track>/…` refs live in the repository's common dir; the Track cwd may
//! be a linked worktree that has since moved, so the sweep addresses the repository through the
//! lease rows' persisted `git_common_dir` (`--git-dir=<common dir>`), never through the Track
//! cwd or a derived `repo_root`, and never depends on any worktree directory existing. Best
//! effort: every failure is a warning, nothing is retried (G16 — a crash between the row delete
//! and this sweep leaves the refs for a human `for-each-ref` + `update-ref -d`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::workspace_materialize::neige_git_command;

/// The prefix every candidate ref of one Track shares (the ref is
/// `refs/neige/candidates/<track>/<card>/<delivery_id>`, `delivery::candidate_ref_name`).
pub(crate) fn candidate_ref_prefix(track_id: &str) -> String {
    format!("refs/neige/candidates/{track_id}/")
}

/// Delete every candidate ref of `track_id` in each distinct common dir the Track's lease rows
/// recorded (`for-each-ref <prefix>` then `update-ref -d` per ref). Rows without a base (leases
/// claimed before the base columns existed) have no candidates and are not listed.
pub(crate) fn delete_candidate_refs_for_track<'a>(
    track_id: &str,
    git_common_dirs: impl IntoIterator<Item = &'a Path>,
) {
    let distinct: BTreeSet<PathBuf> = git_common_dirs.into_iter().map(Path::to_path_buf).collect();
    let prefix = candidate_ref_prefix(track_id);
    for common_dir in distinct {
        let refs = match list_refs(&common_dir, &prefix) {
            Ok(refs) => refs,
            Err(error) => {
                tracing::warn!(
                    track_id,
                    git_common_dir = %common_dir.display(),
                    error,
                    "candidate ref cleanup could not list refs; leaving them"
                );
                continue;
            }
        };
        for ref_name in refs {
            if let Err(error) = delete_ref(&common_dir, &ref_name) {
                tracing::warn!(
                    track_id,
                    git_common_dir = %common_dir.display(),
                    ref_name,
                    error,
                    "candidate ref cleanup could not delete a ref; leaving it"
                );
            }
        }
    }
}

fn list_refs(common_dir: &Path, prefix: &str) -> Result<Vec<String>, String> {
    let output = neige_git_command()
        .arg(format!("--git-dir={}", common_dir.display()))
        .args(["for-each-ref", "--format=%(refname)", prefix])
        .output()
        .map_err(|error| format!("spawn git for-each-ref: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git for-each-ref exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with(prefix))
        .map(str::to_string)
        .collect())
}

fn delete_ref(common_dir: &Path, ref_name: &str) -> Result<(), String> {
    let output = neige_git_command()
        .arg(format!("--git-dir={}", common_dir.display()))
        .args(["update-ref", "-d", ref_name])
        .output()
        .map_err(|error| format!("spawn git update-ref: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git update-ref -d exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}
