//! #1147 S5 — safe recycling of managed track workspaces.
//!
//! This is the only place in the tree that removes a track's working directory,
//! and it is the only slice of #1147 that deletes user-visible bytes at all.
//! Everything S1–S4 bought — a typed `kind` that cannot be guessed from the
//! path, an ownership marker written inside `.git/`, canonical (not lexical)
//! root containment, one managed directory per track row — exists so that the
//! four guards below can be *believed*.
//!
//! # The four guards
//!
//! A directory is recycled only when **all four** hold. Any one of them being
//! unknowable — unreadable, unparseable, `canonicalize` failing — counts as not
//! holding. Fail-closed, with no "legacy rows have no marker, let them through"
//! escape hatch: per the design's §前提 二, old data is not migrated and not
//! supported, so such rows do not exist and a compatibility branch would only
//! be a hole.
//!
//! 1. `workspace.kind == Managed`. `Attached` points at a repository the user
//!    owns; the server never creates, moves or deletes it.
//! 2. `fs::canonicalize(path)` is under `fs::canonicalize(workspace_root)`.
//!    **Not** a lexical `starts_with`: S2's red team measured that a symlink
//!    under the root makes a lexical prefix check pass while the real bytes sit
//!    anywhere on the filesystem.
//! 3. `<path>/.git/neige-workspace` exists and its contents equal this track's
//!    id. "Is it a git repository" is not a substitute — S2 measured that
//!    predicate waving a third-party repository through.
//! 4. The owning area is not the system area. The launchpad's workspace is
//!    kernel-maintained (`today_launchpad_ensure_tx` repoints it) and is not
//!    user-recyclable.
//!
//! # Move to trash, do not `rm -rf`
//!
//! Recycling is a `rename` into `<workspace-root>/.trash/<track_id>-<ts>`,
//! never a recursive delete. The point is the blast radius of a *bug*: if some
//! future change weakens a guard, the consequence degrades from "the user's
//! repository is gone" to "there is a stale directory under `.trash`". GC
//! ([`gc_trash`]) is a separate, later, independently-guarded step.
//!
//! A cross-device `rename` (`EXDEV`) is a hard error. There is deliberately no
//! copy+delete fallback: a copy+delete is a recursive delete wearing a
//! disguise, and it would reintroduce exactly the failure mode the rename
//! exists to avoid.

use std::path::{Path, PathBuf};

use crate::error::{CalmError, Result};
use crate::model::{AreaKind, TrackWorkspace, TrackWorkspaceKind};

/// Name of the trash directory under the workspace root. Leading dot so it can
/// never collide with an area id (ids are never dot-prefixed) and so it does not
/// look like an area to anything walking the root.
pub const TRASH_DIR_NAME: &str = ".trash";

/// Ownership marker path relative to a managed workspace. Kept in sync with
/// `workspace_materialize::OWNER_MARKER` by
/// [`crate::workspace_materialize::tests`]-adjacent coverage: the recycle tests
/// read the marker only through the *materializer*, so a rename of the marker
/// on one side and not the other turns those tests red rather than silently
/// making every recycle refuse (or, far worse, every recycle accept).
const OWNER_MARKER_RELATIVE: [&str; 2] = [".git", "neige-workspace"];

/// How long a trashed workspace is retained before [`gc_trash`] removes it.
///
/// Seven days, measured from the timestamp encoded in the entry's own name.
/// Rationale for time-based over count-based: the thing a retention window has
/// to survive is *a person noticing*, and "I deleted the wrong track" is noticed
/// on a human clock, not after N more deletions. A count-based cap (`keep the
/// last 20`) can evict this morning's mistake before lunch if a script deletes
/// 20 tracks, and conversely pins a year-old directory forever on a quiet
/// instance. Seven days covers a weekend plus slack, and the entries are
/// workspaces of *deleted* tracks, so the steady-state cost is bounded by one
/// week of deletions rather than by history.
pub const TRASH_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Why a directory was **not** recycled. Every variant means "left exactly as
/// it was on disk".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecycleRefusal {
    /// Guard 1 — `Attached`. The user's own repository.
    NotManaged,
    /// Guard 4 — the owning area is system-owned (or could not be read, which
    /// is treated the same way).
    SystemArea,
    /// Guard 2 — nothing at the stored path. Nothing to recycle; not an error.
    PathMissing,
    /// Guard 2 — `canonicalize` resolved outside the managed root, or the root
    /// itself could not be canonicalized.
    OutsideRoot { real: PathBuf },
    /// Guard 2 — inside the root, but not at `<root>/<area_id>/<track_id>`.
    /// An area layer or a deeper subdirectory would take siblings with it.
    WrongDepth { real: PathBuf },
    /// Guard 3 — no ownership marker.
    MarkerMissing,
    /// Guard 3 — the marker names a different track.
    MarkerMismatch { found: String },
    /// Any guard — the filesystem refused to answer. Fail-closed.
    Unreadable { detail: String },
}

impl RecycleRefusal {
    /// Short stable tag for logs and test assertions.
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

/// Outcome of one recycle attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecycleDecision {
    /// The directory was renamed into the trash. `to` is where it went.
    Trashed { from: PathBuf, to: PathBuf },
    /// The directory was left untouched, for this reason.
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

/// The single controlled entry point for reclaiming a track workspace.
///
/// Returns `Ok(Refused(..))` when a guard does not hold: refusing to delete a
/// *directory* must not block deletion of the *row*, or a track whose marker was
/// lost (design gap N5) would become permanently undeletable — which is a worse
/// outcome than a leaked directory and pushes users toward `rm -rf` by hand.
/// The refusal is logged at `warn`/`error` so the leak is visible.
///
/// Returns `Err` only when a guard passed and the move itself failed — most
/// notably `EXDEV`.
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
            // Both are ordinary: an attached track, or a managed track whose
            // directory was never materialized / already recycled.
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

fn decide_and_move(
    workspace_root: &Path,
    area_kind: Option<AreaKind>,
    track_id: &str,
    workspace: &TrackWorkspace,
    now_ms: i64,
) -> Result<RecycleDecision> {
    // Guard 1 — typed kind. Checked first and from the stored column, never
    // inferred from the path.
    if workspace.kind != TrackWorkspaceKind::Managed {
        return Ok(RecycleDecision::Refused(RecycleRefusal::NotManaged));
    }

    // Guard 4 — system area. `None` means the area row could not be read, which
    // is "cannot tell", which is a refusal.
    //
    // **Reachability: this guard is entirely unreachable today. Pure depth.**
    // Stated exactly, so nobody deletes it as dead code and nobody mistakes it
    // for a live defence:
    //
    // * `Some(System)` — both delete routes 403 a system area before they get
    //   here. That 403 is the row-layer half of this same invariant; see
    //   `routes/tracks.rs::delete_track`.
    // * `None` — cannot happen either. `tracks.area_id` is
    //   `NOT NULL REFERENCES areas(id) ON DELETE CASCADE`
    //   (`calm-truth/migrations/0001_init.sql`) and the pool sets
    //   `PRAGMA foreign_keys = ON` per connection, so a track row with no area
    //   row is not a representable state.
    //
    // Kept anyway, deliberately: the routes' 403s are policy at a boundary,
    // this is the last check before an irreversible move, and any future
    // internal caller that skips the routes gets it for free. Measured
    // consequence: mutating this guard away turns NO integration test red. Its
    // single-violation fixtures are `a_system_area_workspace_is_refused` and
    // `an_unknown_area_is_refused_rather_than_assumed_user` in the unit suite,
    // and they construct states the database will not.
    if area_kind != Some(AreaKind::User) {
        return Ok(RecycleDecision::Refused(RecycleRefusal::SystemArea));
    }

    let stored = Path::new(&workspace.path);
    if !stored.is_absolute() {
        return Ok(RecycleDecision::Refused(RecycleRefusal::Unreadable {
            detail: format!("workspace path is not absolute: {}", stored.display()),
        }));
    }

    // Guard 2 — canonical containment. `canonicalize` on BOTH sides: comparing
    // a canonical path against a non-canonical root is the same bug as a
    // lexical prefix check, just moved one argument over.
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
    // `starts_with` on canonical paths is component-wise, so it cannot be
    // fooled by a `/root-evil` style prefix — but it CAN accept the root
    // itself and anything already inside the trash. Both are excluded: the
    // first would rename the whole root away, the second would nest trash in
    // trash on a retry.
    let trash_root = real_root.join(TRASH_DIR_NAME);
    if !real_path.starts_with(&real_root)
        || real_path == real_root
        || real_path.starts_with(&trash_root)
    {
        return Ok(RecycleDecision::Refused(RecycleRefusal::OutsideRoot {
            real: real_path,
        }));
    }
    // Containment is not enough: **depth** matters, because what gets renamed
    // is a whole subtree. A managed workspace is `<root>/<area_id>/<track_id>`
    // and nothing else.
    //
    // Measured (red team R1/R2): with only the containment check above, a valid
    // marker sitting on the `<root>/<area_id>/` layer moves the entire area
    // directory into the trash — **including every sibling track's repository**
    // — and a marker on any deeper subdirectory is recyclable too. Today those
    // are closed only by coincidence: nothing writes a marker at those depths.
    // That is not a guard, it is luck, and S3 is precisely the slice that will
    // start writing arbitrary paths into `workspace_path`. `remove_empty_area_dir`
    // already asserts its own depth; the recycle path — the one that moves a
    // whole tree — must not be the weaker of the two.
    //
    // Overlap with the containment check above, measured rather than reasoned
    // about — an earlier revision of this comment stated the opposite and was
    // wrong. Six mutations, `cargo test -p calm-server` (23 lib tests in
    // `workspace_recycle::tests`, 12 integration tests in
    // `domain_api_suite::track_workspace_recycle`):
    //
    // | mutation                                   | lib red | itest red |
    // |--------------------------------------------|---------|-----------|
    // | containment clause -> lexical              |    2    |     0     |
    // | depth clause -> lexical                    |    0    |     0     |
    // | delete containment clause                  |    4    |     0     |
    // | delete depth clause                        |    2    |     0     |
    // | drop `canonicalize`, keep both clauses     |    2    |     1     |
    //
    // What that says:
    //
    // * **Both clauses are load bearing.** Deleting containment loses 4 tests,
    //   including a real safety regression (`a_path_already_inside_the_trash_is_refused`
    //   starts nesting trash inside trash); deleting depth loses the two R1/R2
    //   sibling-destruction tests.
    // * **The canonical-ness that is redundant is depth's, not containment's.**
    //   Making depth lexical turns nothing red, because containment — still
    //   canonical — catches those fixtures first. Making containment lexical
    //   turns the two symlink tests red.
    // * **The single-violation mutation for "canonical, not lexical" is
    //   dropping `canonicalize` itself**, which kills 2 lib + 1 integration
    //   test on its own. No compound mutation is needed to demonstrate it.
    //
    // Containment additionally owns two clauses with no depth equivalent at
    // all: `== real_root`, and the `.trash` exclusion (a trash entry sits at
    // exactly this depth and would otherwise pass).
    if real_path.parent().and_then(Path::parent) != Some(real_root.as_path()) {
        return Ok(RecycleDecision::Refused(RecycleRefusal::WrongDepth {
            real: real_path,
        }));
    }

    // Guard 3 — our marker, naming THIS track. Read from the canonical path:
    // reading it through the symlinked stored path would let a link decide
    // which marker answers for which directory.
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

/// `rename` into `<root>/.trash/<track_id>-<ts>`.
///
/// The name is `<track_id>-<ts_ms>` with **no** other suffix, so [`gc_trash`]
/// can date an entry by `rsplit_once('-')`. Collisions (two recycles of the
/// same track inside one millisecond, or a retry after a crash) bump the
/// timestamp rather than appending a counter, which would break that parse.
///
/// # The destination is validated, not assumed
///
/// The four guards prove where the workspace is coming *from*; on their own
/// they say nothing about where it goes. Two measured holes (red team R6/R11),
/// both closed by the same assertion:
///
/// * **`.trash` is a symlink.** `create_dir_all` follows it, so the workspace
///   lands wherever it points — outside the managed root. `gc_trash`
///   canonicalizes and would then never find it again: a permanent leak, and a
///   silent one, since the recycle itself reports success.
/// * **`track_id` is not a path segment.** It is interpolated straight into the
///   name, so an id containing `../` renames the workspace to an arbitrary
///   location above the root (measured: it landed in `<root>/../escaped-…`).
///   Today ids are uuid-simple, so this is closed by coincidence rather than by
///   a check — exactly the shape worth removing before it stops being true.
///
/// So: canonicalize the trash root *after* creating it, require it to be a
/// direct child of the managed root, and require every candidate to be a
/// direct child of that canonical trash root.
///
/// # …and then verified again, because those checks are only static
///
/// The two checks above close the **static** shape of R6/R11: `.trash` already
/// being a symlink, an id that is not a path segment. They cannot close the
/// window between them and the `rename`, and R22 measured that window closing
/// on the second of 200 attempts: swap `.trash` for a symlink after the
/// canonicalize, and the kernel re-resolves the candidate path at `rename`
/// time, so the workspace lands outside the root while this function returns
/// `Trashed { to: <root>/.trash/… }`.
///
/// The threat model does not justify `openat(O_NOFOLLOW)` + `renameat`:
/// anybody who can create that symlink inside the managed root can already
/// delete the directory outright. **But a return value that lies must go.**
/// "Silently leaked forever, reported as success" is strictly worse than a
/// failure, because nothing downstream — not the GC, not an operator reading
/// logs — has any way to notice.
///
/// So after the rename, canonicalize where the directory actually landed and
/// require its parent to equal the trash root as canonicalized *before* the
/// rename. A swapped `.trash` makes those two differ. On mismatch: try to move
/// the directory back, and fail either way. Never report success.
///
/// This is detection, not prevention. Prevention is `renameat`; the gap is
/// registered as N16.
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
    // Test seam for R22: fires in the exact window between the canonicalize
    // above and the rename below. Production compiles nothing for it.
    #[cfg(test)]
    tests::fire_pre_rename_hook(trash_root);
    let mut stamp = now_ms;
    for _ in 0..1000 {
        let candidate = trash_root.join(format!("{track_id}-{stamp}"));
        // `track_id` is interpolated, not validated upstream. `join` on a name
        // containing `../` produces a path that is no longer a child of the
        // trash root, and `rename` would happily honour it.
        if candidate.parent() != Some(trash_root) {
            return Err(CalmError::Internal(format!(
                "recycle workspace: track id `{track_id}` does not form a single path \
                 segment; {} is not directly inside {}. Refusing to rename anywhere \
                 the GC cannot reach.",
                candidate.display(),
                trash_root.display()
            )));
        }
        // `rename` silently replaces an existing *empty* directory, so an
        // occupied slot is skipped rather than reused. `symlink_metadata`, not
        // `exists`, so a dangling symlink counts as occupied instead of being
        // renamed over.
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

/// Post-`rename` verification — see [`move_into_trash`]'s "verified again"
/// section.
///
/// `trash_root` is the canonical trash root as resolved *before* the rename.
/// If `.trash` was swapped for a symlink in between, `candidate` re-resolves
/// through the new link and its canonical parent is somewhere else entirely,
/// which is what this compares.
///
/// A failure here is an error, never a `Refused`: the directory has already
/// moved, so "leave it alone" is not one of the outcomes on offer. Best effort
/// is made to put it back, and the error says whether that worked — an
/// operator needs to know which of two very different states they are in.
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
    // Detection, not prevention (N16). Try to undo it; report either way.
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

/// One track's identity as far as recycling is concerned.
pub struct RecycleTarget<'a> {
    pub track_id: &'a str,
    pub workspace: &'a TrackWorkspace,
}

/// Report of an area-level recycle.
#[derive(Debug, Default)]
pub struct AreaRecycleReport {
    pub decisions: Vec<(String, RecycleDecision)>,
    /// `true` when `<root>/<area_id>/` was removed (it was empty afterwards).
    pub area_dir_removed: bool,
}

/// Recycle every managed workspace under an area, then the `<root>/<area_id>/`
/// layer itself.
///
/// Before this slice, `DELETE /api/areas/{id}` released leases and swept
/// worktrees but never touched the managed directories, so every area delete
/// left a tree of repositories that no database row pointed at any more.
///
/// The area directory is removed with a **non-recursive** `remove_dir`: it
/// succeeds only when the directory is genuinely empty, which makes "did every
/// child get recycled?" a precondition the kernel cannot get wrong rather than
/// a claim it asserts. Anything left behind (a refused track, a stray file)
/// keeps the area directory, and that is the correct, visible outcome.
pub fn recycle_area_workspaces(
    workspace_root: &Path,
    area_id: &str,
    area_kind: Option<AreaKind>,
    tracks: &[RecycleTarget<'_>],
    now_ms: i64,
) -> Result<AreaRecycleReport> {
    let mut report = AreaRecycleReport::default();
    for target in tracks {
        let decision = recycle_track_workspace(
            workspace_root,
            area_kind,
            target.track_id,
            target.workspace,
            now_ms,
        )?;
        report
            .decisions
            .push((target.track_id.to_string(), decision));
    }

    if area_kind == Some(AreaKind::User) {
        report.area_dir_removed = remove_empty_area_dir(workspace_root, area_id);
    }
    Ok(report)
}

/// `rmdir <root>/<area_id>` when it is empty and canonically a direct child of
/// the root. Never recursive; failure is reported as `false`, never as an
/// error, because an un-removed empty directory is cosmetic.
fn remove_empty_area_dir(workspace_root: &Path, area_id: &str) -> bool {
    let Ok(real_root) = std::fs::canonicalize(workspace_root) else {
        return false;
    };
    let area_dir = real_root.join(area_id);
    let Ok(real_area_dir) = std::fs::canonicalize(&area_dir) else {
        return false;
    };
    // Same canonical containment rule as a track, plus "exactly one level
    // down": an area directory is `<root>/<area_id>` and nothing else.
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

/// Delete trash entries older than [`TRASH_RETENTION_MS`].
///
/// Deliberately a **separate step** from recycling: the rename is what makes a
/// guard bug survivable, and it only does that if the delete is not welded to
/// it. This function is the one place in #1147 that calls `remove_dir_all`, and
/// it is guarded on its own terms:
///
/// * it only ever looks at direct children of the canonical
///   `<root>/.trash`, so it cannot walk out of the trash;
/// * it dates an entry by the timestamp **in its own name**, not by `mtime`.
///   `rename` preserves mtime, so an mtime-based sweep would delete a
///   just-trashed workspace whose last write was two weeks ago — i.e. it would
///   have no retention window at all for exactly the repositories most worth
///   keeping;
/// * an entry whose name it cannot date, or which is not a real directory, is
///   **kept**. Fail-closed here means "do not delete", same as everywhere else
///   in this module.
///
/// Returns the entries it removed.
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
        // `symlink_metadata` so a symlink planted in the trash is seen as a
        // symlink and skipped, rather than followed to whatever it names.
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
        // Belt and braces: the path came from `read_dir` on the canonical
        // trash root, but assert containment against the canonical entry too,
        // so a race that replaced the entry with a link cannot redirect the
        // delete.
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

/// Sweep the trash, swallowing failures.
///
/// Called from the delete routes. GC is housekeeping: it must never turn a
/// successful track/area delete into a 500. The trash only grows when something
/// is recycled, so sweeping on each recycle keeps it bounded by one retention
/// window of deletions without any new background-task plumbing.
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
