//! The upstream half of a lease base (#1777): the fetch writes only the
//! kernel ref, is bounded, and every failure leaves the last known upstream —
//! kernel ref, then remote-tracking ref, then HEAD.
//!
//! The fixtures here are shared with the worker adapters' tests, which drive
//! the same repositories through `before_insert` and `prepare_tx`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::upstream::*;

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
    format!("refs/neige/upstream/origin/{}", origin.branch)
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

fn attached_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    std::fs::write(dir.path().join("README.md"), "initial\n").unwrap();
    git(dir.path(), &["add", "README.md"]);
    git(dir.path(), &["commit", "-q", "-m", "initial"]);
    dir
}

/// The fetch writes the kernel ref and nothing of the user's: not
/// `refs/remotes/*` (which `git fetch <remote> <refspec>` updates
/// opportunistically without `--refmap=`), not `FETCH_HEAD`, not the tag the
/// upstream carries, and `git status` reads as before.
#[tokio::test]
async fn fetch_writes_only_the_kernel_ref() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    git(origin.path(), &["tag", "v-upstream"]);
    let tip = origin.commit("upstream moved");
    let before = user_ref_state(attached.path());

    let refresh = refresh_upstream(attached.path()).await;

    assert_eq!(
        refresh,
        UpstreamRefresh::Fetched {
            kernel_ref: kernel_ref(&origin)
        }
    );
    assert_eq!(
        git(attached.path(), &["rev-parse", &kernel_ref(&origin)]),
        tip
    );
    assert_eq!(
        user_ref_state(attached.path()),
        before,
        "the fetch must not write refs/remotes/*, tags or FETCH_HEAD"
    );
    assert_eq!(
        last_known_upstream(attached.path()).unwrap().as_deref(),
        Some(tip.as_str())
    );
}

/// Fetch failure with a kernel ref from an earlier fetch: the base is that
/// kernel ref, even though the remote-tracking ref is older still.
#[tokio::test]
async fn failed_fetch_resolves_the_kernel_ref() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    let tracking = git(
        attached.path(),
        &[
            "rev-parse",
            &format!("refs/remotes/origin/{}", origin.branch),
        ],
    );
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
    assert_eq!(
        last_known_upstream(attached.path()).unwrap().as_deref(),
        Some(fetched.as_str())
    );
}

/// Fetch failure and no kernel ref yet: the base is the repository's own
/// remote-tracking ref, read and not written.
#[tokio::test]
async fn failed_fetch_without_kernel_ref_resolves_the_tracking_ref() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    origin.commit("upstream moved");
    git(attached.path(), &["fetch", "-q", "origin"]);
    let tracking = git(
        attached.path(),
        &[
            "rev-parse",
            &format!("refs/remotes/origin/{}", origin.branch),
        ],
    );
    let head = git(attached.path(), &["rev-parse", "HEAD"]);
    assert_ne!(tracking, head, "HEAD lags its upstream");
    break_origin(attached.path());

    assert!(matches!(
        refresh_upstream(attached.path()).await,
        UpstreamRefresh::Failed { .. }
    ));

    assert_eq!(
        last_known_upstream(attached.path()).unwrap().as_deref(),
        Some(tracking.as_str())
    );
}

/// Fetch failure and neither ref resolves: no upstream is known, the base is
/// HEAD. The same for a branch without upstream config and a detached HEAD.
#[tokio::test]
async fn nothing_known_of_the_upstream_resolves_head() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    git(
        attached.path(),
        &[
            "update-ref",
            "-d",
            &format!("refs/remotes/origin/{}", origin.branch),
        ],
    );
    break_origin(attached.path());
    assert!(matches!(
        refresh_upstream(attached.path()).await,
        UpstreamRefresh::Failed { .. }
    ));
    assert_eq!(last_known_upstream(attached.path()).unwrap(), None);

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

/// A branch whose name another branch extends (`main` and `main/x`): only
/// HEAD's own upstream is read.
#[test]
fn head_upstream_is_the_exact_branch() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    git(
        attached.path(),
        &["branch", "-q", &format!("{}-other", origin.branch)],
    );
    let upstream = head_upstream(attached.path()).unwrap().unwrap();
    assert_eq!(upstream.remote, "origin");
    assert_eq!(upstream.merge, format!("refs/heads/{}", origin.branch));
    assert_eq!(
        upstream.tracking_ref,
        Some(format!("refs/remotes/origin/{}", origin.branch))
    );
}

#[test]
fn kernel_ref_only_for_a_plain_remote_branch() {
    let upstream = |remote: &str, merge: &str| Upstream {
        remote: remote.into(),
        merge: merge.into(),
        tracking_ref: None,
    };
    assert_eq!(
        upstream("origin", "refs/heads/main")
            .kernel_ref()
            .as_deref(),
        Some("refs/neige/upstream/origin/main")
    );
    assert_eq!(
        upstream("up-stream_2", "refs/heads/feat/x")
            .kernel_ref()
            .as_deref(),
        Some("refs/neige/upstream/up-stream_2/feat/x")
    );
    for (remote, merge) in [
        (".", "refs/heads/main"),
        ("https://example.test/r.git", "refs/heads/main"),
        ("a/b", "refs/heads/main"),
        ("-oops", "refs/heads/main"),
        ("origin", "refs/tags/v1"),
        ("origin", "refs/heads/a:b"),
        ("origin", "refs/heads/a..b"),
        ("origin", "refs/heads/"),
    ] {
        assert_eq!(
            upstream(remote, merge).kernel_ref(),
            None,
            "{remote} {merge}"
        );
    }
}

/// Live processes whose command line names `needle`, zombies excluded.
fn processes_naming(needle: &str) -> Vec<i32> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let zombie = std::fs::read_to_string(entry.path().join("stat"))
            .map(|stat| {
                stat.rsplit_once(')')
                    .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
            })
            .unwrap_or(true);
        if !zombie && String::from_utf8_lossy(&cmdline).contains(needle) {
            found.push(pid);
        }
    }
    found
}

/// A fetch that hangs is bounded: it ends as `Failed` at the bound, and the
/// whole process group — the transport command git forked, not only git —
/// is gone. The hang is the local transport's `uploadpack`, run through the
/// shell, sleeping far past the bound.
#[tokio::test]
async fn hanging_fetch_is_killed_at_the_bound() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    origin.commit("never arrives");
    let needle = "sleep 91.7177";
    git(
        attached.path(),
        &[
            "config",
            "remote.origin.uploadpack",
            &format!("{needle}; git-upload-pack"),
        ],
    );
    let started = std::time::Instant::now();

    let refresh = refresh_upstream_within(attached.path(), Duration::from_millis(1500)).await;

    let UpstreamRefresh::Failed { reason } = refresh else {
        panic!("a hanging fetch must fail at the bound, got {refresh:?}");
    };
    assert!(reason.contains("timed out"), "{reason}");
    assert!(started.elapsed() < Duration::from_secs(15));
    let mut live = processes_naming(needle);
    for _ in 0..100 {
        if live.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        live = processes_naming(needle);
    }
    for pid in &live {
        // SAFETY: pids just observed; killed only so a failing assertion
        // leaves no 90-second sleep behind.
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
    assert!(live.is_empty(), "the fetch's transport survived: {live:?}");
    assert_eq!(
        last_known_upstream(attached.path()).unwrap(),
        Some(git(
            attached.path(),
            &[
                "rev-parse",
                &format!("refs/remotes/origin/{}", origin.branch)
            ]
        ))
    );
}

/// The fetch argv, pinned: one force refspec into the kernel ref and every
/// flag that keeps the user's refs and prompts out of it.
#[test]
fn fetch_args_are_pinned() {
    let upstream = Upstream {
        remote: "origin".into(),
        merge: "refs/heads/main".into(),
        tracking_ref: None,
    };
    assert_eq!(
        fetch_args(&upstream, "refs/neige/upstream/origin/main"),
        [
            "fetch",
            "--quiet",
            "--no-write-fetch-head",
            "--refmap=",
            "--no-tags",
            "--no-prune",
            "--no-recurse-submodules",
            "--no-auto-maintenance",
            "origin",
            "+refs/heads/main:refs/neige/upstream/origin/main",
        ]
    );
}
