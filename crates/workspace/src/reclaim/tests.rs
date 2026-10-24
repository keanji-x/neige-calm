use super::*;
use std::{
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    path: PathBuf,
}
fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "reclaim@example.test"]);
        git(&repo, &["config", "user.name", "Reclaim"]);
        std::fs::write(repo.join("file"), "original").unwrap();
        std::fs::write(repo.join(".gitignore"), "ignored\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "init"]);
        let path = tmp.path().join("extra");
        git(
            &repo,
            &["worktree", "add", "-qb", "extra", path.to_str().unwrap()],
        );
        Self {
            _tmp: tmp,
            repo,
            path,
        }
    }
    fn request(&self) -> ReclaimRequest {
        ReclaimRequest {
            repo: self.repo.clone(),
            path: self.path.clone(),
            protected: vec![],
            excluded_refs: vec![],
            ownership_conflict: false,
            unknown_terminal: false,
        }
    }
}

#[test]
fn reclaim_dirty_preflight_never_attempts_remove() {
    for name in ["file", "untracked", "ignored"] {
        let f = Fixture::new();
        std::fs::write(f.path.join(name), "do not delete").unwrap();
        let attempts = AtomicUsize::new(0);
        let result = reclaim(&f.request(), |point, _| {
            if point == "extra-worktree-remove-attempt" {
                attempts.fetch_add(1, Ordering::SeqCst);
            }
        });
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            0,
            "dirty preflight must run before Git removal: {name}"
        );
        assert_eq!(result.unwrap(), ReclaimOutcome::Retained("dirty"));
        assert_eq!(
            std::fs::read_to_string(f.path.join(name)).unwrap(),
            "do not delete"
        );
    }
}

#[test]
fn reclaim_clean_removes_only_linked_tree_and_keeps_refs() {
    let f = Fixture::new();
    let attempts = AtomicUsize::new(0);
    assert_eq!(
        reclaim(&f.request(), |point, _| {
            if point == "extra-worktree-remove-attempt" {
                attempts.fetch_add(1, Ordering::SeqCst);
            }
        })
        .unwrap(),
        ReclaimOutcome::Removed
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert!(!f.path.exists());
    git(&f.repo, &["show-ref", "--verify", "refs/heads/extra"]);
    assert!(f.repo.join("file").exists());
}

#[test]
fn reclaim_ref_terminal_identity_and_descendant_preflights() {
    for variant in [
        "terminal",
        "conflict",
        "relative",
        "symlink",
        "foreign",
        "missing-git",
        "no-ref",
        "excluded-ref",
        "nested",
        "main",
        "git-error",
        "alias",
    ] {
        let f = Fixture::new();
        let mut request = f.request();
        match variant {
            "terminal" => request.unknown_terminal = true,
            "conflict" => request.ownership_conflict = true,
            "relative" => request.path = PathBuf::from("extra"),
            "symlink" => {
                request.path = f._tmp.path().join("link");
                std::os::unix::fs::symlink(&f.path, &request.path).unwrap();
            }
            "foreign" => {
                let other = Fixture::new();
                request.repo = other.repo.clone();
                assert!(matches!(
                    reclaim(&request, |_, _| {}).unwrap(),
                    ReclaimOutcome::Retained(_)
                ));
                continue;
            }
            "missing-git" => {
                std::fs::remove_file(f.path.join(".git")).unwrap();
            }
            "no-ref" | "excluded-ref" => {
                git(&f.path, &["checkout", "--detach", "-q"]);
                std::fs::write(f.path.join("file"), "new commit").unwrap();
                git(&f.path, &["commit", "-qam", "unreferenced"]);
                if variant == "excluded-ref" {
                    git(&f.path, &["branch", "disposable"]);
                    request.excluded_refs.push("refs/heads/disposable".into());
                }
            }
            "nested" => request.protected.push(f.path.join("nested")),
            "main" => request.path = f.repo.clone(),
            "git-error" => request.repo = f._tmp.path().join("gone"),
            "alias" => {
                let parent = f._tmp.path().join("alias");
                std::os::unix::fs::symlink(f._tmp.path(), &parent).unwrap();
                request.path = parent.join("extra");
            }
            _ => unreachable!(),
        }
        let attempts = AtomicUsize::new(0);
        let result = reclaim(&request, |point, _| {
            if point == "extra-worktree-remove-attempt" {
                attempts.fetch_add(1, Ordering::SeqCst);
            }
        });
        assert_eq!(attempts.load(Ordering::SeqCst), 0, "{variant}: {result:?}");
        assert!(!matches!(result, Ok(ReclaimOutcome::Removed)), "{variant}");
        assert!(f.path.exists());
    }
}

#[test]
fn reclaim_detached_head_with_surviving_ref_is_allowed() {
    let f = Fixture::new();
    git(&f.path, &["checkout", "--detach", "-q"]);
    assert_eq!(
        reclaim(&f.request(), |_, _| {}).unwrap(),
        ReclaimOutcome::Removed
    );
}
