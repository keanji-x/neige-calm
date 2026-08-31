//! #1147 S5 unit coverage for the recycle guards and the trash GC.
//!
//! These tests exercise the *mechanism* (does the guard hold, does the rename
//! land where it should, does GC date entries the way it claims). The tests
//! that prove the mechanism is actually wired into deletion — and that a
//! refusal really means "the bytes are still there, byte for byte" — live in
//! `crates/calm-server/tests/cases/wave_workspace_recycle.rs`, driven through
//! the real REST routes.

use std::path::{Path, PathBuf};

use super::*;
use crate::model::{WaveWorkspace, WaveWorkspaceKind};
use crate::workspace_materialize::materialize_managed_workspace;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

// --------------------------------------------------------------------------
// R22 test seam
// --------------------------------------------------------------------------

type PreRenameHook = Box<dyn Fn(&Path)>;

thread_local! {
    /// Runs inside `move_into_trash`, after the trash root is canonicalized and
    /// before the `rename` — the exact window R22 exploits.
    ///
    /// Thread-local, not a global: recycling is synchronous and runs on the
    /// calling thread, so a hook installed by one test cannot leak into another
    /// test running in parallel in the same binary. Deterministic injection
    /// beats a threaded hammer here — the red team needed 2 of 200 attempts to
    /// hit this window by racing, which is exactly the flakiness profile a
    /// regression test must not have.
    static PRE_RENAME_HOOK: std::cell::RefCell<Option<PreRenameHook>> =
        const { std::cell::RefCell::new(None) };
}

pub(super) fn fire_pre_rename_hook(trash_root: &Path) {
    PRE_RENAME_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow().as_ref() {
            hook(trash_root);
        }
    });
}

/// Installs `hook` for the duration of `body`, then clears it.
fn with_pre_rename_hook<T>(hook: impl Fn(&Path) + 'static, body: impl FnOnce() -> T) -> T {
    PRE_RENAME_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    let result = body();
    PRE_RENAME_HOOK.with(|slot| *slot.borrow_mut() = None);
    result
}

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("workspaces");
        std::fs::create_dir_all(&root).unwrap();
        Fixture { _tmp: tmp, root }
    }

    /// A real managed workspace, built by the *production* materializer — so
    /// the ownership marker under test is the one production writes, not a
    /// re-implementation that could drift away from it.
    fn managed(&self, cove_id: &str, wave_id: &str) -> PathBuf {
        let path =
            crate::workspace_materialize::managed_workspace_path(&self.root, cove_id, wave_id);
        materialize_managed_workspace(&self.root, &path, wave_id).unwrap();
        path
    }

    fn workspace(path: &Path, kind: WaveWorkspaceKind) -> WaveWorkspace {
        WaveWorkspace {
            kind,
            path: path.to_string_lossy().into_owned(),
            frozen_at: Some(1),
        }
    }

    fn recycle(&self, wave_id: &str, workspace: &WaveWorkspace) -> RecycleDecision {
        recycle_wave_workspace(
            &self.root,
            Some(CoveKind::User),
            wave_id,
            workspace,
            1_000_000,
        )
        .unwrap()
    }
}

#[test]
fn a_managed_workspace_moves_into_the_trash_and_is_not_deleted() {
    let f = Fixture::new();
    let path = f.managed("cove-1", "wave-1");
    std::fs::write(path.join("work.txt"), b"precious").unwrap();

    let decision = f.recycle(
        "wave-1",
        &Fixture::workspace(&path, WaveWorkspaceKind::Managed),
    );

    let to = decision.trashed_path().expect("should have been trashed");
    assert!(
        !path.exists(),
        "the workspace must be gone from its old path"
    );
    // The whole point of rename-over-delete: the bytes are still readable.
    assert_eq!(std::fs::read(to.join("work.txt")).unwrap(), b"precious");
    assert!(to.join(".git").is_dir(), "the repository moved intact");
    assert_eq!(
        to.parent().unwrap(),
        f.root.join(TRASH_DIR_NAME),
        "trash entries live directly under <root>/.trash"
    );
    assert_eq!(
        to.file_name().unwrap().to_str().unwrap(),
        "wave-1-1000000",
        "the name must be <wave_id>-<ts> so the GC can date it"
    );
}

#[test]
fn an_attached_workspace_is_refused() {
    let f = Fixture::new();
    // Deliberately *inside* the root and carrying a valid marker: the only
    // thing making this refuse is the typed kind. That is the design's point —
    // deletion permission comes from the column, never from the path.
    let path = f.managed("cove-1", "wave-1");
    let decision = f.recycle(
        "wave-1",
        &Fixture::workspace(&path, WaveWorkspaceKind::Attached),
    );
    assert_eq!(decision.refusal(), Some(&RecycleRefusal::NotManaged));
    assert!(path.is_dir());
}

#[test]
fn a_system_cove_workspace_is_refused() {
    let f = Fixture::new();
    let path = f.managed("cove-sys", "launchpad-wave");
    let decision = recycle_wave_workspace(
        &f.root,
        Some(CoveKind::System),
        "launchpad-wave",
        &Fixture::workspace(&path, WaveWorkspaceKind::Managed),
        1_000_000,
    )
    .unwrap();
    assert_eq!(decision.refusal(), Some(&RecycleRefusal::SystemCove));
    assert!(path.join(".git").is_dir());
}

#[test]
fn an_unknown_cove_is_refused_rather_than_assumed_user() {
    let f = Fixture::new();
    let path = f.managed("cove-1", "wave-1");
    let decision = recycle_wave_workspace(
        &f.root,
        None,
        "wave-1",
        &Fixture::workspace(&path, WaveWorkspaceKind::Managed),
        1_000_000,
    )
    .unwrap();
    assert_eq!(decision.refusal(), Some(&RecycleRefusal::SystemCove));
    assert!(path.join(".git").is_dir());
}

#[test]
fn a_missing_marker_is_refused() {
    let f = Fixture::new();
    let path = f.managed("cove-1", "wave-1");
    std::fs::remove_file(path.join(".git").join("neige-workspace")).unwrap();
    let decision = f.recycle(
        "wave-1",
        &Fixture::workspace(&path, WaveWorkspaceKind::Managed),
    );
    assert_eq!(decision.refusal(), Some(&RecycleRefusal::MarkerMissing));
    assert!(path.join(".git").is_dir());
}

#[test]
fn a_marker_naming_another_wave_is_refused() {
    let f = Fixture::new();
    let path = f.managed("cove-1", "wave-1");
    let decision = f.recycle(
        "wave-2",
        &Fixture::workspace(&path, WaveWorkspaceKind::Managed),
    );
    assert_eq!(
        decision.refusal(),
        Some(&RecycleRefusal::MarkerMismatch {
            found: "wave-1".into()
        })
    );
    assert!(path.join(".git").is_dir());
}

#[test]
fn a_symlink_that_resolves_outside_the_root_is_refused() {
    let f = Fixture::new();
    // A real repository outside the managed root, with a *matching* marker —
    // so the only guard standing between it and deletion is the canonical
    // containment check.
    let outside = f._tmp.path().join("elsewhere").join("wave-1");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(outside.join(".git")).unwrap();
    std::fs::write(outside.join(".git").join("neige-workspace"), "wave-1\n").unwrap();
    std::fs::write(outside.join("user-file.txt"), b"not ours").unwrap();

    let cove_dir = f.root.join("cove-1");
    std::fs::create_dir_all(&cove_dir).unwrap();
    let link = cove_dir.join("wave-1");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    // Lexically the stored path is squarely under the root.
    assert!(link.starts_with(&f.root));

    let decision = f.recycle(
        "wave-1",
        &Fixture::workspace(&link, WaveWorkspaceKind::Managed),
    );
    assert!(
        matches!(decision.refusal(), Some(RecycleRefusal::OutsideRoot { .. })),
        "a lexical prefix check would have let this through: {decision:?}"
    );
    assert_eq!(
        std::fs::read(outside.join("user-file.txt")).unwrap(),
        b"not ours"
    );
}

#[test]
fn a_symlinked_parent_that_resolves_outside_the_root_is_refused() {
    let f = Fixture::new();
    // The subtler shape of the same bug: the *cove* level is the link, so the
    // wave directory itself is a perfectly ordinary directory.
    let outside_cove = f._tmp.path().join("elsewhere-cove");
    std::fs::create_dir_all(outside_cove.join("wave-1").join(".git")).unwrap();
    std::fs::write(
        outside_cove
            .join("wave-1")
            .join(".git")
            .join("neige-workspace"),
        "wave-1\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&outside_cove, f.root.join("cove-1")).unwrap();

    let stored = f.root.join("cove-1").join("wave-1");
    let decision = f.recycle(
        "wave-1",
        &Fixture::workspace(&stored, WaveWorkspaceKind::Managed),
    );
    assert!(
        matches!(decision.refusal(), Some(RecycleRefusal::OutsideRoot { .. })),
        "{decision:?}"
    );
    assert!(outside_cove.join("wave-1").join(".git").is_dir());
}

// --------------------------------------------------------------------------
// Guard 2, depth — red team R1/R2
// --------------------------------------------------------------------------

/// A valid ownership marker on the **cove layer** must not make the cove
/// directory recyclable: renaming it takes every sibling wave's repository
/// with it. Executable, not a decision assertion — the sibling's bytes are
/// what the test reads.
#[test]
fn a_marker_on_the_cove_layer_does_not_take_the_siblings_with_it() {
    let f = Fixture::new();
    let sibling = f.managed("cove-1", "wave-2");
    std::fs::write(sibling.join("sibling-work.txt"), b"do not lose me").unwrap();

    // Someone (a restore, a future PATCH writing an arbitrary path, a bug)
    // leaves our marker one level up and points a wave row at it.
    let cove_dir = f.root.join("cove-1");
    std::fs::create_dir_all(cove_dir.join(".git")).unwrap();
    std::fs::write(cove_dir.join(".git").join("neige-workspace"), "wave-1\n").unwrap();

    let decision = f.recycle(
        "wave-1",
        &Fixture::workspace(&cove_dir, WaveWorkspaceKind::Managed),
    );
    assert!(
        matches!(decision.refusal(), Some(RecycleRefusal::WrongDepth { .. })),
        "the cove layer was accepted for recycling: {decision:?}"
    );
    assert!(cove_dir.is_dir());
    assert_eq!(
        std::fs::read(sibling.join("sibling-work.txt")).unwrap(),
        b"do not lose me",
        "a sibling wave's repository was moved into the trash"
    );
}

/// The same rule from below: a marked directory *deeper* than
/// `<root>/<cove_id>/<wave_id>` is not a workspace either.
#[test]
fn a_marker_deeper_than_a_wave_directory_is_refused() {
    let f = Fixture::new();
    let wave = f.managed("cove-1", "wave-1");
    let nested = wave.join("nested").join("deeper");
    std::fs::create_dir_all(nested.join(".git")).unwrap();
    std::fs::write(nested.join(".git").join("neige-workspace"), "wave-1\n").unwrap();

    let decision = f.recycle(
        "wave-1",
        &Fixture::workspace(&nested, WaveWorkspaceKind::Managed),
    );
    assert!(
        matches!(decision.refusal(), Some(RecycleRefusal::WrongDepth { .. })),
        "{decision:?}"
    );
    assert!(nested.is_dir());
}

/// The `.trash` exclusion branch of guard 2, which otherwise has no test:
/// recycling something already in the trash would nest trash inside trash and
/// make the entry undateable by [`gc_trash`].
#[test]
fn a_path_already_inside_the_trash_is_refused() {
    let f = Fixture::new();
    let path = f.managed("cove-1", "wave-1");
    let decision = f.recycle(
        "wave-1",
        &Fixture::workspace(&path, WaveWorkspaceKind::Managed),
    );
    let trashed = decision.trashed_path().unwrap().to_path_buf();
    // The trashed copy still carries a valid marker for this wave, so guards 1,
    // 3 and 4 all hold; only "not already in the trash" refuses.
    let again = f.recycle(
        "wave-1",
        &Fixture::workspace(&trashed, WaveWorkspaceKind::Managed),
    );
    assert!(
        matches!(again.refusal(), Some(RecycleRefusal::OutsideRoot { .. })),
        "{again:?}"
    );
    assert!(trashed.is_dir());
    assert_eq!(
        std::fs::read_dir(f.root.join(TRASH_DIR_NAME))
            .unwrap()
            .count(),
        1,
        "trash was nested inside trash"
    );
}

// --------------------------------------------------------------------------
// The destination is validated too — red team R6/R11
// --------------------------------------------------------------------------

/// `.trash` as a symlink out of the root. `create_dir_all` follows it, so the
/// workspace would land outside the managed tree and [`gc_trash`] — which
/// canonicalizes — could never see it again: a silent, permanent leak reported
/// as a successful recycle.
#[test]
fn a_symlinked_trash_directory_is_a_hard_error_not_a_silent_leak() {
    let f = Fixture::new();
    let elsewhere = f._tmp.path().join("trash-elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, f.root.join(TRASH_DIR_NAME)).unwrap();
    let path = f.managed("cove-1", "wave-1");

    let error = recycle_wave_workspace(
        &f.root,
        Some(CoveKind::User),
        "wave-1",
        &Fixture::workspace(&path, WaveWorkspaceKind::Managed),
        1_000_000,
    )
    .expect_err("a trash directory outside the root must be fatal");
    assert!(
        format!("{error}").contains("resolves outside the managed"),
        "{error}"
    );
    assert!(path.join(".git").is_dir(), "the workspace was moved anyway");
    assert_eq!(
        std::fs::read_dir(&elsewhere).unwrap().count(),
        0,
        "the workspace was moved out of the managed root"
    );
}

/// `wave_id` is interpolated into the trash entry name. An id that is not a
/// single path segment must not be able to steer the `rename` above the root.
/// Closed by coincidence today (ids are uuid-simple); this pins it.
#[test]
fn a_wave_id_that_escapes_its_path_segment_is_a_hard_error() {
    let f = Fixture::new();
    let escaping_id = "../escaped";
    let path = f.managed("cove-1", "wave-1");
    // Marker must match the (hostile) id, so guard 3 holds and the destination
    // check is the only thing left.
    std::fs::write(
        path.join(".git").join("neige-workspace"),
        format!("{escaping_id}\n"),
    )
    .unwrap();

    let error = recycle_wave_workspace(
        &f.root,
        Some(CoveKind::User),
        escaping_id,
        &Fixture::workspace(&path, WaveWorkspaceKind::Managed),
        1_000_000,
    )
    .expect_err("a wave id containing `..` must be fatal");
    assert!(
        format!("{error}").contains("does not form a single path segment"),
        "{error}"
    );
    assert!(path.join(".git").is_dir());
    // And nothing landed above the root.
    let stray: Vec<_> = std::fs::read_dir(f._tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n.to_string_lossy().contains("escaped"))
        .collect();
    assert!(
        stray.is_empty(),
        "renamed above the managed root: {stray:?}"
    );
}

/// Red team R22 — the static `.trash` check cannot cover the window between
/// itself and the `rename`. Swap `.trash` for a symlink in that window and the
/// kernel re-resolves the candidate at rename time: the workspace lands outside
/// the managed root while the function reports
/// `Trashed { to: <root>/.trash/… }`.
///
/// The red team hit this by racing (2 of 200 attempts). Reproduced here by
/// deterministic injection instead — a threaded hammer would make this test
/// flaky in exactly the way a regression test must not be.
///
/// The fix is detection, not prevention (prevention is `renameat`, registered
/// as N16). What must never happen is the *lie*: reporting success while the
/// bytes are somewhere the GC can never see.
#[test]
fn a_trash_swapped_between_canonicalize_and_rename_is_not_reported_as_success() {
    let f = Fixture::new();
    let path = f.managed("cove-1", "wave-1");
    std::fs::write(path.join("work.txt"), b"precious").unwrap();
    let elsewhere = f._tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();

    let swap_target = elsewhere.clone();
    let result = with_pre_rename_hook(
        move |trash_root| {
            // Only fire once: after the swap `trash_root` is a symlink, and
            // `remove_dir` on it would fail.
            if trash_root.is_symlink() {
                return;
            }
            std::fs::remove_dir(trash_root).unwrap();
            std::os::unix::fs::symlink(&swap_target, trash_root).unwrap();
        },
        || {
            recycle_wave_workspace(
                &f.root,
                Some(CoveKind::User),
                "wave-1",
                &Fixture::workspace(&path, WaveWorkspaceKind::Managed),
                1_000_000,
            )
        },
    );

    let error = result.expect_err(
        "the workspace landed outside the managed root and was reported as a \
         successful recycle",
    );
    let message = format!("{error}");
    assert!(
        message.contains("is not the trash directory"),
        "unexpected error: {message}"
    );
    // Best-effort restore: same filesystem here, so it must have worked, and the
    // error must say so.
    assert!(
        message.contains("has been moved back"),
        "the error must state which of the two recovery states applies: {message}"
    );
    assert_eq!(
        std::fs::read(path.join("work.txt")).unwrap(),
        b"precious",
        "the workspace was not restored to its original path"
    );
    assert_eq!(
        std::fs::read_dir(&elsewhere).unwrap().count(),
        0,
        "the workspace is still outside the managed root"
    );
}

#[test]
fn the_root_itself_is_never_recycled() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.root.join(".git")).unwrap();
    std::fs::write(f.root.join(".git").join("neige-workspace"), "wave-1\n").unwrap();
    let decision = f.recycle(
        "wave-1",
        &Fixture::workspace(&f.root, WaveWorkspaceKind::Managed),
    );
    assert!(
        matches!(decision.refusal(), Some(RecycleRefusal::OutsideRoot { .. })),
        "{decision:?}"
    );
    assert!(f.root.is_dir());
}

#[test]
fn a_missing_directory_is_a_no_op_not_an_error() {
    let f = Fixture::new();
    let path = f.root.join("cove-1").join("wave-1");
    let decision = f.recycle(
        "wave-1",
        &Fixture::workspace(&path, WaveWorkspaceKind::Managed),
    );
    assert_eq!(decision.refusal(), Some(&RecycleRefusal::PathMissing));
}

#[test]
fn recycling_the_same_wave_twice_does_not_clobber_the_first_entry() {
    let f = Fixture::new();
    let first_path = f.managed("cove-1", "wave-1");
    std::fs::write(first_path.join("gen.txt"), b"one").unwrap();
    let first = f.recycle(
        "wave-1",
        &Fixture::workspace(&first_path, WaveWorkspaceKind::Managed),
    );
    let second_path = f.managed("cove-1", "wave-1");
    std::fs::write(second_path.join("gen.txt"), b"two").unwrap();
    let second = f.recycle(
        "wave-1",
        &Fixture::workspace(&second_path, WaveWorkspaceKind::Managed),
    );

    let a = first.trashed_path().unwrap();
    let b = second.trashed_path().unwrap();
    assert_ne!(a, b, "the same millisecond must not reuse a trash slot");
    assert_eq!(std::fs::read(a.join("gen.txt")).unwrap(), b"one");
    assert_eq!(std::fs::read(b.join("gen.txt")).unwrap(), b"two");
    // Both names still parse, which is what keeps the GC able to date them.
    assert!(trash_entry_timestamp(a).is_some());
    assert!(trash_entry_timestamp(b).is_some());
}

#[test]
fn the_cove_directory_is_removed_only_once_it_is_empty() {
    let f = Fixture::new();
    let one = f.managed("cove-1", "wave-1");
    let two = f.managed("cove-1", "wave-2");
    let ws_one = Fixture::workspace(&one, WaveWorkspaceKind::Managed);
    let ws_two = Fixture::workspace(&two, WaveWorkspaceKind::Managed);

    // Only one of the two recycled: the cove layer must survive.
    let partial = recycle_cove_workspaces(
        &f.root,
        "cove-1",
        Some(CoveKind::User),
        &[RecycleTarget {
            wave_id: "wave-1",
            workspace: &ws_one,
        }],
        1_000_000,
    )
    .unwrap();
    assert!(!partial.cove_dir_removed);
    assert!(f.root.join("cove-1").is_dir());
    assert!(two.join(".git").is_dir());

    let full = recycle_cove_workspaces(
        &f.root,
        "cove-1",
        Some(CoveKind::User),
        &[RecycleTarget {
            wave_id: "wave-2",
            workspace: &ws_two,
        }],
        1_000_001,
    )
    .unwrap();
    assert!(full.cove_dir_removed);
    assert!(!f.root.join("cove-1").exists());
}

#[test]
fn a_cove_directory_holding_an_unrecycled_wave_is_kept() {
    let f = Fixture::new();
    let attached_shaped = f.managed("cove-1", "wave-1");
    let ws = Fixture::workspace(&attached_shaped, WaveWorkspaceKind::Attached);
    let report = recycle_cove_workspaces(
        &f.root,
        "cove-1",
        Some(CoveKind::User),
        &[RecycleTarget {
            wave_id: "wave-1",
            workspace: &ws,
        }],
        1_000_000,
    )
    .unwrap();
    assert!(!report.cove_dir_removed);
    assert!(attached_shaped.join(".git").is_dir());
}

// --------------------------------------------------------------------------
// Trash GC
// --------------------------------------------------------------------------

fn seed_trash_entry(root: &Path, name: &str) -> PathBuf {
    let path = root.join(TRASH_DIR_NAME).join(name);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("payload"), b"x").unwrap();
    path
}

#[test]
fn gc_removes_entries_past_the_retention_window_and_keeps_the_rest() {
    let f = Fixture::new();
    let now = 100 * DAY_MS;
    let old = seed_trash_entry(&f.root, &format!("wave-old-{}", now - 8 * DAY_MS));
    let fresh = seed_trash_entry(&f.root, &format!("wave-fresh-{}", now - 6 * DAY_MS));
    // Exactly at the boundary: retention is "younger than", so this goes.
    let boundary = seed_trash_entry(&f.root, &format!("wave-edge-{}", now - TRASH_RETENTION_MS));

    let removed = gc_trash(&f.root, now, TRASH_RETENTION_MS).unwrap();

    assert!(!old.exists());
    assert!(!boundary.exists());
    assert!(fresh.exists(), "a six-day-old entry is still recoverable");
    assert_eq!(removed.len(), 2, "removed={removed:?}");
}

#[test]
fn gc_dates_entries_by_name_not_by_mtime() {
    // The mtime of a renamed directory is whenever its contents last changed,
    // which for a workspace is typically long before it was trashed. An
    // mtime-based sweep would therefore delete a just-recycled workspace
    // immediately — no retention window at all, for exactly the repositories
    // most worth keeping.
    let f = Fixture::new();
    let now = 100 * DAY_MS;
    let entry = seed_trash_entry(&f.root, &format!("wave-1-{now}"));
    // 1970-01-01 + 1000s, i.e. as stale as an mtime gets.
    let status = std::process::Command::new("touch")
        .args(["-d", "@1000"])
        .arg(&entry)
        .status()
        .unwrap();
    assert!(status.success());
    let mtime = std::fs::metadata(&entry).unwrap().modified().unwrap();
    assert!(
        mtime < std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(10_000),
        "the fixture must actually have an ancient mtime, or this proves nothing"
    );

    let removed = gc_trash(&f.root, now, TRASH_RETENTION_MS).unwrap();
    assert!(removed.is_empty(), "removed={removed:?}");
    assert!(entry.exists());
}

#[test]
fn gc_keeps_anything_it_cannot_date() {
    let f = Fixture::new();
    let now = 100 * DAY_MS;
    let undateable = seed_trash_entry(&f.root, "no-timestamp-here");
    let not_a_number = seed_trash_entry(&f.root, "wave-1-notanumber");
    let removed = gc_trash(&f.root, now, TRASH_RETENTION_MS).unwrap();
    assert!(removed.is_empty(), "removed={removed:?}");
    assert!(undateable.exists());
    assert!(not_a_number.exists());
}

#[test]
fn gc_does_not_follow_a_symlink_planted_in_the_trash() {
    let f = Fixture::new();
    let now = 100 * DAY_MS;
    let victim = f._tmp.path().join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("keep.txt"), b"keep").unwrap();
    std::fs::create_dir_all(f.root.join(TRASH_DIR_NAME)).unwrap();
    let link = f
        .root
        .join(TRASH_DIR_NAME)
        .join(format!("wave-1-{}", now - 30 * DAY_MS));
    std::os::unix::fs::symlink(&victim, &link).unwrap();

    let removed = gc_trash(&f.root, now, TRASH_RETENTION_MS).unwrap();
    assert!(removed.is_empty(), "removed={removed:?}");
    assert_eq!(std::fs::read(victim.join("keep.txt")).unwrap(), b"keep");
}

#[test]
fn gc_on_a_root_without_a_trash_dir_is_a_no_op() {
    let f = Fixture::new();
    assert!(
        gc_trash(&f.root, 100 * DAY_MS, TRASH_RETENTION_MS)
            .unwrap()
            .is_empty()
    );
    assert!(
        gc_trash(
            &f.root.join("never-created"),
            100 * DAY_MS,
            TRASH_RETENTION_MS
        )
        .unwrap()
        .is_empty()
    );
}
