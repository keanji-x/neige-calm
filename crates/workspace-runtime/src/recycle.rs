//! Safe recycling of managed track workspaces: a directory is renamed into `<root>/.trash/` only when it is
//! `Managed`, canonically under the root, carries this track's ownership marker, and is not in the system area.
//! Fail-closed; never `rm -rf`, and no copy+delete fallback for a cross-device rename.

use std::path::{Path, PathBuf};

use crate::error::{Result, WorkspaceError};
use calm_types::model::{AreaKind, Track, TrackWorkspace, WorkspaceIsolation};

/// Boot/lazy recovery fence: a managed workspace is recoverable only while its original path still carries
/// this track's marker, which persists deletion quarantine across restarts.
pub fn workspace_allows_runtime_recovery(track: &Track) -> bool {
    match track.workspace.isolation() {
        WorkspaceIsolation::Managed => {
            workspace::materialize::owned_by(Path::new(&track.workspace.path), track.id.as_str())
        }
        WorkspaceIsolation::Worktree => true,
        // Guarantee G: no lifecycle effects on a borrowed directory.
        WorkspaceIsolation::None => true,
    }
}

/// Retention is time-based because "I deleted the wrong track" is noticed on a human clock, not after N more deletions.
pub const TRASH_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Why a directory was **not** recycled; every variant means it was left exactly as it was on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecycleRefusal {
    NotManaged,
    /// The owning area is system-owned, or could not be read (fail-closed).
    SystemArea,
    PathMissing,
    /// `canonicalize` resolved outside the managed root, or onto the root / trash itself.
    OutsideRoot {
        real: PathBuf,
    },
    /// Inside the root, but not at `<root>/<area_id>/<track_id>`.
    WrongDepth {
        real: PathBuf,
    },
    MarkerMissing,
    MarkerMismatch {
        found: String,
    },
    /// The filesystem refused to answer. Fail-closed.
    Unreadable {
        detail: String,
    },
}

impl RecycleRefusal {
    pub fn tag(&self) -> &'static str {
        match self {
            RecycleRefusal::NotManaged => "not-managed",
            RecycleRefusal::SystemArea => "system-area",
            RecycleRefusal::PathMissing => "path-missing",
            RecycleRefusal::OutsideRoot { .. } => "outside-root",
            RecycleRefusal::WrongDepth { .. } => "wrong-depth",
            RecycleRefusal::MarkerMissing => "marker-missing",
            RecycleRefusal::MarkerMismatch { .. } => "marker-mismatch",
            RecycleRefusal::Unreadable { .. } => "unreadable",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecycleDecision {
    Trashed { from: PathBuf, to: PathBuf },
    Refused(RecycleRefusal),
}

impl RecycleDecision {
    pub fn refusal(&self) -> Option<&RecycleRefusal> {
        match self {
            RecycleDecision::Trashed { .. } => None,
            RecycleDecision::Refused(refusal) => Some(refusal),
        }
    }
}

/// Returns `Ok(Refused(..))` when a guard does not hold: refusing to delete a directory must not block
/// deletion of the row. `Err` only when a guard passed and the move itself failed (notably `EXDEV`).
pub fn recycle_track_workspace(
    workspace_root: &Path,
    area_kind: Option<AreaKind>,
    track_id: &str,
    workspace: &TrackWorkspace,
    now_ms: i64,
) -> Result<RecycleDecision> {
    let decision = decide_and_move(workspace_root, area_kind, track_id, workspace, now_ms)?;
    match &decision {
        RecycleDecision::Trashed { from, to } => {
            tracing::info!(
                track_id,
                from = %from.display(),
                to = %to.display(),
                "recycled managed track workspace into the trash"
            );
        }
        RecycleDecision::Refused(RecycleRefusal::NotManaged | RecycleRefusal::PathMissing) => {
            tracing::debug!(
                track_id,
                path = %workspace.path,
                reason = decision.refusal().map(|r| r.tag()).unwrap_or_default(),
                "no managed workspace to recycle"
            );
        }
        RecycleDecision::Refused(refusal) => {
            tracing::error!(
                track_id,
                path = %workspace.path,
                reason = refusal.tag(),
                detail = ?refusal,
                "refusing to recycle a track workspace; the directory is left on disk. \
                 This is fail-closed by design (#1147 S5): a guard could not be \
                 satisfied, so the bytes stay."
            );
        }
    }
    Ok(decision)
}

/// Compensate a successful trash rename when the owning database deletion rolls back. The original path
/// must still be absent: replacing anything that appeared there would trade a delete failure for data loss.
pub fn restore_recycled_workspace(decision: &RecycleDecision) -> Result<()> {
    let RecycleDecision::Trashed { from, to } = decision else {
        return Ok(());
    };
    workspace::recycle::restore_recycled_workspace(from, to).map_err(Into::into)
}

fn decide_and_move(
    workspace_root: &Path,
    area_kind: Option<AreaKind>,
    track_id: &str,
    workspace: &TrackWorkspace,
    now_ms: i64,
) -> Result<RecycleDecision> {
    // Guard 1 — typed kind, from the stored column, never inferred from the path.
    match workspace.isolation() {
        WorkspaceIsolation::Managed => {}
        WorkspaceIsolation::Worktree => {
            return Ok(RecycleDecision::Refused(RecycleRefusal::NotManaged));
        }
        // Guarantee G: leave the source directory untouched.
        WorkspaceIsolation::None => {
            return Ok(RecycleDecision::Refused(RecycleRefusal::NotManaged));
        }
    }

    // Guard 4 — system area; `None` means the area row could not be read, which is a refusal.
    // Unreachable today (the routes 403 first and `area_id` is NOT NULL), kept as the last check before an irreversible move.
    if area_kind != Some(AreaKind::User) {
        return Ok(RecycleDecision::Refused(RecycleRefusal::SystemArea));
    }

    let decision = workspace::recycle::recycle_owned_directory(
        workspace_root,
        track_id,
        Path::new(&workspace.path),
        now_ms,
    )?;
    Ok(match decision {
        workspace::recycle::RecycleDecision::Trashed { from, to } => {
            RecycleDecision::Trashed { from, to }
        }
        workspace::recycle::RecycleDecision::Refused(reason) => {
            RecycleDecision::Refused(match reason {
                workspace::recycle::RecycleRefusal::PathMissing => RecycleRefusal::PathMissing,
                workspace::recycle::RecycleRefusal::OutsideRoot { real } => {
                    RecycleRefusal::OutsideRoot { real }
                }
                workspace::recycle::RecycleRefusal::WrongDepth { real } => {
                    RecycleRefusal::WrongDepth { real }
                }
                workspace::recycle::RecycleRefusal::MarkerMissing => RecycleRefusal::MarkerMissing,
                workspace::recycle::RecycleRefusal::MarkerMismatch { found } => {
                    RecycleRefusal::MarkerMismatch { found }
                }
                workspace::recycle::RecycleRefusal::Unreadable { detail } => {
                    RecycleRefusal::Unreadable { detail }
                }
            })
        }
    })
}

pub struct RecycleTarget<'a> {
    pub track_id: &'a str,
    pub workspace: &'a TrackWorkspace,
}

#[derive(Clone, Debug, Default)]
pub struct AreaRecycleReport {
    pub decisions: Vec<(String, RecycleDecision)>,
    pub area_dir_removed: bool,
}

/// Recycle every managed workspace under an area, then the `<root>/<area_id>/` layer with a non-recursive
/// `remove_dir`, so anything left behind keeps the area directory visibly.
pub fn recycle_area_workspaces(
    workspace_root: &Path,
    area_id: &str,
    area_kind: Option<AreaKind>,
    tracks: &[RecycleTarget<'_>],
    now_ms: i64,
) -> Result<AreaRecycleReport> {
    let mut report = AreaRecycleReport::default();
    for target in tracks {
        let decision = match recycle_track_workspace(
            workspace_root,
            area_kind,
            target.track_id,
            target.workspace,
            now_ms,
        ) {
            Ok(decision) => decision,
            Err(error) => {
                if let Err(restore_error) = restore_area_recycle_report(&report) {
                    return Err(WorkspaceError::Internal(format!(
                        "area workspace recycle failed ({error}), and partial rollback failed: {restore_error}"
                    )));
                }
                return Err(error);
            }
        };
        report
            .decisions
            .push((target.track_id.to_string(), decision));
    }

    let _ = area_id;
    Ok(report)
}

/// Best-effort across the whole batch: one occupied path must not prevent other workspaces from returning.
pub fn restore_area_recycle_report(report: &AreaRecycleReport) -> Result<()> {
    let mut errors = Vec::new();
    for (_, decision) in report.decisions.iter().rev() {
        if let Err(error) = restore_recycled_workspace(decision) {
            errors.push(error.to_string());
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(WorkspaceError::Internal(errors.join("; ")))
    }
}

/// Cosmetic post-commit finalization; keeping the area directory until commit means rollback never recreates a path.
pub fn finalize_area_recycle(workspace_root: &Path, area_id: &str, report: &mut AreaRecycleReport) {
    report.area_dir_removed = workspace::recycle::remove_empty_area_dir(workspace_root, area_id);
}

/// Sweep the trash, swallowing failures: GC must never turn a successful delete into a 500.
pub fn gc_trash_best_effort(
    workspace_root: &Path,
    now_ms: i64,
    retained_tracks: &std::collections::HashSet<String>,
) {
    match workspace::recycle::gc_trash(workspace_root, now_ms, TRASH_RETENTION_MS, retained_tracks)
    {
        Ok(removed) if !removed.is_empty() => {
            tracing::info!(count = removed.len(), "trash gc: swept expired workspaces");
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(error = %error, "trash gc: sweep failed");
        }
    }
}

#[cfg(test)]
mod tests;
