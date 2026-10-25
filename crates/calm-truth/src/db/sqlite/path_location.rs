use crate::error::{CalmError, Result};

/// The two observed locations used by the one overlap comparison. An admitted
/// attached-workspace cwd must exist; explicit terminal cwd, owned roots, persisted
/// execution roots and inherited paths may have missing tails.
pub struct PathLocation<'a> {
    lexical: &'a std::path::Path,
    physical: std::path::PathBuf,
}

impl<'a> PathLocation<'a> {
    pub fn existing(path: &'a std::path::Path) -> Result<Self> {
        let physical = std::fs::canonicalize(path).map_err(|error| {
            CalmError::BadRequest(format!("cannot resolve cwd {}: {error}", path.display()))
        })?;
        Ok(Self {
            lexical: path,
            physical,
        })
    }

    /// Relative lexical and physical locations under an owned layout root. Identity
    /// derivation consumes this observation; overlap policy remains `paths_overlap`.
    pub fn relative_to(&self, root: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
        let physical_root = physical_path(root)?;
        Ok([
            (self.lexical, root),
            (self.physical.as_path(), physical_root.as_path()),
        ]
        .into_iter()
        .filter_map(|(path, base)| path.strip_prefix(base).ok().map(ToOwned::to_owned))
        .collect())
    }

    pub fn allow_missing(path: &'a std::path::Path) -> Result<Self> {
        Ok(Self {
            lexical: path,
            physical: physical_path(path)?,
        })
    }
}

/// Compare lexical and physical locations, preserving a missing tail below the nearest
/// existing ancestor. Failure to resolve a location is an error, never a disjoint proof.
fn physical_path(path: &std::path::Path) -> Result<std::path::PathBuf> {
    workspace::worktree::physical_path(path)
        .map_err(|workspace::error::Error::Internal(message)| CalmError::BadRequest(message))
}

pub fn paths_overlap(
    candidate: &PathLocation<'_>,
    owned: &std::path::Path,
    containing: bool,
) -> Result<bool> {
    let overlaps = |a: &std::path::Path, b: &std::path::Path| {
        a.starts_with(b) || (containing && b.starts_with(a))
    };
    if overlaps(candidate.lexical, owned) {
        return Ok(true);
    }
    Ok(overlaps(&candidate.physical, &physical_path(owned)?))
}
