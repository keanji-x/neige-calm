//! #1147 S2 — §5 tests 1 and 7.
//!
//! These live in-crate (not `tests/cases/`) because they drive
//! `provision_workspace_worktree`, which is `pub(crate)`: the point of test 1
//! is that the *real* lease provisioner succeeds against a *real* materialized
//! workspace, not that a re-implementation of it does.

use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

use super::{InitCommit, materialize_managed_workspace, materialize_managed_workspace_inner};
use crate::operation::workspace_lease::{WorkspaceLeaseTarget, provision_workspace_worktree};

/// The git environment is **process-global**, so any test in this module that
/// spawns git must hold this lock for its whole body.
///
/// `cargo-nextest` (what CI runs) gives each test its own process and would
/// hide a violation completely. A plain `cargo test` shares one process across
/// threads: with the lock scoped to a *helper function* instead of the test
/// body, `cargo test -p calm-server --lib workspace_materialize` failed 5 runs
/// out of 20, and `--test-threads=1` was always green. A test whose colour
/// depends on the runner is not evidence, so every test below takes a
/// [`GitEnv`] guard first — including the ones that only want the default
/// environment.
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

    /// git localizes its diagnostics (`LANG=zh_CN.UTF-8` on this host prints
    /// `不是一个有效的对象名`). §5 test 1 asserts on the English text, so pin
    /// the locale rather than assert against whatever the host happens to be.
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

const WAVE: &str = "wave0000000000000000000000000001";

/// `(root, repo_root)` for a fresh sandbox. The repository lives at
/// `<root>/<area>/<wave>` like production's `managed_workspace_path`.
fn sandbox(tmp: &tempfile::TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let repo_root = super::managed_workspace_path(&root, "area0000000000000000000000000001", WAVE);
    (root, repo_root)
}

fn materialize(root: &Path, repo_root: &Path) -> crate::error::Result<()> {
    materialize_managed_workspace(root, repo_root, WAVE)
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

/// `(major, minor)` of the `git` on PATH.
///
/// Behaviour under test differs across Git versions (see
/// `worktree_add_without_a_baseline_commit_is_version_dependent`), and the
/// version must be *parsed*, not inferred from an error string: the message
/// differs between bare and non-bare repositories and is localized (this host
/// prints `致命错误：不是一个有效的对象名`).
fn git_version() -> (u32, u32) {
    let out = Command::new("git").arg("--version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let nums = text
        .split_whitespace()
        .find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .unwrap_or_else(|| panic!("cannot parse `git --version` output: {text}"));
    let mut parts = nums.split('.');
    let major = parts.next().unwrap().parse().unwrap();
    let minor = parts.next().unwrap_or("0").parse().unwrap();
    (major, minor)
}

/// `git rev-list --count --all` — the number design D4 compares against 1 when
/// deciding whether a workspace is still untouched.
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

fn lease_target(repo_root: &Path) -> WorkspaceLeaseTarget {
    let wave_id = "wave0000000000000000000000000001";
    let card_id = "card0000000000000000000000000001";
    WorkspaceLeaseTarget {
        repo_root: repo_root.to_path_buf(),
        path: repo_root
            .join(".claude")
            .join("worktrees")
            .join(wave_id)
            .join(card_id),
        branch: format!("neige/{wave_id}/{card_id}"),
    }
}

/// §5 test 1 (green half): a freshly materialized managed workspace can host a
/// worker lease worktree.
#[test]
fn materialized_workspace_hosts_a_worker_worktree() {
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

    let target = lease_target(&repo_root);
    provision_workspace_worktree(&target).expect("worktree add on a materialized workspace");
    assert!(
        target.path.is_dir(),
        "lease worktree directory {} is missing",
        target.path.display()
    );
}

/// §5 test 1 (single-violation fixture): run the *production* materialize path
/// with step 3 (the empty initial commit) removed.
///
/// The assertion is **version-independent on purpose**: what step 3 guarantees
/// is that the workspace has exactly one commit, which is the baseline design
/// D4 compares against when it asks "has anything happened here yet"
/// (`git rev-list --count --all == 1`). Without the step there is no baseline
/// at all.
///
/// It deliberately does NOT assert that `git worktree add` fails — that is a
/// Git-version-dependent side effect, pinned separately in
/// `worktree_add_without_a_baseline_commit_is_version_dependent`.
#[test]
fn without_the_init_commit_there_is_no_baseline_commit() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    materialize_managed_workspace_inner(&root, &repo_root, WAVE, InitCommit::Skip).unwrap();

    // Prove the mutation actually applied.
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

    // …and the unmutated path establishes it, so the gap above is attributable
    // to the missing commit and nothing else.
    materialize(&root, &repo_root).unwrap();
    assert!(head_resolves(&repo_root));
    assert_eq!(count_all_commits(&repo_root), 1);
    let target = lease_target(&repo_root);
    provision_workspace_worktree(&target).expect("worktree add after the commit is restored");
}

/// Pins the Git-version-dependent behaviour that the original rationale for
/// step 3 got wrong.
///
/// The design used to justify the empty initial commit with "otherwise
/// `git worktree add` fails and the first worker cannot start". That is true
/// only on Git **< 2.42.0**. Git 2.42.0 (commit `128e5496b`, "worktree add:
/// extend DWIM to infer `--orphan`", Jacob Abel) made `worktree add` in a
/// repository with an unborn HEAD *succeed*: it infers `--orphan`, prints
/// `No possible source branch, inferring '--orphan'` to stderr, and points the
/// new worktree's HEAD at an unborn branch. No commit is created.
/// `git-worktree` documents it as "as if `--orphan` was passed".
///
/// So on modern Git the missing baseline is **silent**, which is worse than the
/// old hard error: the lease worktree exists, has no history, and each one is
/// an unrelated orphan. The commit is still required — for the D4 baseline —
/// but not for the reason originally written down.
///
/// Version is parsed, not string-matched: the old error text differs between
/// bare (`invalid reference: HEAD`) and non-bare (`not a valid object name`)
/// repositories and is localized.
#[test]
fn worktree_add_without_a_baseline_commit_is_version_dependent() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    materialize_managed_workspace_inner(&root, &repo_root, WAVE, InitCommit::Skip).unwrap();
    assert_eq!(count_all_commits(&repo_root), 0);

    let version = git_version();
    let target = lease_target(&repo_root);
    let result = provision_workspace_worktree(&target);

    if version < (2, 42) {
        let error = result.expect_err(&format!(
            "git {}.{} predates 2.42.0, where `worktree add` on an unborn HEAD \
             fails outright",
            version.0, version.1
        ));
        assert!(
            error.to_string().contains("not a valid object name"),
            "git {}.{}: expected the unborn-HEAD failure, got: {error}",
            version.0,
            version.1
        );
    } else {
        result.unwrap_or_else(|error| {
            panic!(
                "git {}.{} is >= 2.42.0, where `worktree add` DWIMs `--orphan` \
                 on an unborn HEAD and succeeds; got: {error}",
                version.0, version.1
            )
        });
        assert_eq!(
            count_all_commits(&repo_root),
            0,
            "git {}.{}: the inferred `--orphan` worktree must still create no \
             commit — that is exactly why the baseline has to come from step 3",
            version.0,
            version.1
        );
    }
}

/// §5 test 7: a global `commit.gpgsign=true` makes `git commit --allow-empty`
/// fail hard unless materialize overrides it for its own invocation.
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

    // Sanity: the injected config really does break an un-overridden commit,
    // otherwise the assertion below is vacuous on hosts without gpg.
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

/// D3 step 1: a directory we did not create is never adopted.
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

/// D3: materialize is idempotent, and re-running it does not disturb the
/// worker output already sitting in the workspace.
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

/// D3 step 4: the exclusion goes into `.git/info/exclude`, never `.gitignore`
/// — a `.gitignore` would show up as `?? .gitignore` and make D4's
/// "nothing on disk" predicate permanently false.
#[test]
fn materialize_excludes_worktrees_via_git_info_exclude_not_gitignore() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    materialize(&root, &repo_root).unwrap();

    let exclude = std::fs::read_to_string(repo_root.join(".git").join("info").join("exclude"))
        .expect("`.git/info/exclude` must exist");
    assert!(
        exclude
            .lines()
            .any(|line| line.trim() == ".claude/worktrees/"),
        "exclude file does not carry the worktree root: {exclude}"
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

/// D2: the layout is `<root>/<area_id>/<wave_id>`, ids only.
#[test]
fn managed_path_is_root_area_wave() {
    let _env = GitEnv::c_locale();
    let path = super::managed_workspace_path(Path::new("/srv/ws"), "area1", "wave1");
    assert_eq!(path, Path::new("/srv/ws/area1/wave1"));
}

// ---------------------------------------------------------------------------
// S2 red-team fixtures.
// ---------------------------------------------------------------------------

/// Measures how many materializations of one path are inside the critical
/// section at the same time.
///
/// The per-path mutex's whole job is "never more than one", and that property
/// is not observable through git's own symptoms: 24 barrier-synchronized
/// threads racing `git init` on one directory did NOT reproduce a failure on
/// this host even with the lock removed. Asserting the symptom would therefore
/// have been a test that passes either way — worse than no test, because it
/// would read as coverage. This probe asserts the property directly, so
/// removing the lock turns it red deterministically.
pub(super) struct OverlapProbe(std::path::PathBuf);

/// `path -> (currently inside, peak simultaneous, total entries)`
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

    /// Highest simultaneous occupancy observed for `path`.
    fn peak(path: &Path) -> usize {
        let guard = OVERLAP.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .as_ref()
            .and_then(|m| m.get(path))
            .map(|(_, peak, _)| *peak)
            .unwrap_or(0)
    }

    /// How many materializations of `path` the probe saw in total. Guards the
    /// peak assertion against passing because nothing ever entered.
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

/// Build a *third-party* git repository with real history and a working file,
/// as if the user had put one of their projects on the derived path.
/// Caller must already hold a [`GitEnv`] guard — this spawns git.
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

/// **B2** — "is it ours" must be decided by our own marker, never by "is this a
/// git repository".
///
/// A third-party repository sitting on the derived path answers *yes* to the
/// latter. Adopting it means the server appends to their `.git/info/exclude`,
/// and — since S5 `remove_dir_all`s every `kind = Managed` directory — arms a
/// deletion of the user's real work. Nothing about their repository may change.
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

    // Zero changes, byte for byte.
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

/// **B2, single-violation fixture** — prove the marker is what refuses it.
///
/// Same third-party repository, but pre-marked as ours: materialize now
/// proceeds. If the guard were "is this a git repository", the marker could not
/// change the outcome and this assertion would fail — which is exactly what
/// makes the test above attributable to the marker and not to some other check.
#[test]
fn the_marker_is_what_decides_adoption() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    third_party_repo(&repo_root);
    std::fs::write(
        repo_root.join(".git").join(super::OWNER_MARKER),
        format!("{WAVE}\n"),
    )
    .unwrap();

    materialize(&root, &repo_root)
        .expect("a directory carrying our marker is ours and must be accepted");
}

/// **B2** — a marker naming a *different* wave is corruption, not an invitation.
#[test]
fn a_marker_for_another_wave_is_refused() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    std::fs::create_dir_all(repo_root.join(".git")).unwrap();
    std::fs::write(
        repo_root.join(".git").join(super::OWNER_MARKER),
        "some-other-wave\n",
    )
    .unwrap();

    let error = materialize(&root, &repo_root).expect_err("foreign marker must be refused");
    assert!(
        error.to_string().contains("not `"),
        "unexpected error: {error}"
    );
}

/// **B3** — a symlink out of the workspace root must be refused.
///
/// `create_dir_all` follows symlinks, so `<root>/<area>` pointing elsewhere
/// yields a stored path that satisfies every *lexical* `starts_with` while the
/// repository — and all worker output in it — lives outside the tree S5
/// believes it owns.
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

    // The stored path is lexically inside the root — this is the check that
    // the invariant test and D8's prefix assertion would both be making.
    assert!(repo_root.starts_with(&root));

    let error =
        materialize(&root, &repo_root).expect_err("a symlink out of the root must be refused");
    assert!(
        error.to_string().contains("outside the managed"),
        "unexpected error: {error}"
    );
}

/// **B3, single-violation fixture** — the same layout with a *real* directory
/// instead of the symlink succeeds, so the refusal above is attributable to the
/// symlink and not to the nested path shape.
#[test]
#[cfg(unix)]
fn the_same_layout_without_a_symlink_succeeds() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    std::fs::create_dir_all(repo_root.parent().unwrap()).unwrap();
    materialize(&root, &repo_root).expect("a real directory under the root is fine");
}

/// **B4** — concurrent materialization of one path must not tear.
///
/// The launchpad's `ensure` is expected to race with itself (it carries a
/// unique-index retry), and materialization runs outside the transaction.
/// Four-way concurrency previously produced `cannot lock config file
/// .git/config` and a spurious "not a neige-managed repository".
#[test]
fn concurrent_materialization_of_one_path_all_succeed() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);

    const THREADS: usize = 24;
    // A barrier, so every thread enters `materialize` at the same instant.
    // Without it the first caller usually finishes before the rest start and
    // they all take the cheap steady-state path — no contention, no coverage.
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

    // The load-bearing assertion. See `OverlapProbe`: git's own failure modes
    // did not reproduce on this host even with the lock removed, so asserting
    // only "they all succeeded" would pass with or without the mutex.
    assert_eq!(
        OverlapProbe::peak(&repo_root),
        1,
        "two materializations of one workspace overlapped: the per-path mutex \
         is not doing its job, and the interleaving that leaves a half-built \
         directory behind is reachable again"
    );
    // …and the probe genuinely saw every thread arrive, so `peak == 1` cannot
    // be passing because the critical section was never entered.
    assert_eq!(
        OverlapProbe::entries(&repo_root),
        THREADS,
        "the probe did not observe every materialization, so the peak above is \
         not evidence of anything"
    );
}

/// **B4** — a directory left half-built by a crash is repairable.
///
/// Simulated by marking the directory as ours and then destroying the
/// repository under it, which is the state a process killed mid-`git init`
/// leaves behind. Before the marker existed this was permanently
/// un-materializable: non-empty, unrecognisable, 500 on every later call with
/// no path back.
#[test]
fn a_half_built_workspace_of_ours_is_repaired() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    materialize(&root, &repo_root).unwrap();

    // Destroy everything except our marker, the way a crash mid-init would.
    let marker = std::fs::read_to_string(repo_root.join(".git").join(super::OWNER_MARKER)).unwrap();
    std::fs::remove_dir_all(repo_root.join(".git")).unwrap();
    std::fs::create_dir_all(repo_root.join(".git")).unwrap();
    std::fs::write(repo_root.join(".git").join(super::OWNER_MARKER), marker).unwrap();
    assert!(!head_resolves(&repo_root), "fixture did not break the repo");

    materialize(&root, &repo_root).expect("a marked half-built workspace must be repaired");
    assert!(head_resolves(&repo_root));
}

/// **B6** — the git *environment* is isolated, not just the config files.
///
/// Each of these was measured to break materialization when inherited.
#[test]
fn materialize_survives_hostile_git_environment_variables() {
    let tmp = tempfile::TempDir::new().unwrap();
    let poison = tmp.path().join("poison");
    std::fs::create_dir_all(&poison).unwrap();
    let env = GitEnv::set(&[
        ("LC_ALL", std::ffi::OsStr::new("C")),
        ("LANGUAGE", std::ffi::OsStr::new("")),
        ("GIT_DIR", poison.as_os_str()),
        ("GIT_WORK_TREE", poison.as_os_str()),
        ("GIT_INDEX_FILE", poison.as_os_str()),
        ("GIT_OBJECT_DIRECTORY", poison.as_os_str()),
        ("GIT_AUTHOR_DATE", std::ffi::OsStr::new("not-a-date")),
        ("GIT_COMMITTER_NAME", std::ffi::OsStr::new("")),
    ]);

    // Sanity: the injected environment really does break an un-isolated git,
    // otherwise this test would pass without the isolation doing anything.
    let control = tmp.path().join("control");
    std::fs::create_dir_all(&control).unwrap();
    let control_init = Command::new("git")
        .args(["-c", "init.templateDir=", "-c", "init.defaultBranch=main"])
        .arg("init")
        .arg(&control)
        .output()
        .unwrap();
    let control_commit = Command::new("git")
        .arg("-C")
        .arg(&control)
        .args([
            "-c",
            "commit.gpgsign=false",
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
        !control_init.status.success() || !control_commit.status.success(),
        "the injected git environment did not break an un-isolated git; this \
         test would pass without any isolation"
    );

    let tmp2 = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp2);
    materialize(&root, &repo_root).expect("materialize under a hostile git environment");

    // Restore the environment before verifying, so the repository is judged by
    // a clean `git` — proving materialize produced a genuinely good repository
    // rather than one that only looks good through the same isolation.
    drop(env);
    assert!(head_resolves(&repo_root));
    let target = lease_target(&repo_root);
    provision_workspace_worktree(&target).expect("worktree add on the isolated workspace");
}

/// **N1** — third leg of D3 contract (3): clear our own half-built state.
///
/// Marker present (so the directory is provably ours) + a `.git/config.lock`
/// left by a process killed mid-`init`. Without the cleanup `git init` fails
/// with `could not lock config file` on *every* subsequent call — the same
/// permanent 500 the contract exists to abolish, entered through a lock file
/// instead of an unmarked directory. On the launchpad that is a permanently
/// dead Today panel.
#[test]
fn a_stale_lock_file_from_a_killed_init_is_cleared() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);

    // The state a SIGKILL mid-`git init` leaves: ours, no resolvable HEAD, and
    // a lock file guarding the config.
    std::fs::create_dir_all(repo_root.join(".git")).unwrap();
    std::fs::write(
        repo_root.join(".git").join(super::OWNER_MARKER),
        format!("{WAVE}\n"),
    )
    .unwrap();
    std::fs::write(repo_root.join(".git/config.lock"), "").unwrap();
    assert!(
        !head_resolves(&repo_root),
        "fixture must start un-materialized"
    );

    materialize(&root, &repo_root).expect("a stale lock of ours must be cleared, not reported");
    assert!(head_resolves(&repo_root));
    assert!(
        !repo_root.join(".git/config.lock").exists(),
        "the stale lock is still there; the next call will fail the same way"
    );

    // Idempotent afterwards — the failure mode was that it repeated forever.
    materialize(&root, &repo_root).expect("second call");
}

/// **N1** — the same for a lock deeper in `.git/`, e.g. a killed `commit`.
#[test]
fn a_stale_ref_lock_is_cleared() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    materialize_managed_workspace_inner(&root, &repo_root, WAVE, InitCommit::Skip).unwrap();
    std::fs::write(repo_root.join(".git/HEAD.lock"), "").unwrap();
    assert!(!head_resolves(&repo_root));

    materialize(&root, &repo_root).expect("a stale ref lock of ours must be cleared");
    assert!(head_resolves(&repo_root));
}

/// **N2** — `GIT_TEMPLATE_DIR` outranks `-c init.templateDir=`.
///
/// Git's precedence is `--template` > `GIT_TEMPLATE_DIR` > `init.templateDir`,
/// so D3 step 2's `-c` override does **not** stop a template's `hooks/` from
/// being copied into the new repository. The init commit itself survives
/// (`--no-verify`), which is exactly what makes this quiet: the hook only
/// fires later, on every git command a worker runs inside the workspace.
#[test]
fn a_template_dir_in_the_environment_cannot_inject_hooks() {
    let tmp = tempfile::TempDir::new().unwrap();
    let template = tmp.path().join("tmpl");
    std::fs::create_dir_all(template.join("hooks")).unwrap();
    let hook = template.join("hooks").join("pre-commit");
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let env = GitEnv::set(&[
        ("LC_ALL", std::ffi::OsStr::new("C")),
        ("LANGUAGE", std::ffi::OsStr::new("")),
        ("GIT_TEMPLATE_DIR", template.as_os_str()),
    ]);

    // Sanity: the injection really does work against an un-isolated git, even
    // WITH the `-c init.templateDir=` override D3 step 2 relies on. Without
    // this the test could pass on a git that ignores the variable entirely.
    let control = tmp.path().join("control");
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
    assert!(
        control.join(".git/hooks/pre-commit").exists(),
        "GIT_TEMPLATE_DIR did not inject through `-c init.templateDir=` on this \
         git, so this test cannot detect the hole it is guarding"
    );

    let (root, repo_root) = sandbox(&tmp);
    materialize(&root, &repo_root).expect("materialize under an injected template dir");
    assert!(
        !repo_root.join(".git/hooks/pre-commit").exists(),
        "a hook was injected into a server-owned workspace; every later git \
         command a worker runs there would execute it"
    );

    drop(env);
    assert!(head_resolves(&repo_root));
}

/// **N7** — pinned, NOT fixed in S2.
///
/// The symlink refusal happens *after* materialization has already written a
/// complete repository at the resolved location, so a rejected create leaves a
/// marked, fully-formed repository outside the workspace root that nothing
/// will ever collect. Refusing earlier is not enough on its own (the resolved
/// location is only knowable after `create_dir_all`), so this needs a real
/// cleanup path — S5's job, and this test is here so S5 does not rediscover it
/// as a new bug.
#[test]
#[cfg(unix)]
fn n7_a_refused_symlink_workspace_leaves_an_orphan_repository_outside_the_root() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, repo_root.parent().unwrap()).unwrap();

    materialize(&root, &repo_root).expect_err("symlink must be refused");

    let orphan = elsewhere.join(WAVE);
    assert!(
        orphan.join(".git").is_dir() && orphan.join(".git").join(super::OWNER_MARKER).exists(),
        "KNOWN GAP (#1147 N7): if this assertion starts failing, the orphan is \
         being cleaned up — delete this test and note the fix, do not loosen it"
    );
}

/// **N5** — pinned, NOT fixed in S2.
///
/// Losing the ownership marker (a partially restored backup, an over-eager
/// cleanup) makes our own workspace permanently un-materializable, and there is
/// no administrative way to re-claim it: the refusal is unconditional and
/// nothing exposes "adopt this directory". S5/S6 need a re-claim path.
#[test]
fn n5_losing_our_own_marker_is_an_unrecoverable_refusal() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (root, repo_root) = sandbox(&tmp);
    materialize(&root, &repo_root).unwrap();
    std::fs::remove_file(repo_root.join(".git").join(super::OWNER_MARKER)).unwrap();

    let error = materialize(&root, &repo_root)
        .expect_err("KNOWN GAP (#1147 N5): a marker-less workspace of ours is refused forever");
    assert!(
        error
            .to_string()
            .contains("carries no neige ownership marker"),
        "unexpected error: {error}"
    );
    // Deliberately asserting the *gap*: there is no supported recovery. If a
    // later slice adds one, this test fails and the author must record it.
}

/// **N4** — pinned, NOT fixed in S2.
///
/// Moving `CALM_WORKSPACE_ROOT` (or `$HOME`) strands every existing managed
/// wave: its stored path is no longer under the configured root, so the
/// containment assertion refuses it and the wave can never take a lease again.
/// The launchpad self-heals because its path is re-derived on every `ensure`;
/// an ordinary wave has no such path. A migration is owed — S5, with the
/// recycle/relocate machinery.
#[test]
fn n4_moving_the_workspace_root_strands_existing_waves() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let (old_root, repo_root) = sandbox(&tmp);
    materialize(&old_root, &repo_root).unwrap();

    let new_root = tmp.path().join("moved-root");
    std::fs::create_dir_all(&new_root).unwrap();
    let error = materialize_managed_workspace(&new_root, &repo_root, WAVE).expect_err(
        "KNOWN GAP (#1147 N4): an existing wave under the old root is refused \
         outright after the root moves, with no migration path",
    );
    assert!(
        error.to_string().contains("outside the managed"),
        "unexpected error: {error}"
    );
    assert!(
        error.to_string().contains("N4"),
        "the error must name the gap so the next reader is not left guessing \
         about a symlink that is not there: {error}"
    );
}
