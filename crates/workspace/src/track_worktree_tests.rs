use crate::error::Error;
use crate::git::*;
use crate::teardown::remove_workspace_worktree;
use std::path::{Path, PathBuf};
use std::process::Command;
fn add_track_worktree(repo: &Path, track_id: &str) -> WorkspaceLeaseTarget {
    let path = crate::worktree::track_worktree_path_for(repo, track_id);
    let target = crate::worktree::track_worktree_target(track_id, path.to_str().unwrap()).unwrap();
    crate::worktree::ensure_track_worktree(
        &target,
        |root, _| crate::base::resolve_head_base(root),
        |_, _| {},
    )
    .unwrap();
    target
}
/// The parent variable the hook-environment tests plant; it must never reach repository code.
pub(super) const ENV_SENTINEL: &str = "NEIGE_LEASE_ENV_SENTINEL";

/// Sets a variable of this test process and restores the previous value on drop. Correct under
/// `cargo nextest`'s process-per-test model, the documented runner (CI and the local gate). Under a
/// shared-process runner it is not: two concurrent tests setting the same variable can see each
/// other's value and restore the wrong one.
pub(super) struct EnvVar(&'static str, Option<std::ffi::OsString>);

impl EnvVar {
    pub(super) fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: one test, one process under nextest; no other thread of the test reads the
        // environment while it is written.
        unsafe { std::env::set_var(key, value) };
        Self(key, previous)
    }
}

impl Drop for EnvVar {
    fn drop(&mut self) {
        // SAFETY: see `set`.
        unsafe {
            match self.1.take() {
                Some(previous) => std::env::set_var(self.0, previous),
                None => std::env::remove_var(self.0),
            }
        }
    }
}

/// A `reference-transaction` hook in `repo` that appends its environment to `<dir>/delete.env`
/// for each ref deletion and `<dir>/update.env` for each other ref update; returns `dir`.
pub(super) fn install_ref_hook_env_probe(repo: &Path, dir: &Path) -> std::path::PathBuf {
    let hook = repo.join(".git/hooks/reference-transaction");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    let zero = "0".repeat(40);
    let body = format!(
        "#!/bin/sh\nwhile read -r old new ref; do\n  kind=update\n  [ \"$new\" = {zero} ] && kind=delete\n  env >> '{}'/\"$kind.env\"\ndone\n",
        dir.display()
    );
    std::fs::write(&hook, body).unwrap();
    let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&hook, permissions).unwrap();
    dir.to_path_buf()
}

/// The probe at `path` was written, and by a process that saw `PATH` but not [`ENV_SENTINEL`].
pub(super) fn assert_env_allowlisted(path: &Path) {
    let seen = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("the hook wrote {}: {error}", path.display()));
    assert!(seen.contains("PATH="), "{seen}");
    assert!(!seen.contains(ENV_SENTINEL), "leaked: {seen}");
}

fn init_git_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    run_git(path, ["init"]);
    run_git(path, ["config", "user.email", "lease@example.test"]);
    run_git(path, ["config", "user.name", "Lease Test"]);
    std::fs::write(path.join("README.md"), "initial\n").unwrap();
    run_git(path, ["add", "README.md"]);
    run_git(path, ["commit", "-m", "initial"]);
}

fn run_git<const N: usize>(repo: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
        args,
        repo.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout<const N: usize>(repo: &Path, args: [&str; N]) -> String {
    String::from_utf8_lossy(&git_stdout_bytes(repo, args)).to_string()
}

fn git_stdout_bytes<const N: usize>(repo: &Path, args: [&str; N]) -> Vec<u8> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
        args,
        repo.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn remove_workspace_dir_if_exists_treats_missing_as_success() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("already-gone");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::remove_dir_all(&path).unwrap();

    remove_workspace_dir_if_exists(path.to_str().unwrap()).unwrap();
}
#[test]
fn workspace_worktree_remove_deletes_branch_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    init_git_repo(tmp.path());
    let target = add_track_worktree(tmp.path(), "a");
    assert!(target.path.is_dir(), "the worktree exists");

    assert!(remove_workspace_worktree(&target, |_, _| {}).unwrap());
    assert!(!target.path.exists(), "worktree path removed");
    assert!(
        !git_ref_exists(&target.repo_root, &format!("refs/heads/{}", target.branch)).unwrap(),
        "branch removed"
    );

    assert!(!remove_workspace_worktree(&target, |_, _| {}).unwrap());
}
/// #1792: the branch delete runs the repository's `reference-transaction` hook, so it runs
/// with the allowlisted environment. nextest gives this test its own process.
#[test]
fn workspace_worktree_remove_hook_sees_only_the_allowlisted_environment() {
    let _sentinel = EnvVar::set(ENV_SENTINEL, "server-secret");
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    let target = add_track_worktree(&repo, "hook");
    let probes = install_ref_hook_env_probe(&repo, tmp.path());

    remove_workspace_worktree(&target, |_, _| {}).unwrap();

    assert_env_allowlisted(&probes.join("delete.env"));
}
/// The `.claude/worktrees/` exclude lands in the common dir's `info/exclude` for a linked
/// checkout too, exactly once.
#[test]
fn worktree_root_exclude_resolves_for_a_linked_checkout() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = tmp.path().join("primary");
    init_git_repo(&primary);
    let linked = tmp.path().join("linked-track");
    run_git(
        &primary,
        [
            "worktree",
            "add",
            "-b",
            "linked-track",
            linked.to_str().unwrap(),
        ],
    );
    assert!(
        linked.join(".git").is_file(),
        "linked worktree .git is a gitdir file"
    );

    ensure_workspace_worktree_root_excluded(&linked).unwrap();
    ensure_workspace_worktree_root_excluded(&linked).unwrap();

    let exclude_path = git_exclude_path(&linked).unwrap();
    assert_eq!(
        exclude_path.canonicalize().unwrap(),
        primary.join(".git/info/exclude").canonicalize().unwrap()
    );
    let exclude = std::fs::read_to_string(&exclude_path).unwrap();
    assert_eq!(
        exclude
            .lines()
            .filter(|line| line.trim() == ".claude/worktrees/")
            .count(),
        1,
        "the exclude entry is written once"
    );
}
/// G15 — a symlink leaf is unlinked, never followed: whatever it points at (an external
/// directory, registered or not) keeps every entry and registration it had.
#[test]
fn removal_of_symlink_leaf_unlinks_only() {
    for registered in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_git_repo(&repo);
        let external = tmp.path().join("external");
        let target = WorkspaceLeaseTarget {
            repo_root: repo.clone(),
            path: crate::worktree::track_worktree_path_for(&repo, "rm"),
            branch: crate::worktree::track_branch_for("rm").unwrap(),
        };
        if registered {
            run_git(
                &repo,
                [
                    "worktree",
                    "add",
                    "-b",
                    &target.branch,
                    external.to_str().unwrap(),
                    "HEAD",
                ],
            );
        } else {
            std::fs::create_dir_all(&external).unwrap();
        }
        let sentinel = external.join("SENTINEL.txt");
        std::fs::write(&sentinel, "external data\n").unwrap();
        let entries_before = std::fs::read_dir(&external).unwrap().count();
        std::fs::create_dir_all(target.path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&external, &target.path).unwrap();
        let listing_before = git_stdout(&repo, ["worktree", "list", "--porcelain"]);

        let result = remove_workspace_worktree(&target, |_, _| {});

        assert!(
            std::fs::symlink_metadata(&target.path).is_err(),
            "registered={registered}: the link is gone"
        );
        assert_eq!(
            std::fs::read_to_string(&sentinel).unwrap(),
            "external data\n",
            "registered={registered}: the sentinel is untouched"
        );
        assert_eq!(
            std::fs::read_dir(&external).unwrap().count(),
            entries_before,
            "registered={registered}: nothing added or removed in the external directory"
        );
        assert_eq!(
            git_stdout(&repo, ["worktree", "list", "--porcelain"]),
            listing_before,
            "registered={registered}: registrations are untouched"
        );
        let branch_ref = format!("refs/heads/{}", target.branch);
        if registered {
            let err = result.expect_err("the branch is checked out in the external worktree");
            assert!(err.to_string().contains("git branch -D"), "{err}");
            assert!(git_ref_exists(&repo, &branch_ref).unwrap());
        } else {
            assert!(
                result.unwrap(),
                "registered=false: the link counted as removed"
            );
            assert!(!git_ref_exists(&repo, &branch_ref).unwrap());
        }
    }
}
/// G15 — someone else's registration at the target's realpath (track A's worktree moved to track
/// B's path, a symlink left at A's) is refused rather than `worktree remove --force`d through the
/// alias; B's directory and its uncommitted file are exactly as they were.
#[test]
fn foreign_registration_at_the_target_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    let a = add_track_worktree(&repo, "al-a");
    let b_path = crate::worktree::track_worktree_path_for(&repo, "al-b");
    std::fs::rename(&a.path, &b_path).unwrap();
    std::os::unix::fs::symlink(&b_path, &a.path).unwrap();
    std::fs::write(b_path.join("UNCOMMITTED.txt"), "A's uncommitted work\n").unwrap();
    let entries_before = std::fs::read_dir(&b_path).unwrap().count();
    let b = WorkspaceLeaseTarget {
        repo_root: repo.clone(),
        path: b_path.clone(),
        branch: crate::worktree::track_branch_for("al-b").unwrap(),
    };
    assert!(matches!(
        git_worktree_registration(&b).unwrap(),
        GitWorktreeRegistration::Foreign { .. }
    ));

    let err = remove_workspace_worktree(&b, |_, _| {}).unwrap_err();

    assert!(
        err.to_string()
            .contains("resolves to a worktree registered as"),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(&b_path).unwrap().count(), entries_before);
    assert!(b_path.join("UNCOMMITTED.txt").is_file());
}
/// A user worktree at a path that is not UTF-8 is someone else's record: the registration parser
/// compares bytes, so the target reads absent, then present once added, and is removed, all `Ok`,
/// with the foreign worktree untouched.
#[test]
fn foreign_non_utf8_worktree_record_does_not_fail_the_target() {
    use std::os::unix::ffi::OsStringExt;
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_git_repo(&repo);
    let mut foreign = tmp.path().as_os_str().to_os_string().into_vec();
    foreign.extend_from_slice(b"/wt-\xff");
    let foreign = PathBuf::from(std::ffi::OsString::from_vec(foreign));
    let output = Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(&foreign)
        .arg("HEAD")
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let target = WorkspaceLeaseTarget {
        repo_root: repo.clone(),
        path: crate::worktree::track_worktree_path_for(&repo, "u8"),
        branch: crate::worktree::track_branch_for("u8").unwrap(),
    };

    assert_eq!(
        git_worktree_registration(&target).unwrap(),
        GitWorktreeRegistration::Absent
    );
    add_track_worktree(&repo, "u8");
    assert_eq!(
        git_worktree_registration(&target).unwrap(),
        GitWorktreeRegistration::Present
    );
    assert!(remove_workspace_worktree(&target, |_, _| {}).unwrap());
    assert!(!target.path.exists());
    assert!(
        foreign.join("README.md").is_file(),
        "the foreign worktree is untouched"
    );
}
/// The repository's common dir path holds a byte that is
/// not UTF-8 (`wt-\xff`), and a sibling directory carrying the U+FFFD that a
/// lossy decode would turn it into exists at the same relative path. The
/// path reader keeps git's bytes, so `canonicalize` resolves the real common
/// dir and the UTF-8 rule then refuses it — the lease never records the
/// decoy directory.
#[test]
fn common_dir_bytes_are_not_decoded_before_canonicalize() {
    use std::os::unix::ffi::OsStringExt;
    let tmp = tempfile::tempdir().unwrap();
    let mut raw = tmp.path().as_os_str().to_os_string().into_vec();
    raw.extend_from_slice(b"/wt-\xff");
    let repo = PathBuf::from(std::ffi::OsString::from_vec(raw)).join("repo");
    init_git_repo(&repo);
    let decoy = tmp.path().join("wt-\u{FFFD}").join("repo").join(".git");
    std::fs::create_dir_all(&decoy).unwrap();
    assert_eq!(
        PathBuf::from(repo.join(".git").to_string_lossy().into_owned()),
        decoy,
        "test setup: a lossy decode of the common dir names the decoy"
    );
    assert!(decoy.is_dir(), "test setup: the decoy resolves");

    let err = crate::base::lease_git_common_dir(&repo).unwrap_err();

    assert!(matches!(err, Error::Internal(_)), "{err}");
    assert!(
        err.to_string().contains("not UTF-8"),
        "the real common dir was resolved and refused for what it is: {err}"
    );
    assert_eq!(
        std::fs::read_dir(&decoy).unwrap().count(),
        0,
        "the decoy was never used"
    );
}
