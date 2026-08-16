//! Cove folder claim rules — the single place that decides whether an
//! absolute path overlaps an existing `cove_folders` claim, and the
//! single place that decides which claim *covers* a given path.
//!
//! **Issue #275.** Both resolvers (`GET /api/coves/resolve` and the
//! wave-create owner scan) used to carry their own copy of the covering
//! scan, and they disagreed: one took a longest-prefix tiebreak, the
//! other took the first row in `ORDER BY path ASC`. That only mattered
//! because overlapping rows were reachable: the conflict scan ran on one
//! pooled connection and the INSERT on another, so two concurrent claims
//! for `/a` and `/a/b` could both pass a scan that saw an empty table
//! (`UNIQUE(path)` catches only *equal* paths, never overlap).
//!
//! The fix is atomicity, not a tiebreak: every writer now runs its
//! conflict scan and its INSERT inside one `BEGIN IMMEDIATE`
//! transaction, so at most one claim can cover any path. With that
//! invariant actually enforced, [`find_owner`] is a uniqueness oracle
//! and both resolvers can share it verbatim.

use calm_types::model::{CoveFolder, FolderConflict, FolderConflictKind};

/// Normalize an absolute filesystem path for storage / comparison.
///
/// * Trims exactly one trailing slash unless the entire string is the
///   root `/`.
/// * Does **not** validate that the path starts with `/` — that's a
///   separate concern surfaced as a 400 at the route layer so the wire
///   error code is precise.
pub fn normalize_path(raw: &str) -> String {
    if raw == "/" {
        return "/".to_string();
    }
    if let Some(stripped) = raw.strip_suffix('/') {
        return stripped.to_string();
    }
    raw.to_string()
}

/// True when `candidate` is a descendant of `parent` (or equal).
/// Implementation: `parent == candidate` OR `candidate` starts with
/// `parent + "/"`. The `+ "/"` guard prevents `/abc` from matching
/// against parent `/ab`.
pub fn is_descendant_of(parent: &str, candidate: &str) -> bool {
    if parent == candidate {
        return true;
    }
    // Root `/` is a special case — every absolute path is a descendant
    // of it, but naive `candidate.starts_with("/")` is trivially true,
    // so the join below would still produce `"//..."`. Handle directly.
    if parent == "/" {
        return candidate.starts_with('/');
    }
    candidate.starts_with(&format!("{parent}/"))
}

/// The claim that covers `normalized`, i.e. the row whose path is an
/// ancestor of (or equal to) it.
///
/// **The one covering-scan rule.** `GET /api/coves/resolve` and the
/// wave-create owner scan both call this so they can never disagree
/// about which cove owns a cwd (#275). No tiebreak: overlapping claims
/// are impossible because every writer classifies overlap and inserts
/// inside one `BEGIN IMMEDIATE` transaction (see
/// [`classify_conflict`]), so at most one row can match.
///
/// Should the table ever be corrupted out-of-band (a direct SQL write,
/// a repo-level seed that bypasses the checked writer), this returns the
/// **first** match in the caller's iteration order. Callers feed it rows
/// from `cove_folders … ORDER BY path ASC`, which makes the degenerate
/// answer deterministic — and, critically, the *same* answer for both
/// resolvers.
pub fn find_owner<'a>(existing: &'a [CoveFolder], normalized: &str) -> Option<&'a CoveFolder> {
    existing
        .iter()
        .find(|f| is_descendant_of(&f.path, normalized))
}

/// Classify `normalized` against every existing claim, returning the
/// first overlap as a ready-to-serialize 409 body.
///
/// Overlap is symmetric-but-labelled from the *proposed* path's point of
/// view: `Equal` (exact same path), `Ancestor` (proposed path would
/// silently widen an existing narrower claim), `Descendant` (an existing
/// claim already covers the proposed path).
///
/// Must run inside the same transaction as the INSERT that follows it —
/// see [`crate::db::RepoOutOfDomain::cove_folder_create_checked`].
pub fn classify_conflict(existing: &[CoveFolder], normalized: &str) -> Option<FolderConflict> {
    existing.iter().find_map(|f| {
        let conflict_kind = if f.path == normalized {
            FolderConflictKind::Equal
        } else if is_descendant_of(normalized, &f.path) {
            FolderConflictKind::Ancestor
        } else if is_descendant_of(&f.path, normalized) {
            FolderConflictKind::Descendant
        } else {
            return None;
        };
        Some(FolderConflict {
            folder_id: f.id,
            cove_id: f.cove_id.clone(),
            conflict_path: f.path.clone(),
            conflict_kind,
        })
    })
}

/// Every overlapping pair in `existing`, as `(row, conflict)` where
/// `conflict` describes the *other* row of the pair from `row`'s point of
/// view (so `conflict_kind` is `Descendant` when `row` sits under the
/// other row, `Ancestor` when it sits above it, `Equal` when the paths
/// are identical).
///
/// **Why this exists.** The atomic claim writer makes overlap
/// unreachable *going forward* (#275), but databases created before it
/// landed can already hold overlapping rows: the wave-attach path's
/// in-tx insert was gated on the request flag alone, never on the scan
/// result, so a cove claiming `/a` plus a wave with `cwd = "/a/b",
/// attach_folder = true` minted an overlapping `/a/b` row from ordinary
/// single-threaded HTTP. [`find_owner`] resolves such a table by
/// iteration order, which silently picks a *different* owner than the
/// longest-prefix rule those databases were written under — so the boot
/// fence uses this to refuse the ambiguity instead of guessing (see
/// [`crate::db::sqlite::assert_cove_folders_disjoint`]).
///
/// Pairs are enumerated once each (i < j), and the predicate is
/// [`classify_conflict`] applied to a one-row slice — deliberately the
/// *same* definition the writers enforce, never a second copy of it.
pub fn overlapping_pairs(existing: &[CoveFolder]) -> Vec<(&CoveFolder, FolderConflict)> {
    let mut pairs = Vec::new();
    for (i, row) in existing.iter().enumerate() {
        for other in &existing[i + 1..] {
            if let Some(conflict) = classify_conflict(std::slice::from_ref(other), &row.path) {
                pairs.push((row, conflict));
            }
        }
    }
    pairs
}

/// Outcome of an atomic claim attempt. `Conflict` is a normal (non-error)
/// outcome so the route can render the structured 409 body; genuine
/// failures (missing cove, sqlite errors) still come back as `Err`.
#[derive(Debug, Clone)]
pub enum CoveFolderClaim {
    Created(CoveFolder),
    Conflict(FolderConflict),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: i64, cove: &str, path: &str) -> CoveFolder {
        CoveFolder {
            id,
            cove_id: cove.to_string().into(),
            path: path.to_string(),
            created_at: 0,
        }
    }

    #[test]
    fn normalize_trims_trailing_slash() {
        assert_eq!(normalize_path("/a/b/"), "/a/b");
        assert_eq!(normalize_path("/a/b"), "/a/b");
    }

    #[test]
    fn normalize_preserves_root() {
        assert_eq!(normalize_path("/"), "/");
    }

    #[test]
    fn descendant_match_basics() {
        assert!(is_descendant_of("/a", "/a"));
        assert!(is_descendant_of("/a", "/a/b"));
        assert!(is_descendant_of("/a", "/a/b/c"));
        assert!(!is_descendant_of("/a", "/ab"));
        assert!(!is_descendant_of("/a", "/b"));
    }

    #[test]
    fn descendant_root_special_case() {
        assert!(is_descendant_of("/", "/"));
        assert!(is_descendant_of("/", "/a"));
        assert!(is_descendant_of("/", "/a/b/c"));
    }

    #[test]
    fn classify_labels_each_overlap_shape() {
        let existing = vec![folder(1, "c1", "/a")];
        assert!(classify_conflict(&existing, "/b").is_none());
        assert_eq!(
            classify_conflict(&existing, "/a").unwrap().conflict_kind,
            FolderConflictKind::Equal
        );
        assert_eq!(
            classify_conflict(&existing, "/a/b").unwrap().conflict_kind,
            FolderConflictKind::Descendant
        );
        let deep = vec![folder(1, "c1", "/a/b")];
        assert_eq!(
            classify_conflict(&deep, "/a").unwrap().conflict_kind,
            FolderConflictKind::Ancestor
        );
    }

    /// #275 — the degenerate corrupt-state answer must be pinned, because
    /// it is what makes the two resolvers provably identical: they both
    /// call `find_owner` over `ORDER BY path ASC` rows, so both pick `/a`.
    #[test]
    fn find_owner_takes_first_row_in_iteration_order() {
        let existing = vec![folder(1, "c1", "/a"), folder(2, "c2", "/a/b")];
        assert_eq!(find_owner(&existing, "/a/b/c").unwrap().path, "/a");
        assert!(find_owner(&existing, "/z").is_none());
    }

    #[test]
    fn overlapping_pairs_empty_for_disjoint_table() {
        let existing = vec![
            folder(1, "c1", "/a"),
            folder(2, "c2", "/b"),
            folder(3, "c3", "/c/d"),
        ];
        assert!(overlapping_pairs(&existing).is_empty());
    }

    /// The string-prefix trap: `/a` is a string prefix of `/ab`, but not
    /// a *path* prefix. Neither direction may register as an overlap.
    #[test]
    fn overlapping_pairs_ignores_shared_string_prefix_siblings() {
        let existing = vec![folder(1, "c1", "/a"), folder(2, "c2", "/ab")];
        assert!(overlapping_pairs(&existing).is_empty());
        let deep = vec![folder(1, "c1", "/home/kenji"), folder(2, "c2", "/home/ken")];
        assert!(overlapping_pairs(&deep).is_empty());
    }

    #[test]
    fn overlapping_pairs_reports_ancestor_and_descendant_from_row_pov() {
        // Rows arrive `ORDER BY path ASC`, so the ancestor comes first
        // and the pair is labelled from `/a`'s point of view: `/a` is an
        // *ancestor* of the `/a/b` row it collides with.
        let existing = vec![folder(1, "c1", "/a"), folder(2, "c2", "/a/b")];
        let pairs = overlapping_pairs(&existing);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0.id, 1);
        assert_eq!(pairs[0].1.folder_id, 2);
        assert_eq!(pairs[0].1.conflict_kind, FolderConflictKind::Ancestor);

        // Reversed iteration order flips only the label, never the count.
        let reversed = vec![folder(2, "c2", "/a/b"), folder(1, "c1", "/a")];
        let pairs = overlapping_pairs(&reversed);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].1.conflict_kind, FolderConflictKind::Descendant);
    }

    /// `UNIQUE(cove_folders.path)` makes duplicate paths unreachable
    /// through any writer, but the fence must not *depend* on that — a
    /// hand-edited DB (or a future schema change) could still present
    /// them, and they are the most ambiguous shape of all.
    #[test]
    fn overlapping_pairs_catches_equal_paths_even_though_unique_blocks_them() {
        let existing = vec![folder(1, "c1", "/a"), folder(2, "c2", "/a")];
        let pairs = overlapping_pairs(&existing);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].1.conflict_kind, FolderConflictKind::Equal);
    }

    #[test]
    fn overlapping_pairs_enumerates_every_colliding_pair_once() {
        // `/a` collides with both `/a/b` and `/a/b/c`; `/a/b` collides
        // with `/a/b/c`. Three pairs, each reported exactly once.
        let existing = vec![
            folder(1, "c1", "/a"),
            folder(2, "c2", "/a/b"),
            folder(3, "c3", "/a/b/c"),
            folder(4, "c4", "/z"),
        ];
        let pairs = overlapping_pairs(&existing);
        assert_eq!(pairs.len(), 3);
        let ids: Vec<(i64, i64)> = pairs.iter().map(|(r, c)| (r.id, c.folder_id)).collect();
        assert_eq!(ids, vec![(1, 2), (1, 3), (2, 3)]);
    }

    /// Root is an ancestor of everything — a `/` claim alongside any
    /// other claim is exactly the ambiguity the fence exists to catch.
    #[test]
    fn overlapping_pairs_catches_root_claim() {
        let existing = vec![folder(1, "c1", "/"), folder(2, "c2", "/a")];
        let pairs = overlapping_pairs(&existing);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].1.conflict_kind, FolderConflictKind::Ancestor);
    }
}
