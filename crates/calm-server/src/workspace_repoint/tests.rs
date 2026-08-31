//! #1147 S3 — the three clauses of the "is anything on disk" predicate.
//!
//! Every fixture is built by running the **production materializer** and then
//! performing one real action a real writer performs. Nothing here
//! re-implements the predicate or hand-crafts a `.git` directory: the point of
//! the design's D4 is that it does not have to know who wrote, so a fixture
//! that simulates the *verdict* rather than the *cause* would prove nothing.
//!
//! Each clause has a fixture that only it rejects — see
//! `each_clause_has_a_fixture_only_it_rejects`, which asserts that
//! mechanically rather than leaving it to a reviewer's reading.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::workspace_materialize::materialize_managed_workspace;

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    wave_id: String,
}

/// A freshly materialized managed workspace at `<root>/<cove>/<wave>` — the
/// exact shape `POST /api/waves` produces.
fn materialized() -> Fixture {
    let root = tempfile::TempDir::new().unwrap();
    let wave_id = format!("w{}", uuid::Uuid::new_v4().simple());
    let workspace = root.path().join("cove-1").join(&wave_id);
    materialize_managed_workspace(root.path(), &workspace, &wave_id).unwrap();
    Fixture {
        root,
        workspace,
        wave_id,
    }
}

fn git(at: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(at)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
    assert!(
        output.status.success(),
        "git {args:?} in {at:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The identity every fixture commit needs; the materializer only sets it for
/// its own one-shot commit, so a repository it produced has no `user.name`.
fn with_identity(at: &Path) {
    git(at, &["config", "user.name", "fixture"]);
    git(at, &["config", "user.email", "fixture@example.com"]);
}

#[test]
fn a_freshly_materialized_workspace_is_pristine() {
    let fx = materialized();
    assert_eq!(
        workspace_pristine(&fx.workspace),
        PristineVerdict::Pristine,
        "the shape `POST /api/waves` produces must be re-pointable from second \
         zero; if it is not, the workspace is a default nobody can ever change"
    );
    drop(fx.root);
}

#[test]
fn a_plain_untracked_file_is_dirty() {
    let fx = materialized();
    std::fs::write(fx.workspace.join("notes.md"), b"the agent wrote this\n").unwrap();
    let verdict = workspace_pristine(&fx.workspace);
    assert!(
        matches!(
            &verdict,
            PristineVerdict::Dirty { check, .. } if *check == "git status --porcelain --ignored"
        ),
        "expected the status clause to reject, got {verdict:?}"
    );
    drop(fx.root);
}

/// The clause that only `--ignored` catches.
///
/// `materialize_managed_workspace` writes `.claude/worktrees/` into
/// `.git/info/exclude` (S2, so that worker output does not permanently dirty
/// the tree). That exclusion is exactly what would hide a workspace full of
/// worker output from a plain `git status --porcelain`. Dropping `--ignored`
/// from the predicate turns this test — and only this test — green-to-red.
#[test]
fn excluded_worker_output_is_dirty() {
    let fx = materialized();
    let lease = fx
        .workspace
        .join(".claude")
        .join("worktrees")
        .join(&fx.wave_id)
        .join("card-1");
    std::fs::create_dir_all(&lease).unwrap();
    std::fs::write(lease.join("out.txt"), b"worker output\n").unwrap();

    // The premise: a plain `status --porcelain` really is blind to this.
    let plain = Command::new("git")
        .arg("-C")
        .arg(&fx.workspace)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&plain.stdout).trim().is_empty(),
        "premise broken: `git status --porcelain` already sees the excluded \
         worker output, so `--ignored` would not be load bearing"
    );

    let verdict = workspace_pristine(&fx.workspace);
    assert!(
        matches!(
            &verdict,
            PristineVerdict::Dirty { check, .. } if *check == "git status --porcelain --ignored"
        ),
        "expected the --ignored status clause to reject, got {verdict:?}"
    );
    drop(fx.root);
}

/// An empty `.claude/worktrees/` must NOT be dirty.
///
/// The design says so explicitly, and it matters: `create_workspace_lease_directory`
/// makes the parent directory, and a lease that has been released leaves the
/// tree behind. If an empty directory counted, a workspace would become
/// permanently un-re-pointable for no reason.
#[test]
fn an_empty_worktrees_directory_is_still_pristine() {
    let fx = materialized();
    std::fs::create_dir_all(fx.workspace.join(".claude").join("worktrees")).unwrap();
    assert_eq!(workspace_pristine(&fx.workspace), PristineVerdict::Pristine);
    drop(fx.root);
}

/// A commit that leaves no working-tree trace at all.
#[test]
fn a_commit_on_another_branch_is_dirty() {
    let fx = materialized();
    with_identity(&fx.workspace);
    git(&fx.workspace, &["checkout", "-q", "-b", "neige/slice"]);
    std::fs::write(fx.workspace.join("work.txt"), b"worker work\n").unwrap();
    git(&fx.workspace, &["add", "-A"]);
    git(
        &fx.workspace,
        &["commit", "-q", "--no-verify", "-m", "worker commit"],
    );
    git(&fx.workspace, &["checkout", "-q", "main"]);

    // Working tree is clean again — only the commit count betrays the work.
    let status = Command::new("git")
        .arg("-C")
        .arg(&fx.workspace)
        .args(["status", "--porcelain", "--ignored"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "premise broken: the status clause already rejects this, so the \
         rev-list clause would not be load bearing"
    );

    let verdict = workspace_pristine(&fx.workspace);
    assert!(
        matches!(
            &verdict,
            PristineVerdict::Dirty { check, .. } if *check == "git rev-list --count --all"
        ),
        "expected the rev-list clause to reject, got {verdict:?}"
    );
    drop(fx.root);
}

/// `git stash` is the same class as the branch commit above and is the reason
/// the clause says `--all` rather than `HEAD`.
#[test]
fn a_stash_is_dirty() {
    let fx = materialized();
    with_identity(&fx.workspace);
    std::fs::write(fx.workspace.join("wip.txt"), b"work in progress\n").unwrap();
    git(&fx.workspace, &["add", "-A"]);
    git(&fx.workspace, &["stash", "-q"]);

    let status = Command::new("git")
        .arg("-C")
        .arg(&fx.workspace)
        .args(["status", "--porcelain", "--ignored"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "premise broken: the status clause already rejects the stash fixture"
    );

    let verdict = workspace_pristine(&fx.workspace);
    assert!(
        matches!(
            &verdict,
            PristineVerdict::Dirty { check, .. } if *check == "git rev-list --count --all"
        ),
        "expected the rev-list clause to reject the stash, got {verdict:?}"
    );
    drop(fx.root);
}

/// A live worktree: files live elsewhere, the repository here is clean, and a
/// `rename` would dangle both of the absolute pointers that bind them.
#[test]
fn a_live_worktree_is_dirty() {
    let fx = materialized();
    let elsewhere = fx.root.path().join("lease-worktree");
    git(
        &fx.workspace,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "neige/lease",
            elsewhere.to_str().unwrap(),
        ],
    );

    let verdict = workspace_pristine(&fx.workspace);
    assert!(
        matches!(
            &verdict,
            PristineVerdict::Dirty { check, .. } if *check == "git worktree list"
        ),
        "expected the worktree clause to reject, got {verdict:?}"
    );
    drop(fx.root);
}

/// Fail-closed: a path that is not a repository at all cannot be proven empty,
/// so it is dirty. "Cannot tell" is never "clean" — the consequence of a wrong
/// `Pristine` is renaming a directory with work in it into the trash.
#[test]
fn a_path_that_is_not_a_repository_is_dirty() {
    let tmp = tempfile::TempDir::new().unwrap();
    let verdict = workspace_pristine(tmp.path());
    assert!(
        matches!(&verdict, PristineVerdict::Dirty { .. }),
        "expected fail-closed on a non-repository, got {verdict:?}"
    );
}

#[test]
fn a_missing_path_is_dirty() {
    let tmp = tempfile::TempDir::new().unwrap();
    let verdict = workspace_pristine(&tmp.path().join("does-not-exist"));
    assert!(
        matches!(&verdict, PristineVerdict::Dirty { .. }),
        "expected fail-closed on a missing path, got {verdict:?}"
    );
}

/// The meta-test the design asks for: every clause is load bearing, stated as
/// an assertion rather than as three separate tests a reader has to correlate.
///
/// For each clause, a fixture exists that **only that clause** rejects — i.e.
/// running the predicate with that one clause removed would accept it. This is
/// checked by running each clause's own command against each fixture.
#[test]
fn each_clause_has_a_fixture_only_it_rejects() {
    fn clause_verdicts(path: &Path) -> [bool; 3] {
        let status = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["status", "--porcelain", "--ignored"])
            .output()
            .unwrap();
        let commits = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["rev-list", "--count", "--all"])
            .output()
            .unwrap();
        let worktrees = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["worktree", "list"])
            .output()
            .unwrap();
        [
            !String::from_utf8_lossy(&status.stdout).trim().is_empty(),
            String::from_utf8_lossy(&commits.stdout).trim() != "1",
            String::from_utf8_lossy(&worktrees.stdout)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count()
                != 1,
        ]
    }

    // Fixture 1 — excluded worker output: only the status clause (thanks to
    // `--ignored`) rejects it.
    let a = materialized();
    let lease = a.workspace.join(".claude").join("worktrees").join("c");
    std::fs::create_dir_all(&lease).unwrap();
    std::fs::write(lease.join("out.txt"), b"x\n").unwrap();
    assert_eq!(
        clause_verdicts(&a.workspace),
        [true, false, false],
        "excluded worker output must be rejected by the status clause alone"
    );

    // Fixture 2 — a stash: only the rev-list clause rejects it.
    let b = materialized();
    with_identity(&b.workspace);
    std::fs::write(b.workspace.join("wip.txt"), b"x\n").unwrap();
    git(&b.workspace, &["add", "-A"]);
    git(&b.workspace, &["stash", "-q"]);
    assert_eq!(
        clause_verdicts(&b.workspace),
        [false, true, false],
        "a stash must be rejected by the rev-list clause alone"
    );

    // Fixture 3 — a live worktree: only the worktree clause rejects it.
    let c = materialized();
    let elsewhere = c.root.path().join("wt");
    git(
        &c.workspace,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "neige/x",
            elsewhere.to_str().unwrap(),
        ],
    );
    assert_eq!(
        clause_verdicts(&c.workspace),
        [false, false, true],
        "a live worktree must be rejected by the worktree clause alone"
    );

    drop((a.root, b.root, c.root));
}
