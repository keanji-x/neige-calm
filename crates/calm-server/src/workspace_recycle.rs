//! Safe recycling of managed track workspaces: a directory is renamed into `<root>/.trash/` only when it is
//! `Managed`, canonically under the root, carries this track's ownership marker, and is not in the system area.
//! Fail-closed; never `rm -rf`, and no copy+delete fallback for a cross-device rename.

use std::path::{Path, PathBuf};

use crate::error::{CalmError, Result};
use crate::model::{AreaKind, Track, TrackWorkspace, TrackWorkspaceKind};

/// Leading dot so it can never collide with an area id and is not mistaken for an area by root walkers.
pub const TRASH_DIR_NAME: &str = ".trash";

/// Kept in sync with `workspace_materialize::OWNER_MARKER`; the recycle tests read the marker only through the materializer.
const OWNER_MARKER_RELATIVE: [&str; 2] = [".git", "neige-workspace"];

/// Boot/lazy recovery fence: a managed workspace is recoverable only while its original path still carries
/// this track's marker, which persists deletion quarantine across restarts.
pub fn workspace_allows_runtime_recovery(track: &Track) -> bool {
    if track.workspace.kind != TrackWorkspaceKind::Managed {
        return true;
    }
    let marker = OWNER_MARKER_RELATIVE
        .iter()
        .fold(PathBuf::from(&track.workspace.path), |path, part| {
            path.join(part)
        });
    std::fs::read_to_string(marker).is_ok_and(|contents| contents.trim() == track.id.as_str())
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
    pub fn trashed_path(&self) -> Option<&Path> {
        match self {
            RecycleDecision::Trashed { to, .. } => Some(to.as_path()),
            RecycleDecision::Refused(_) => None,
        }
    }

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
    match std::fs::symlink_metadata(from) {
        Ok(_) => {
            return Err(CalmError::Internal(format!(
                "cannot restore recycled workspace {}: original path is occupied",
                from.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(CalmError::Internal(format!(
                "inspect original workspace path {} before restore: {error}",
                from.display()
            )));
        }
    }
    std::fs::rename(to, from).map_err(|error| {
        CalmError::Internal(format!(
            "restore recycled workspace {} -> {}: {error}",
            to.display(),
            from.display()
        ))
    })?;
    tracing::warn!(
        from = %to.display(),
        to = %from.display(),
        "restored recycled workspace after track deletion rolled back"
    );
    Ok(())
}

fn decide_and_move(
    workspace_root: &Path,
    area_kind: Option<AreaKind>,
    track_id: &str,
    workspace: &TrackWorkspace,
    now_ms: i64,
) -> Result<RecycleDecision> {
    // Guard 1 — typed kind, from the stored column, never inferred from the path.
    if workspace.kind != TrackWorkspaceKind::Managed {
        return Ok(RecycleDecision::Refused(RecycleRefusal::NotManaged));
    }

    // Guard 4 — system area; `None` means the area row could not be read, which is a refusal.
    // Unreachable today (the routes 403 first and `area_id` is NOT NULL), kept as the last check before an irreversible move.
    if area_kind != Some(AreaKind::User) {
        return Ok(RecycleDecision::Refused(RecycleRefusal::SystemArea));
    }

    let stored = Path::new(&workspace.path);
    if !stored.is_absolute() {
        return Ok(RecycleDecision::Refused(RecycleRefusal::Unreadable {
            detail: format!("workspace path is not absolute: {}", stored.display()),
        }));
    }

    // Guard 2 — canonical containment, on BOTH sides.
    let real_root = match std::fs::canonicalize(workspace_root) {
        Ok(root) => root,
        Err(error) => {
            return Ok(RecycleDecision::Refused(RecycleRefusal::Unreadable {
                detail: format!(
                    "canonicalize workspace root {}: {error}",
                    workspace_root.display()
                ),
            }));
        }
    };
    let real_path = match std::fs::canonicalize(stored) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RecycleDecision::Refused(RecycleRefusal::PathMissing));
        }
        Err(error) => {
            return Ok(RecycleDecision::Refused(RecycleRefusal::Unreadable {
                detail: format!("canonicalize {}: {error}", stored.display()),
            }));
        }
    };
    // Component-wise `starts_with` still accepts the root itself and anything inside the trash: the first would
    // rename the whole root away, the second would nest trash in trash on a retry.
    let trash_root = real_root.join(TRASH_DIR_NAME);
    if !real_path.starts_with(&real_root)
        || real_path == real_root
        || real_path.starts_with(&trash_root)
    {
        return Ok(RecycleDecision::Refused(RecycleRefusal::OutsideRoot {
            real: real_path,
        }));
    }
    // Depth matters too: a valid marker on the `<root>/<area_id>/` layer would otherwise move the entire
    // area directory — every sibling track — into the trash.
    if real_path.parent().and_then(Path::parent) != Some(real_root.as_path()) {
        return Ok(RecycleDecision::Refused(RecycleRefusal::WrongDepth {
            real: real_path,
        }));
    }

    // Guard 3 — our marker, naming THIS track, read from the canonical path so a link cannot decide which
    // marker answers for which directory.
    let marker_path = OWNER_MARKER_RELATIVE
        .iter()
        .fold(real_path.clone(), |acc, part| acc.join(part));
    match std::fs::read_to_string(&marker_path) {
        Ok(contents) if contents.trim() == track_id => {}
        Ok(contents) => {
            return Ok(RecycleDecision::Refused(RecycleRefusal::MarkerMismatch {
                found: contents.trim().to_string(),
            }));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RecycleDecision::Refused(RecycleRefusal::MarkerMissing));
        }
        Err(error) => {
            return Ok(RecycleDecision::Refused(RecycleRefusal::Unreadable {
                detail: format!("read ownership marker {}: {error}", marker_path.display()),
            }));
        }
    }

    // All four hold. Move, never delete.
    let to = move_into_trash(&real_root, &trash_root, &real_path, track_id, now_ms)?;
    Ok(RecycleDecision::Trashed {
        from: real_path,
        to,
    })
}

/// `rename` into `<root>/.trash/<track_id>-<ts>` — no other suffix, so [`gc_trash`] can date it by `rsplit_once('-')`.
/// The trash root is canonicalized after creation and every candidate must be its direct child (a symlinked
/// `.trash` or a `../` in the id would otherwise rename the workspace outside the root); the landing spot is
/// re-verified after the rename because `.trash` can be swapped in between. Detection, not prevention.
fn move_into_trash(
    real_root: &Path,
    trash_root: &Path,
    from: &Path,
    track_id: &str,
    now_ms: i64,
) -> Result<PathBuf> {
    std::fs::create_dir_all(trash_root).map_err(|error| {
        CalmError::Internal(format!(
            "recycle workspace: create trash dir {}: {error}",
            trash_root.display()
        ))
    })?;
    let trash_root = std::fs::canonicalize(trash_root).map_err(|error| {
        CalmError::Internal(format!(
            "recycle workspace: canonicalize trash dir {}: {error}",
            trash_root.display()
        ))
    })?;
    if trash_root.parent() != Some(real_root) {
        return Err(CalmError::Internal(format!(
            "recycle workspace: the trash directory {} resolves outside the managed \
             workspace root {} — most likely `{TRASH_DIR_NAME}` is a symlink. \
             Renaming into it would move the workspace out of the tree the GC can \
             see, i.e. leak it permanently while reporting success.",
            trash_root.display(),
            real_root.display()
        )));
    }
    let trash_root = trash_root.as_path();
    // Test seam: fires between the canonicalize above and the rename below. Production compiles nothing for it.
    #[cfg(test)]
    tests::fire_pre_rename_hook(trash_root);
    let mut stamp = now_ms;
    for _ in 0..1000 {
        let candidate = trash_root.join(format!("{track_id}-{stamp}"));
        // `track_id` is interpolated, not validated upstream: a `../` would make `join` escape the trash root.
        if candidate.parent() != Some(trash_root) {
            return Err(CalmError::Internal(format!(
                "recycle workspace: track id `{track_id}` does not form a single path \
                 segment; {} is not directly inside {}. Refusing to rename anywhere \
                 the GC cannot reach.",
                candidate.display(),
                trash_root.display()
            )));
        }
        // `rename` silently replaces an existing *empty* directory; `symlink_metadata` so a dangling symlink counts as occupied.
        if std::fs::symlink_metadata(&candidate).is_ok() {
            stamp += 1;
            continue;
        }
        std::fs::rename(from, &candidate).map_err(|error| {
            CalmError::Internal(format!(
                "recycle workspace: rename {} -> {}: {error}. A cross-device rename \
                 (EXDEV) is fatal on purpose: falling back to copy + delete would turn \
                 this into a recursive delete of a live directory, which is the exact \
                 failure this path exists to make impossible.",
                from.display(),
                candidate.display()
            ))
        })?;
        verify_landed_inside_trash(trash_root, from, &candidate)?;
        return Ok(candidate);
    }
    Err(CalmError::Internal(format!(
        "recycle workspace: could not find a free trash slot for track `{track_id}` under {}",
        trash_root.display()
    )))
}

/// `trash_root` is the canonical trash root as resolved *before* the rename; a swapped `.trash` makes the
/// landed parent differ. A failure here is an error, never `Refused`: the directory has already moved.
fn verify_landed_inside_trash(trash_root: &Path, from: &Path, candidate: &Path) -> Result<()> {
    let landed = std::fs::canonicalize(candidate).map_err(|error| {
        CalmError::Internal(format!(
            "recycle workspace: cannot confirm where {} landed: {error}. The rename \
             reported success, so the workspace has moved somewhere this process can \
             no longer name. Refusing to report a successful recycle.",
            candidate.display()
        ))
    })?;
    if landed.parent() == Some(trash_root) {
        return Ok(());
    }
    // Detection, not prevention. Try to undo it; report either way.
    let restored = std::fs::rename(&landed, from).is_ok();
    Err(CalmError::Internal(format!(
        "recycle workspace: the rename landed at {}, whose parent is not the trash \
         directory {} resolved a moment earlier. `{TRASH_DIR_NAME}` was replaced \
         between the two steps (#1147 N16), so the workspace was moved outside the \
         tree the GC can see. {} Reporting an error rather than a successful recycle: \
         a silent permanent leak is worse than a failure, because nothing downstream \
         can notice it.",
        landed.display(),
        trash_root.display(),
        if restored {
            format!("It has been moved back to {}.", from.display())
        } else {
            format!(
                "It could NOT be moved back to {} and is still at {} — recover it by hand.",
                from.display(),
                landed.display()
            )
        }
    )))
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
                    return Err(CalmError::Internal(format!(
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
        Err(CalmError::Internal(errors.join("; ")))
    }
}

/// Cosmetic post-commit finalization; keeping the area directory until commit means rollback never recreates a path.
pub fn finalize_area_recycle(workspace_root: &Path, area_id: &str, report: &mut AreaRecycleReport) {
    report.area_dir_removed = remove_empty_area_dir(workspace_root, area_id);
}

/// `rmdir <root>/<area_id>` when empty and canonically a direct child of the root; failure is `false`, never an error.
fn remove_empty_area_dir(workspace_root: &Path, area_id: &str) -> bool {
    let Ok(real_root) = std::fs::canonicalize(workspace_root) else {
        return false;
    };
    let area_dir = real_root.join(area_id);
    let Ok(real_area_dir) = std::fs::canonicalize(&area_dir) else {
        return false;
    };
    if real_area_dir.parent() != Some(real_root.as_path()) {
        tracing::error!(
            area_id,
            path = %real_area_dir.display(),
            root = %real_root.display(),
            "refusing to remove an area workspace directory that does not resolve to a \
             direct child of the managed workspace root"
        );
        return false;
    }
    match std::fs::remove_dir(&real_area_dir) {
        Ok(()) => true,
        Err(error) => {
            tracing::info!(
                area_id,
                path = %real_area_dir.display(),
                error = %error,
                "left the area workspace directory in place (not empty, or not removable)"
            );
            false
        }
    }
}

/// Delete trash entries older than [`TRASH_RETENTION_MS`]; the one `remove_dir_all` in the module. Looks only at
/// direct children of the canonical trash, dates entries by the timestamp in their name (`rename` preserves
/// mtime), and keeps anything it cannot date or that is not a real directory.
pub fn gc_trash(workspace_root: &Path, now_ms: i64, retention_ms: i64) -> Result<Vec<PathBuf>> {
    let real_root = match std::fs::canonicalize(workspace_root) {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(CalmError::Internal(format!(
                "trash gc: canonicalize workspace root {}: {error}",
                workspace_root.display()
            )));
        }
    };
    let trash_root = real_root.join(TRASH_DIR_NAME);
    let entries = match std::fs::read_dir(&trash_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(CalmError::Internal(format!(
                "trash gc: read {}: {error}",
                trash_root.display()
            )));
        }
    };

    let mut removed = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(error = %error, "trash gc: unreadable entry, keeping");
                continue;
            }
        };
        let path = entry.path();
        // `symlink_metadata` so a symlink planted in the trash is skipped, not followed.
        let is_real_dir = std::fs::symlink_metadata(&path)
            .map(|meta| meta.file_type().is_dir())
            .unwrap_or(false);
        if !is_real_dir {
            tracing::warn!(path = %path.display(), "trash gc: not a directory, keeping");
            continue;
        }
        let Some(stamp) = trash_entry_timestamp(&path) else {
            tracing::warn!(
                path = %path.display(),
                "trash gc: entry name carries no timestamp, keeping"
            );
            continue;
        };
        if now_ms.saturating_sub(stamp) < retention_ms {
            continue;
        }
        // Assert containment against the canonical entry too, so a race that swapped it for a link cannot redirect the delete.
        match std::fs::canonicalize(&path) {
            Ok(real) if real.parent() == Some(trash_root.as_path()) => {}
            _ => {
                tracing::warn!(path = %path.display(), "trash gc: entry moved or escaped, keeping");
                continue;
            }
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {
                tracing::info!(path = %path.display(), "trash gc: removed expired workspace");
                removed.push(path);
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), error = %error, "trash gc: remove failed");
            }
        }
    }
    Ok(removed)
}

/// `<track_id>-<ts_ms>` → `ts_ms`. `None` for any other shape.
fn trash_entry_timestamp(path: &Path) -> Option<i64> {
    let name = path.file_name()?.to_str()?;
    let (_, stamp) = name.rsplit_once('-')?;
    stamp.parse::<i64>().ok()
}

/// Sweep the trash, swallowing failures: GC must never turn a successful delete into a 500.
pub fn gc_trash_best_effort(workspace_root: &Path, now_ms: i64) {
    match gc_trash(workspace_root, now_ms, TRASH_RETENTION_MS) {
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
