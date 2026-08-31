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

/// `GIT_CONFIG_GLOBAL` is process-global. `cargo-nextest` gives each test its
/// own process so this is belt-and-braces, but a plain `cargo test` run shares
/// one process across threads and the two env-touching tests below would race
/// every other git-spawning test in this binary.
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
            // SAFETY: the process-wide lock above is the only writer of these
            // vars in this crate's tests, and every git-spawning test in this
            // module takes the same lock before reading them.
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

fn head_resolves(path: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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
    let tmp = tempfile::TempDir::new().unwrap();
    let repo_root = tmp.path().join("cove").join("wave");
    materialize_managed_workspace(&repo_root).unwrap();

    let target = lease_target(&repo_root);
    provision_workspace_worktree(&target).expect("worktree add on a materialized workspace");
    assert!(
        target.path.is_dir(),
        "lease worktree directory {} is missing",
        target.path.display()
    );
}

/// §5 test 1 (single-violation fixture): run the *production* materialize path
/// with step 3 (the empty initial commit) removed. `git worktree add` must
/// die with `not a valid object name`.
#[test]
fn without_the_init_commit_the_worker_worktree_cannot_be_created() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let repo_root = tmp.path().join("cove").join("wave");
    materialize_managed_workspace_inner(&repo_root, InitCommit::Skip).unwrap();

    // Prove the mutation actually applied: the whole point of step 3 is that
    // HEAD resolves afterwards.
    assert!(
        !head_resolves(&repo_root),
        "mutation did not apply — HEAD still resolves, so the assertion below \
         would pass for the wrong reason"
    );

    let target = lease_target(&repo_root);
    let error = provision_workspace_worktree(&target)
        .expect_err("worktree add must fail without an initial commit");
    let message = error.to_string();
    assert!(
        message.contains("not a valid object name"),
        "expected the empty-HEAD failure, got: {message}"
    );

    // …and the unmutated path succeeds on the same directory, so the failure
    // above is attributable to the missing commit and nothing else.
    materialize_managed_workspace(&repo_root).unwrap();
    assert!(head_resolves(&repo_root));
    provision_workspace_worktree(&target).expect("worktree add after the commit is restored");
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

    let repo_root = tmp.path().join("cove").join("wave");
    materialize_managed_workspace(&repo_root).expect("materialize under a hostile global config");
    assert!(head_resolves(&repo_root));
}

/// D3 step 1: a directory we did not create is never adopted.
#[test]
fn materialize_refuses_a_foreign_non_empty_directory() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo_root = tmp.path().join("cove").join("wave");
    std::fs::create_dir_all(&repo_root).unwrap();
    std::fs::write(repo_root.join("someones-notes.md"), "hi").unwrap();

    let error = materialize_managed_workspace(&repo_root)
        .expect_err("a non-empty foreign directory must be refused");
    assert!(
        error.to_string().contains("refusing to reuse"),
        "unexpected error: {error}"
    );
}

/// D3: materialize is idempotent, and re-running it does not disturb the
/// worker output already sitting in the workspace.
#[test]
fn materialize_is_idempotent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo_root = tmp.path().join("cove").join("wave");
    materialize_managed_workspace(&repo_root).unwrap();
    let head = Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    std::fs::write(repo_root.join("worker-output.txt"), "produced").unwrap();

    materialize_managed_workspace(&repo_root).expect("second materialize");

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
    let tmp = tempfile::TempDir::new().unwrap();
    let repo_root = tmp.path().join("cove").join("wave");
    materialize_managed_workspace(&repo_root).unwrap();

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

/// D2: the layout is `<root>/<cove_id>/<wave_id>`, ids only.
#[test]
fn managed_path_is_root_cove_wave() {
    let path = super::managed_workspace_path(Path::new("/srv/ws"), "cove1", "wave1");
    assert_eq!(path, Path::new("/srv/ws/cove1/wave1"));
}
