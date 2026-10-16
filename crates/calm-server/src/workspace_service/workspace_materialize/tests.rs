use std::path::Path;
use std::sync::{Mutex, MutexGuard};
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

fn user_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec![
            "-c",
            "user.name=neige-test",
            "-c",
            "user.email=neige-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=",
            "commit",
            "--allow-empty",
            "-m",
            "seed",
        ],
    ] {
        let out = super::neige_git_command()
            .arg("-C")
            .arg(dir)
            .args(&args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn git_in(dir: &Path, args: &[&str]) {
    let out = super::neige_git_command()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn attached_admission_names_the_ref_the_track_branch_lives_under() {
    let branch =
        workspace::worktree::track_branch_for("track0000000000000000000000000001").unwrap();
    let track_ref = format!("refs/heads/{branch}");
    assert!(
        track_ref.starts_with(&format!("{}/", super::SLICE_BRANCH_NAMESPACE_REF)),
        "the admission check guards `{}`, but a track branch resolves to \
         `{track_ref}` — the check is watching a ref the track branch does not \
         live under, so it can never prevent the conflict it exists for",
        super::SLICE_BRANCH_NAMESPACE_REF
    );
}

#[test]
fn attaching_a_repo_holding_refs_heads_neige_is_refused_by_name() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path().join("users-own-repo");
    user_repo(&repo);
    git_in(&repo, &["branch", "neige"]);

    let err = super::validate_attached_workspace(&repo)
        .expect_err("a repository holding `refs/heads/neige` must be refused");
    assert!(
        matches!(err, crate::error::CalmError::BadRequest(_)),
        "naming a directory that cannot host slice branches is the caller's \
         mistake, not a server failure: {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains(super::SLICE_BRANCH_NAMESPACE_REF),
        "the refusal must name the offending ref: {msg}"
    );
    assert!(
        msg.contains("neige/track-<id>"),
        "the refusal must name what the ref collides with: {msg}"
    );
}

#[test]
fn a_repo_carrying_old_slice_branches_is_still_attachable() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path().join("users-own-repo");
    user_repo(&repo);
    git_in(
        &repo,
        &[
            "branch",
            "neige/track0000000000000000000000000009/card0000000000000000000000000009",
        ],
    );

    super::validate_attached_workspace(&repo).expect(
        "leftover slice branches occupy `refs/heads/neige/…` as a directory, \
         which is what a new slice branch needs — refusing them would reject \
         every repository a worker has already run in",
    );
}

/// Reached directly: `validate_attached_workspace`'s `rev-parse` gate would reject a non-repository first.
#[test]
fn an_unanswerable_show_ref_is_refused() {
    let _env = GitEnv::c_locale();
    let tmp = tempfile::TempDir::new().unwrap();
    let not_a_repo = tmp.path().join("plain-directory");
    std::fs::create_dir_all(&not_a_repo).unwrap();

    let err = super::ensure_slice_branch_namespace_is_free(&not_a_repo)
        .expect_err("an unanswered `show-ref` must not be read as `absent`");
    let msg = err.to_string();
    assert!(
        msg.contains("cannot tell whether"),
        "the refusal must say the question went unanswered, not claim a \
         conflict that was never observed: {msg}"
    );
}
