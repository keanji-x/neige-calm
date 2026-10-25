//! Canonical ownership checks, atomic trash renames and filesystem collection.
use crate::error::{Error, Result};
use std::path::{Path, PathBuf};
pub const TRASH_DIR_NAME: &str = ".trash";
/// Why a directory was **not** recycled; every variant means it was left exactly as it was on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecycleRefusal {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecycleDecision {
    Trashed { from: PathBuf, to: PathBuf },
    Refused(RecycleRefusal),
}

impl RecycleDecision {
    #[cfg(test)]
    fn trashed_path(&self) -> Option<&Path> {
        match self {
            RecycleDecision::Trashed { to, .. } => Some(to.as_path()),
            RecycleDecision::Refused(_) => None,
        }
    }

    #[cfg(test)]
    fn refusal(&self) -> Option<&RecycleRefusal> {
        match self {
            RecycleDecision::Trashed { .. } => None,
            RecycleDecision::Refused(refusal) => Some(refusal),
        }
    }
}

pub fn restore_recycled_workspace(from: &Path, to: &Path) -> Result<()> {
    match std::fs::symlink_metadata(from) {
        Ok(_) => {
            return Err(Error::Internal(format!(
                "cannot restore recycled workspace {}: original path is occupied",
                from.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(Error::Internal(format!(
                "inspect original workspace path {} before restore: {error}",
                from.display()
            )));
        }
    }
    std::fs::rename(to, from).map_err(|error| {
        Error::Internal(format!(
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

pub fn recycle_owned_directory(
    workspace_root: &Path,
    track_id: &str,
    stored: &Path,
    now_ms: i64,
) -> Result<RecycleDecision> {
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
    let marker_path = crate::materialize::owner_marker_path(&real_path);
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
        Error::Internal(format!(
            "recycle workspace: create trash dir {}: {error}",
            trash_root.display()
        ))
    })?;
    let trash_root = std::fs::canonicalize(trash_root).map_err(|error| {
        Error::Internal(format!(
            "recycle workspace: canonicalize trash dir {}: {error}",
            trash_root.display()
        ))
    })?;
    if trash_root.parent() != Some(real_root) {
        return Err(Error::Internal(format!(
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
            return Err(Error::Internal(format!(
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
            Error::Internal(format!(
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
    Err(Error::Internal(format!(
        "recycle workspace: could not find a free trash slot for track `{track_id}` under {}",
        trash_root.display()
    )))
}

/// `trash_root` is the canonical trash root as resolved *before* the rename; a swapped `.trash` makes the
/// landed parent differ. A failure here is an error, never `Refused`: the directory has already moved.
fn verify_landed_inside_trash(trash_root: &Path, from: &Path, candidate: &Path) -> Result<()> {
    let landed = std::fs::canonicalize(candidate).map_err(|error| {
        Error::Internal(format!(
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
    Err(Error::Internal(format!(
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

/// `rmdir <root>/<area_id>` when empty and canonically a direct child of the root; failure is `false`, never an error.
pub fn remove_empty_area_dir(workspace_root: &Path, area_id: &str) -> bool {
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

/// Delete trash entries older than the caller-supplied retention; the one `remove_dir_all` in the module. Looks only at
/// direct children of the canonical trash, dates entries by the timestamp in their name (`rename` preserves
/// mtime), and keeps anything it cannot date or that is not a real directory.
pub fn gc_trash(
    workspace_root: &Path,
    now_ms: i64,
    retention_ms: i64,
    retained_tracks: &std::collections::HashSet<String>,
) -> Result<Vec<PathBuf>> {
    let real_root = match std::fs::canonicalize(workspace_root) {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::Internal(format!(
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
            return Err(Error::Internal(format!(
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
        let Some((track, stamp)) = trash_entry_identity(&path) else {
            tracing::warn!(
                path = %path.display(),
                "trash gc: entry name carries no timestamp, keeping"
            );
            continue;
        };
        if retained_tracks.contains(track) || now_ms.saturating_sub(stamp) < retention_ms {
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
#[cfg(test)]
pub(crate) fn trash_entry_timestamp(path: &Path) -> Option<i64> {
    trash_entry_identity(path).map(|(_, stamp)| stamp)
}

fn trash_entry_identity(path: &Path) -> Option<(&str, i64)> {
    let name = path.file_name()?.to_str()?;
    let (track, stamp) = name.rsplit_once('-')?;
    Some((track, stamp.parse::<i64>().ok()?))
}

#[cfg(test)]
mod tests;
