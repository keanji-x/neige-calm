//! Area folder claim rules: the single place that decides whether an absolute path overlaps an existing
//! `area_folders` claim, and which claim covers a path. Every writer scans and inserts inside one `BEGIN IMMEDIATE` transaction.

use calm_types::model::{AreaFolder, FolderConflict, FolderConflictKind};

/// Normalize an absolute filesystem path for storage / comparison: trims exactly one trailing slash unless the
/// string is the root `/`. Does not validate absolute-ness (a 400 at the route layer).
pub fn normalize_path(raw: &str) -> String {
    if raw == "/" {
        return "/".to_string();
    }
    if let Some(stripped) = raw.strip_suffix('/') {
        return stripped.to_string();
    }
    raw.to_string()
}

/// True when `candidate` is a descendant of `parent` (or equal); the `+ "/"` guard prevents `/abc` matching parent `/ab`.
pub fn is_descendant_of(parent: &str, candidate: &str) -> bool {
    if parent == candidate {
        return true;
    }
    // Root: every absolute path is a descendant, but the join below would produce `"//..."`.
    if parent == "/" {
        return candidate.starts_with('/');
    }
    candidate.starts_with(&format!("{parent}/"))
}

/// The claim that covers `normalized` — the one covering-scan rule both resolvers share. No tiebreak: overlapping
/// claims are impossible, so at most one row matches; on a corrupted table this returns the first match in the caller's
/// iteration order (`ORDER BY path ASC`), which keeps both resolvers deterministic and identical.
pub fn find_owner<'a>(existing: &'a [AreaFolder], normalized: &str) -> Option<&'a AreaFolder> {
    existing
        .iter()
        .find(|f| is_descendant_of(&f.path, normalized))
}

/// Classify `normalized` against every existing claim, returning the first overlap as a 409 body (labelled from the
/// proposed path's point of view). Must run inside the same transaction as the INSERT that follows it.
pub fn classify_conflict(existing: &[AreaFolder], normalized: &str) -> Option<FolderConflict> {
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
            area_id: f.area_id.clone(),
            conflict_path: f.path.clone(),
            conflict_kind,
        })
    })
}

/// Every overlapping pair in `existing`, as `(row, conflict)` labelled from `row`'s point of view. Databases created
/// before the atomic writer can hold overlapping rows, and `find_owner` would silently resolve them by iteration order,
/// so the boot fence uses this to refuse the ambiguity. The predicate is [`classify_conflict`], never a second copy.
pub fn overlapping_pairs(existing: &[AreaFolder]) -> Vec<(&AreaFolder, FolderConflict)> {
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

/// Outcome of an atomic claim attempt; `Conflict` is a normal outcome so the route can render the structured 409 body.
#[derive(Debug, Clone)]
pub enum AreaFolderClaim {
    Created(AreaFolder),
    Conflict(FolderConflict),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: i64, area: &str, path: &str) -> AreaFolder {
        AreaFolder {
            id,
            area_id: area.to_string().into(),
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

    /// `normalize_path` is not idempotent (strips exactly one trailing slash), does not resolve `..`, and does not
    /// collapse interior slashes: a storage normalizer, not a path canonicalizer.
    #[test]
    fn normalize_pins_the_edges_it_actually_has() {
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path("//"), "/");
        // NOT idempotent: exactly one trailing slash comes off per call.
        assert_eq!(normalize_path("///"), "//");
        assert_eq!(normalize_path(&normalize_path("///")), "/");

        assert_eq!(normalize_path("/a"), "/a");
        assert_eq!(normalize_path("/a/"), "/a");
        assert_eq!(normalize_path("/a/b/"), "/a/b");

        assert_eq!(normalize_path("/a/../b"), "/a/../b");
        assert_eq!(normalize_path("/a/.."), "/a/..");
        assert_eq!(normalize_path("/a//b"), "/a//b");

        assert_eq!(normalize_path("a/b"), "a/b");
        assert_eq!(normalize_path(""), "");
    }

    /// KNOWN GAP — a doubled trailing slash survives normalization (`/a//` → `/a/`), and `is_descendant_of` probes
    /// with `"/a//"`, which nothing under `/a` starts with: `/a/b` can then be claimed alongside `/a/`, and the boot
    /// disjointness fence (built on the same `classify_conflict`) cannot see that pair. Asserted rather than fixed.
    #[test]
    fn a_doubled_trailing_slash_produces_a_claim_that_covers_nothing() {
        let stored = normalize_path("/a//");
        assert_eq!(stored, "/a/", "one slash comes off, one stays");
        assert!(
            !is_descendant_of(&stored, "/a/b"),
            "KNOWN GAP (#1147 N19): a claim stored as `{stored}` matches nothing \
             beneath it. If this fails, normalization was fixed — replace this \
             test with the positive one."
        );
        // The un-doubled form does cover it, which is what makes this a real divergence.
        assert!(is_descendant_of(&normalize_path("/a/"), "/a/b"));
    }

    /// KNOWN GAP, consequence 1: two claims, one subtree, stated through `classify_conflict` as production applies it.
    #[test]
    fn n19_lets_two_claims_cover_one_subtree() {
        let stored = vec![folder(1, "c1", &normalize_path("/a//"))];
        assert!(
            classify_conflict(&stored, "/a").is_some(),
            "premise: `/a` is NOT part of this gap; it is caught"
        );
        assert!(
            classify_conflict(&stored, "/a/b").is_none(),
            "KNOWN GAP (#1147 N19): `/a/b` must be admitted alongside `/a/`, \
             giving two claims over one subtree — issue #275's invariant. If \
             this fails, the gap was closed."
        );
    }

    /// KNOWN GAP, consequence 2: `overlapping_pairs` backs the fail-closed boot fence, which cannot see the pair.
    #[test]
    fn n19_is_invisible_to_the_boot_disjointness_fence() {
        let table = vec![
            folder(1, "c1", &normalize_path("/a//")),
            folder(2, "c2", "/a/b"),
        ];
        assert_eq!(
            overlapping_pairs(&table).len(),
            0,
            "KNOWN GAP (#1147 N19): the boot fence reports this table disjoint \
             even though both rows cover `/a/b`. If this fails, the fence (or \
             normalization) was fixed — replace this test with the positive one."
        );
        // The same shape without the doubled slash IS caught: the fence works, the gap is in what it is given.
        let sane = vec![folder(1, "c1", "/a"), folder(2, "c2", "/a/b")];
        assert_eq!(overlapping_pairs(&sane).len(), 1);
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

    /// The degenerate corrupt-state answer must be pinned: both resolvers call `find_owner` over `ORDER BY path ASC` rows.
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

    /// The string-prefix trap: `/a` is a string prefix of `/ab`, but not a *path* prefix.
    #[test]
    fn overlapping_pairs_ignores_shared_string_prefix_siblings() {
        let existing = vec![folder(1, "c1", "/a"), folder(2, "c2", "/ab")];
        assert!(overlapping_pairs(&existing).is_empty());
        let deep = vec![folder(1, "c1", "/home/kenji"), folder(2, "c2", "/home/ken")];
        assert!(overlapping_pairs(&deep).is_empty());
    }

    #[test]
    fn overlapping_pairs_reports_ancestor_and_descendant_from_row_pov() {
        // Rows arrive `ORDER BY path ASC`, so the pair is labelled from `/a`'s point of view.
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

    /// `UNIQUE(area_folders.path)` blocks duplicates through any writer, but a hand-edited DB could still present them.
    #[test]
    fn overlapping_pairs_catches_equal_paths_even_though_unique_blocks_them() {
        let existing = vec![folder(1, "c1", "/a"), folder(2, "c2", "/a")];
        let pairs = overlapping_pairs(&existing);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].1.conflict_kind, FolderConflictKind::Equal);
    }

    #[test]
    fn overlapping_pairs_enumerates_every_colliding_pair_once() {
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

    /// Root is an ancestor of everything.
    #[test]
    fn overlapping_pairs_catches_root_claim() {
        let existing = vec![folder(1, "c1", "/"), folder(2, "c2", "/a")];
        let pairs = overlapping_pairs(&existing);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].1.conflict_kind, FolderConflictKind::Ancestor);
    }
}
