//! In-crate because these drive the `pub(crate)` worker checkout reader against a real materialized
//! workspace: since #1830 S2 a managed track's worker runs in the directory itself, on `main`.

use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

use super::{InitCommit, materialize_managed_workspace, materialize_managed_workspace_inner};
use crate::base::{resolve_head_base, verify_worktree_base};

/// The git environment is process-global, so every git-spawning test holds this lock for its whole body
/// (a helper-scoped guard was flaky under plain `cargo test`; nextest's per-process isolation would hide it).
static GIT_ENV_LOCK: Mutex<()> = Mutex::new(());

struct GitEnv {
    _guard: MutexGuard<'static, ()>,
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl GitEnv {
    fn set(vars: &[(&'static str, &std::ffi::OsStr)]) -> Self {
        let guard = GIT_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut previous = Vec::new();
        for (key, value) in vars {
            previous.push((*key, std::env::var_os(key)));
            // SAFETY: `GIT_ENV_LOCK` is held for as long as the returned guard
            // lives, and the guard is bound in the *test body* (never inside a
            // helper that returns before the git spawns). Every git-spawning
            // test in this module holds one, so there is no concurrent reader
            // or writer of these variables.
            unsafe { std::env::set_var(key, value) };
        }
        Self {
            _guard: guard,
            previous,
        }
    }

    /// git localizes its diagnostics; the tests assert on the English text.
    fn c_locale() -> Self {
        Self::set(&[
            ("LC_ALL", std::ffi::OsStr::new("C")),
            ("LANGUAGE", std::ffi::OsStr::new("")),
        ])
    }

    fn c_locale_with_global_config(path: &Path) -> Self {
        Self::set(&[
            ("LC_ALL", std::ffi::OsStr::new("C")),
            ("LANGUAGE", std::ffi::OsStr::new("")),
            ("GIT_CONFIG_GLOBAL", path.as_os_str()),
        ])
    }
}

impl Drop for GitEnv {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            // SAFETY: see `set`.
            unsafe {
                match value {
                    Some(previous) => std::env::set_var(key, previous),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

const TRACK: &str = "track0000000000000000000000000001";

fn sandbox(tmp: &tempfile::TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let repo_root = super::managed_workspace_path(&root, "area0000000000000000000000000001", TRACK);
    (root, repo_root)
}

fn materialize(root: &Path, repo_root: &Path) -> crate::error::Result<()> {
    materialize_managed_workspace(root, repo_root, TRACK)
}

fn head_resolves(path: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn count_all_commits(path: &Path) -> u32 {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-list", "--count", "--all"])
        .output()
        .unwrap();
    assert!(out.status.success(), "git rev-list failed in {path:?}");
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

/// What a worker's prepare and spawn need of a managed directory (#1830 S2): a HEAD to record, on
/// `main`, and a clean tree (a dirty one refuses the worker).
fn assert_worker_checkout_ready(repo_root: &Path) {
    let head = resolve_head_base(repo_root).expect("a committed HEAD");
    crate::base::lease_git_common_dir(repo_root).expect("a common dir");
    verify_worktree_base(
        repo_root,
        "main",
        &head,
        &std::fs::canonicalize(repo_root).unwrap(),
    )
    .expect("the checkout is on main at its HEAD");
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .output()
        .unwrap();
    assert!(status.status.success());
    assert_eq!(
        String::from_utf8_lossy(&status.stdout),
        "",
        "a materialized workspace is clean, so its first worker is not refused"
    );
}

#[test]
fn materialized_workspace_hosts_a_worker() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    materialize(&root, &repo_root).unwrap();

    assert!(head_resolves(&repo_root));
    assert_eq!(
        count_all_commits(&repo_root),
        1,
        "design D4 compares `rev-list --count --all` against exactly 1; the \
         empty init commit is that baseline"
    );

    assert_worker_checkout_ready(&repo_root);
}

/// Version-independent on purpose: only the missing baseline commit is asserted, not that `worktree add` fails.
#[test]
fn without_the_init_commit_there_is_no_baseline_commit() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    materialize_managed_workspace_inner(&root, &repo_root, TRACK, InitCommit::Skip).unwrap();

    assert!(
        !head_resolves(&repo_root),
        "mutation did not apply — HEAD still resolves, so the assertions below \
         would pass for the wrong reason"
    );
    assert_eq!(
        count_all_commits(&repo_root),
        0,
        "without step 3 the workspace has no commit, so design D4's \
         `rev-list --count --all == 1` baseline does not exist"
    );

    materialize(&root, &repo_root).unwrap();
    assert!(head_resolves(&repo_root));
    assert_eq!(count_all_commits(&repo_root), 1);
    assert_worker_checkout_ready(&repo_root);
}

#[test]
fn materialize_survives_a_global_gpgsign_config() {
    let tmp = tempfile::TempDir::new().unwrap();
    let gitconfig = tmp.path().join("gitconfig");
    std::fs::write(
        &gitconfig,
        "[commit]\n\tgpgsign = true\n[core]\n\thooksPath = /nonexistent/neige-hooks\n",
    )
    .unwrap();
    let _env = GitEnv::c_locale_with_global_config(&gitconfig);

    let control = tmp.path().join("control");
    std::fs::create_dir_all(&control).unwrap();
    assert!(
        Command::new("git")
            .args(["-c", "init.templateDir=", "-c", "init.defaultBranch=main"])
            .arg("init")
            .arg(&control)
            .output()
            .unwrap()
            .status
            .success()
    );
    let control_commit = Command::new("git")
        .arg("-C")
        .arg(&control)
        .args([
            "-c",
            "user.name=neige",
            "-c",
            "user.email=neige@localhost",
            "commit",
            "--allow-empty",
            "-m",
            "control",
        ])
        .output()
        .unwrap();
    assert!(
        !control_commit.status.success(),
        "injected gpgsign config did not break an un-overridden commit; \
         this test would pass without materialize overriding anything"
    );

    let (root, repo_root) = sandbox(&tmp);
    materialize(&root, &repo_root).expect("materialize under a hostile global config");
    assert!(head_resolves(&repo_root));
}

#[test]
fn materialize_refuses_a_foreign_non_empty_directory() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    std::fs::create_dir_all(&repo_root).unwrap();
    std::fs::write(repo_root.join("someones-notes.md"), "hi").unwrap();

    let error =
        materialize(&root, &repo_root).expect_err("a non-empty foreign directory must be refused");
    assert!(
        error.to_string().contains("refusing to reuse"),
        "unexpected error: {error}"
    );
}

#[test]
fn materialize_is_idempotent() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    materialize(&root, &repo_root).unwrap();
    let head = Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    std::fs::write(repo_root.join("worker-output.txt"), "produced").unwrap();

    materialize(&root, &repo_root).expect("second materialize");

    let head_again = Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert_eq!(
        head.stdout, head_again.stdout,
        "HEAD moved on re-materialize"
    );
    assert_eq!(
        std::fs::read_to_string(repo_root.join("worker-output.txt")).unwrap(),
        "produced"
    );
}

#[test]
fn materialize_excludes_neige_via_git_info_exclude_not_gitignore() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    materialize(&root, &repo_root).unwrap();

    let exclude = std::fs::read_to_string(repo_root.join(".git").join("info").join("exclude"))
        .expect("`.git/info/exclude` must exist");
    assert!(
        exclude
            .lines()
            .any(|line| line.trim() == super::NEIGE_GIT_EXCLUDE_ENTRY),
        "exclude file does not carry `.neige/`: {exclude}"
    );
    assert!(
        !repo_root.join(".gitignore").exists(),
        "materialize wrote a .gitignore; that permanently breaks design D4 (2)"
    );

    let status = Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args(["status", "--porcelain", "--ignored"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "a freshly materialized workspace must look empty to D4's predicate: {}",
        String::from_utf8_lossy(&status.stdout)
    );
}

#[test]
fn managed_path_is_root_area_track() {
    let _env = GitEnv::c_locale();
    let path = super::managed_workspace_path(Path::new("/srv/ws"), "area1", "track1");
    assert_eq!(path, Path::new("/srv/ws/area1/track1"));
}

/// Measures how many materializations of one path are inside the critical section at once: git's own
/// symptoms did not reproduce on this host with the lock removed, so the property is asserted directly.
pub(super) struct OverlapProbe(std::path::PathBuf);

type OverlapCounts = std::collections::HashMap<std::path::PathBuf, (usize, usize, usize)>;
static OVERLAP: Mutex<Option<OverlapCounts>> = Mutex::new(None);

impl OverlapProbe {
    pub(super) fn enter(path: &Path) -> Self {
        let mut guard = OVERLAP.lock().unwrap_or_else(|e| e.into_inner());
        let map = guard.get_or_insert_with(Default::default);
        let entry = map.entry(path.to_path_buf()).or_insert((0, 0, 0));
        entry.0 += 1;
        entry.1 = entry.1.max(entry.0);
        entry.2 += 1;
        OverlapProbe(path.to_path_buf())
    }

    fn peak(path: &Path) -> usize {
        let guard = OVERLAP.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .as_ref()
            .and_then(|m| m.get(path))
            .map(|(_, peak, _)| *peak)
            .unwrap_or(0)
    }

    /// Guards the peak assertion against passing because nothing ever entered.
    fn entries(path: &Path) -> usize {
        let guard = OVERLAP.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .as_ref()
            .and_then(|m| m.get(path))
            .map(|(_, _, entries)| *entries)
            .unwrap_or(0)
    }
}

impl Drop for OverlapProbe {
    fn drop(&mut self) {
        let mut guard = OVERLAP.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = guard.as_mut().and_then(|m| m.get_mut(&self.0)) {
            entry.0 -= 1;
        }
    }
}

/// A third-party git repository with real history. Caller must hold a [`GitEnv`] guard.
fn third_party_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    assert!(
        Command::new("git")
            .args(["-c", "init.templateDir=", "-c", "init.defaultBranch=main"])
            .arg("init")
            .arg(path)
            .output()
            .unwrap()
            .status
            .success()
    );
    std::fs::write(path.join("their-work.txt"), "a year of work\n").unwrap();
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["add", "."])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=Someone Else",
                "-c",
                "user.email=someone@example.com",
                "commit",
                "-m",
                "their commit",
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn a_third_party_repository_on_the_derived_path_is_refused_untouched() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    third_party_repo(&repo_root);

    let exclude = repo_root.join(".git/info/exclude");
    let exclude_before = std::fs::metadata(&exclude).map(|m| m.len()).unwrap_or(0);
    let head_before = Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap()
        .stdout;
    let log_before = Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args(["log", "--format=%an <%ae>"])
        .output()
        .unwrap()
        .stdout;

    let error =
        materialize(&root, &repo_root).expect_err("a third-party repository must never be adopted");
    assert!(
        error
            .to_string()
            .contains("carries no neige ownership marker"),
        "unexpected error: {error}"
    );

    assert_eq!(
        std::fs::metadata(&exclude).map(|m| m.len()).unwrap_or(0),
        exclude_before,
        "the server appended to a third party's .git/info/exclude"
    );
    assert_eq!(
        Command::new("git")
            .arg("-C")
            .arg(&repo_root)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
        head_before,
        "the server moved a third party's HEAD"
    );
    assert_eq!(
        Command::new("git")
            .arg("-C")
            .arg(&repo_root)
            .args(["log", "--format=%an <%ae>"])
            .output()
            .unwrap()
            .stdout,
        log_before,
        "the server added a commit to a third party's repository"
    );
    assert_eq!(
        std::fs::read_to_string(repo_root.join("their-work.txt")).unwrap(),
        "a year of work\n"
    );
    assert!(
        !repo_root.join(".git").join(super::OWNER_MARKER).exists(),
        "the server claimed ownership of a third party's repository"
    );
}

#[test]
fn the_marker_is_what_decides_adoption() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    third_party_repo(&repo_root);
    std::fs::write(
        repo_root.join(".git").join(super::OWNER_MARKER),
        format!("{TRACK}\n"),
    )
    .unwrap();

    materialize(&root, &repo_root)
        .expect("a directory carrying our marker is ours and must be accepted");
}

#[test]
fn a_marker_for_another_track_is_refused() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    std::fs::create_dir_all(repo_root.join(".git")).unwrap();
    std::fs::write(
        repo_root.join(".git").join(super::OWNER_MARKER),
        "some-other-track\n",
    )
    .unwrap();

    let error = materialize(&root, &repo_root).expect_err("foreign marker must be refused");
    assert!(
        error.to_string().contains("not `"),
        "unexpected error: {error}"
    );
}

#[test]
#[cfg(unix)]
fn a_symlink_out_of_the_root_is_refused() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let area_dir = repo_root.parent().unwrap();
    std::os::unix::fs::symlink(&elsewhere, area_dir).unwrap();

    assert!(repo_root.starts_with(&root));

    let error =
        materialize(&root, &repo_root).expect_err("a symlink out of the root must be refused");
    assert!(
        error.to_string().contains("outside the managed"),
        "unexpected error: {error}"
    );
}

#[test]
#[cfg(unix)]
fn the_same_layout_without_a_symlink_succeeds() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    std::fs::create_dir_all(repo_root.parent().unwrap()).unwrap();
    materialize(&root, &repo_root).expect("a real directory under the root is fine");
}

#[test]
fn concurrent_materialization_of_one_path_all_succeed() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);

    const THREADS: usize = 24;
    // Without the barrier the first caller finishes before the rest start and they all take the steady-state path.
    let barrier = std::sync::Barrier::new(THREADS);
    let errors: Vec<String> = std::thread::scope(|scope| {
        let barrier = &barrier;
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let root = root.clone();
                let repo_root = repo_root.clone();
                scope.spawn(move || {
                    barrier.wait();
                    materialize(&root, &repo_root).err().map(|e| e.to_string())
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().unwrap())
            .collect()
    });
    assert!(
        errors.is_empty(),
        "concurrent materialize failed: {errors:?}"
    );
    assert!(head_resolves(&repo_root));

    // The load-bearing assertion: "they all succeeded" passes with or without the mutex.
    assert_eq!(
        OverlapProbe::peak(&repo_root),
        1,
        "two materializations of one workspace overlapped: the per-path mutex \
         is not doing its job, and the interleaving that leaves a half-built \
         directory behind is reachable again"
    );
    assert_eq!(
        OverlapProbe::entries(&repo_root),
        THREADS,
        "the probe did not observe every materialization, so the peak above is \
         not evidence of anything"
    );
}

include!("claim_tests.rs");

#[cfg(unix)]
#[test]
fn observed_managed_metadata_consumption_runs_no_git() {
    use std::os::unix::fs::PermissionsExt;
    let mut env = GitEnv::c_locale();
    let tmp = tempfile::tempdir().unwrap();
    let (root, path) = sandbox(&tmp);
    materialize(&root, &path).unwrap();
    let observation = super::observe_materialized(&path, TRACK).unwrap().unwrap();
    let exclude = crate::git::git_exclude_path(&path).unwrap();
    std::fs::write(&exclude, "# metadata removed after observation\n").unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let invoked = tmp.path().join("git-invoked");
    let shim = bin.join("git");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprintf invoked > '{}'\nexit 73\n",
            invoked.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    env.previous.push(("PATH", std::env::var_os("PATH")));
    // SAFETY: the existing GitEnv guard serializes this module's Git environment readers.
    unsafe {
        std::env::set_var("PATH", &bin);
    }
    assert!(super::consume_materialized(observation, &root, &path, TRACK).unwrap());
    assert!(
        !invoked.exists(),
        "steady-state consumption must contain no indirect Git command"
    );
    assert!(crate::git::exclude_entry_present(&exclude, super::NEIGE_GIT_EXCLUDE_ENTRY).unwrap());
}

#[test]
fn observed_managed_metadata_rechecks_owner_under_path_lock() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::tempdir().unwrap();
    let (root, path) = sandbox(&tmp);
    materialize(&root, &path).unwrap();
    let observation = super::observe_materialized(&path, TRACK).unwrap().unwrap();
    super::write_owner_marker(&path, "different-owner").unwrap();
    assert!(super::consume_materialized(observation, &root, &path, TRACK).is_err());
    assert!(super::owned_by(&path, "different-owner"));
}

#[test]
fn observed_managed_metadata_rejects_replaced_directory_without_repair() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::tempdir().unwrap();
    let (root, path) = sandbox(&tmp);
    materialize(&root, &path).unwrap();
    let observation = super::observe_materialized(&path, TRACK).unwrap().unwrap();
    let moved = tmp.path().join("retained-old-directory");
    std::fs::rename(&path, &moved).unwrap();
    materialize(&root, &path).unwrap();
    assert!(!super::consume_materialized(observation, &root, &path, TRACK).unwrap());
    assert!(super::owned_by(&path, TRACK));
}
