//! The local half of an upstream lease base (#1777): which upstream commit is
//! known, and where a lease starts by HEAD's relation to it.
//!
//! The fixtures here are shared with the fetch tests
//! (`upstream_fetch_tests`) and the worker adapters' tests, which drive the
//! same repositories through `before_insert` and `prepare_tx`.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::upstream::*;
use super::upstream_fetch::{UpstreamRefresh, refresh_upstream};

/// `git -C <dir> <args>` with a fixed identity; panics on failure, returns
/// stdout trimmed.
pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Upstream Test",
            "-c",
            "user.email=upstream@example.test",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
        dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A repository an attached checkout tracks: a non-bare clone of the
/// checkout, its `origin`, so a commit here is a commit the checkout's HEAD
/// branch lags behind.
pub(crate) struct Origin {
    _dir: tempfile::TempDir,
    path: PathBuf,
    pub(crate) branch: String,
}

impl Origin {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// One more commit on the tracked branch; returns the new tip.
    pub(crate) fn commit(&self, message: &str) -> String {
        git(
            self.path(),
            &["commit", "--allow-empty", "-q", "-m", message],
        );
        git(self.path(), &["rev-parse", "HEAD"])
    }

    /// The upstream the attached checkout's branch names.
    pub(crate) fn upstream(&self) -> Upstream {
        Upstream {
            remote: "origin".into(),
            merge: format!("refs/heads/{}", self.branch),
            tracking_ref: Some(self.tracking_ref()),
        }
    }

    pub(crate) fn tracking_ref(&self) -> String {
        format!("refs/remotes/origin/{}", self.branch)
    }
}

/// Give `attached` an `origin` remote its HEAD branch tracks
/// (`branch.<b>.remote = origin`, `branch.<b>.merge = refs/heads/<b>`), with
/// `refs/remotes/origin/<b>` fetched once — the shape of a clone.
pub(crate) fn attach_origin(attached: &Path) -> Origin {
    let branch = git(attached, &["symbolic-ref", "--short", "HEAD"]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("origin");
    git(attached, &["clone", "-q", ".", path.to_str().unwrap()]);
    git(
        attached,
        &["remote", "add", "origin", path.to_str().unwrap()],
    );
    git(attached, &["fetch", "-q", "origin"]);
    git(
        attached,
        &[
            "branch",
            "-q",
            &format!("--set-upstream-to=origin/{branch}"),
        ],
    );
    Origin {
        _dir: dir,
        path,
        branch,
    }
}

/// Point `origin` at a path that is not a repository: every fetch fails.
pub(crate) fn break_origin(attached: &Path) {
    let gone = std::env::temp_dir().join("neige-upstream-test-no-such-remote");
    git(
        attached,
        &["remote", "set-url", "origin", gone.to_str().unwrap()],
    );
}

pub(crate) fn kernel_ref(origin: &Origin) -> String {
    origin.upstream().kernel_ref()
}

/// A transport witness: `origin`'s upload-pack runs through a shell line that
/// appends one line to the returned marker file first, so every fetch that
/// reaches the remote — the kernel's or any other — is counted. `then` is the
/// rest of the line (`git-upload-pack` to serve the fetch, `exit 1` to fail
/// it).
pub(crate) fn witness_transport_then(attached: &Path, then: &str) -> PathBuf {
    let marker = tempfile::Builder::new()
        .prefix("neige-upstream-witness-")
        .tempfile()
        .unwrap()
        .into_temp_path()
        .keep()
        .unwrap();
    std::fs::write(&marker, "").unwrap();
    git(
        attached,
        &[
            "config",
            "remote.origin.uploadpack",
            &format!("echo fetch >> '{}'; {then}", marker.display()),
        ],
    );
    marker
}

/// [`witness_transport_then`] serving the fetch.
pub(crate) fn witness_transport(attached: &Path) -> PathBuf {
    witness_transport_then(attached, "git-upload-pack")
}

/// How many transport invocations the witness has seen.
pub(crate) fn transport_count(marker: &Path) -> usize {
    std::fs::read_to_string(marker).unwrap().lines().count()
}

/// Everything of the user's the fetch must not touch: every ref outside
/// `refs/neige/`, `FETCH_HEAD` (absent or its bytes), and `git status` with
/// its ahead/behind line.
pub(crate) fn user_ref_state(repo: &Path) -> (String, Option<Vec<u8>>, String) {
    let refs = git(repo, &["for-each-ref", "--format=%(objectname) %(refname)"])
        .lines()
        .filter(|line| !line.contains(" refs/neige/"))
        .collect::<Vec<_>>()
        .join("\n");
    let common_dir = PathBuf::from(git(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ));
    let fetch_head = std::fs::read(common_dir.join("FETCH_HEAD")).ok();
    let status = git(repo, &["status", "--porcelain=v2", "--branch"]);
    (refs, fetch_head, status)
}

pub(crate) fn attached_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    std::fs::write(dir.path().join("README.md"), "initial\n").unwrap();
    git(dir.path(), &["add", "README.md"]);
    git(dir.path(), &["commit", "-q", "-m", "initial"]);
    dir
}

/// A local commit on the attached checkout that its upstream does not have;
/// returns the new HEAD.
pub(crate) fn commit_locally(attached: &Path, file: &str) -> String {
    std::fs::write(attached.join(file), "unpushed\n").unwrap();
    git(attached, &["add", file]);
    git(attached, &["commit", "-q", "-m", "unpushed local commit"]);
    git(attached, &["rev-parse", "HEAD"])
}

fn known_sha(repo: &Path) -> Option<String> {
    last_known_upstream(repo).unwrap().map(|known| known.sha)
}

/// Fetch failure with a kernel ref from an earlier fetch: the base is that
/// kernel ref, even though the remote-tracking ref is older still.
#[tokio::test]
async fn failed_fetch_resolves_the_kernel_ref() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    let tracking = git(attached.path(), &["rev-parse", &origin.tracking_ref()]);
    let fetched = origin.commit("fetched once");
    assert!(matches!(
        refresh_upstream(attached.path()).await,
        UpstreamRefresh::Fetched { .. }
    ));
    origin.commit("never fetched");
    break_origin(attached.path());

    let refresh = refresh_upstream(attached.path()).await;

    assert!(
        matches!(refresh, UpstreamRefresh::Failed { .. }),
        "{refresh:?}"
    );
    assert_ne!(fetched, tracking);
    let known = last_known_upstream(attached.path()).unwrap().unwrap();
    assert_eq!(known.sha, fetched);
    assert_eq!(known.ref_name, kernel_ref(&origin));
}

/// Fetch failure and no kernel ref yet: the base is the repository's own
/// remote-tracking ref, read and not written.
#[tokio::test]
async fn failed_fetch_without_kernel_ref_resolves_the_tracking_ref() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    origin.commit("upstream moved");
    git(attached.path(), &["fetch", "-q", "origin"]);
    let tracking = git(attached.path(), &["rev-parse", &origin.tracking_ref()]);
    let head = git(attached.path(), &["rev-parse", "HEAD"]);
    assert_ne!(tracking, head, "HEAD lags its upstream");
    break_origin(attached.path());

    assert!(matches!(
        refresh_upstream(attached.path()).await,
        UpstreamRefresh::Failed { .. }
    ));

    assert_eq!(known_sha(attached.path()), Some(tracking));
}

/// The human fetched after the kernel's last fetch: the newer
/// remote-tracking ref wins over the stale kernel ref (the descendant of the
/// two). After a force-push, when the two have diverged, the kernel ref wins.
#[tokio::test]
async fn the_fresher_of_kernel_and_tracking_ref_wins() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    let stale = origin.commit("the kernel fetched this");
    assert!(matches!(
        refresh_upstream(attached.path()).await,
        UpstreamRefresh::Fetched { .. }
    ));
    let newer = origin.commit("the human fetched this later");
    git(attached.path(), &["fetch", "-q", "origin"]);
    assert_eq!(
        git(attached.path(), &["rev-parse", &kernel_ref(&origin)]),
        stale
    );
    let known = last_known_upstream(attached.path()).unwrap().unwrap();
    assert_eq!(known.sha, newer);
    assert_eq!(known.ref_name, origin.tracking_ref());

    // The kernel catches up, then the upstream is force-pushed and only the
    // human fetches the rewrite: kernel ref and tracking ref diverge.
    assert!(matches!(
        refresh_upstream(attached.path()).await,
        UpstreamRefresh::Fetched { .. }
    ));
    git(origin.path(), &["reset", "-q", "--hard", "HEAD~1"]);
    let rewritten = origin.commit("rewritten history");
    git(attached.path(), &["fetch", "-q", "origin"]);
    assert_eq!(
        git(attached.path(), &["rev-parse", &origin.tracking_ref()]),
        rewritten
    );
    let known = last_known_upstream(attached.path()).unwrap().unwrap();
    assert_eq!(known.sha, newer);
    assert_eq!(known.ref_name, kernel_ref(&origin));
}

/// Fetch failure and neither ref resolves: no upstream is known, the base is
/// HEAD. The same for a branch without upstream config and a detached HEAD.
#[tokio::test]
async fn nothing_known_of_the_upstream_resolves_head() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    git(
        attached.path(),
        &["update-ref", "-d", &origin.tracking_ref()],
    );
    break_origin(attached.path());
    assert!(matches!(
        refresh_upstream(attached.path()).await,
        UpstreamRefresh::Failed { .. }
    ));
    assert_eq!(last_known_upstream(attached.path()).unwrap(), None);
    let head = git(attached.path(), &["rev-parse", "HEAD"]);
    assert_eq!(
        choose_lease_start(attached.path()).unwrap(),
        LeaseStart::Head { sha: head }
    );

    let plain = attached_repo();
    assert_eq!(
        refresh_upstream(plain.path()).await,
        UpstreamRefresh::NoUpstream
    );
    assert_eq!(last_known_upstream(plain.path()).unwrap(), None);

    git(attached.path(), &["checkout", "-q", "--detach"]);
    assert_eq!(head_upstream(attached.path()).unwrap(), None);
    assert_eq!(
        refresh_upstream(attached.path()).await,
        UpstreamRefresh::NoUpstream
    );
}

/// HEAD's relation to its upstream decides the start: equal or behind → the
/// upstream; ahead → HEAD (which contains the upstream); diverged → refused,
/// with both commits and the counts.
#[test]
fn lease_start_follows_heads_relation_to_the_upstream() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    let equal = git(attached.path(), &["rev-parse", "HEAD"]);
    assert_eq!(
        choose_lease_start(attached.path()).unwrap(),
        LeaseStart::Upstream { sha: equal }
    );

    let behind_tip = origin.commit("landed upstream");
    git(attached.path(), &["fetch", "-q", "origin"]);
    assert_eq!(
        choose_lease_start(attached.path()).unwrap(),
        LeaseStart::Upstream { sha: behind_tip }
    );

    git(
        attached.path(),
        &["merge", "-q", "--ff-only", &origin.tracking_ref()],
    );
    let ahead = commit_locally(attached.path(), "unpushed.txt");
    assert_eq!(
        choose_lease_start(attached.path()).unwrap(),
        LeaseStart::Head { sha: ahead.clone() }
    );

    origin.commit("landed upstream meanwhile");
    let upstream_tip = origin.commit("and another");
    git(attached.path(), &["fetch", "-q", "origin"]);
    let LeaseStart::Diverged {
        head,
        upstream,
        ahead: ahead_count,
        behind,
    } = choose_lease_start(attached.path()).unwrap()
    else {
        panic!("a diverged checkout must be refused");
    };
    assert_eq!(head, ahead);
    assert_eq!(upstream.sha, upstream_tip);
    assert_eq!((ahead_count, behind), (1, 2));
    let refusal = diverged_refusal(attached.path(), &head, &upstream, ahead_count, behind);
    let crate::error::CalmError::Conflict(text) = &refusal else {
        panic!("the refusal is a client-class Conflict: {refusal:?}");
    };
    for needle in [
        "refused: attached-repo-diverged:",
        head.as_str(),
        upstream_tip.as_str(),
        "1 ahead, 2 behind",
        "Not retryable",
        "push, rebase or reset",
        "re-dispatch works afterwards",
    ] {
        assert!(text.contains(needle), "{needle:?} missing from {text}");
    }
}

/// A branch whose name another branch extends (`main` and `main-other`):
/// only HEAD's own upstream is read.
#[test]
fn head_upstream_is_the_exact_branch() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    git(
        attached.path(),
        &["branch", "-q", &format!("{}-other", origin.branch)],
    );
    assert_eq!(
        head_upstream(attached.path()).unwrap(),
        Some(origin.upstream())
    );
}

/// Every (remote, merge-ref) pair gets its own kernel ref — slashes, `.`,
/// URLs and refs outside `refs/heads/` included — and `git check-ref-format`
/// accepts every one.
#[test]
fn kernel_ref_is_total_distinct_and_valid() {
    let pairs = [
        ("origin", "refs/heads/main"),
        ("origin", "refs/heads/feat/x"),
        ("a/b", "refs/heads/main"),
        ("a", "b/refs/heads/main"),
        (".", "refs/heads/main"),
        ("https://example.test/r.git", "refs/heads/main"),
        ("origin", "refs/tags/v1"),
        ("origin", "refs/pull/1/head"),
        ("origin", "main"),
    ];
    let dir = attached_repo();
    let mut seen = std::collections::BTreeSet::new();
    for (remote, merge) in pairs {
        let kernel_ref = Upstream {
            remote: remote.into(),
            merge: merge.into(),
            tracking_ref: None,
        }
        .kernel_ref();
        assert!(kernel_ref.starts_with(KERNEL_UPSTREAM_REF_PREFIX));
        git(dir.path(), &["check-ref-format", &kernel_ref]);
        assert!(seen.insert(kernel_ref), "{remote} {merge} collides");
    }
}
