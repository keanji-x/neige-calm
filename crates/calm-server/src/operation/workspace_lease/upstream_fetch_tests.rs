//! The network half of an upstream lease base (#1777): the submit-path fetch
//! writes only its kernel ref, never prompts, is bounded, backs off after a
//! failure, and fetches every named-remote configuration.

use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use super::upstream::*;
use super::upstream_fetch::*;
use super::upstream_tests::{
    attach_origin, attached_repo, git, kernel_ref, transport_count, user_ref_state,
    witness_transport, witness_transport_then,
};

fn known_sha(repo: &Path) -> Option<String> {
    last_known_upstream(repo).unwrap().map(|known| known.sha)
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
    assert_eq!(known_sha(attached.path()), Some(tip));
}

/// A kernel ref planted as a symbolic ref to a branch: the fetch must not
/// write through it. The link is deleted, the branch stays where it was, and
/// the kernel ref ends up a plain ref at the upstream. Before the refresh,
/// the read ignores the symbolic kernel ref.
#[tokio::test]
async fn symbolic_kernel_ref_never_moves_the_branch_it_points_at() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    let victim_before = git(attached.path(), &["rev-parse", "HEAD"]);
    git(attached.path(), &["branch", "victim"]);
    let kernel = kernel_ref(&origin);
    git(
        attached.path(),
        &["symbolic-ref", &kernel, "refs/heads/victim"],
    );
    let tip = origin.commit("upstream moved");
    // The planted link is not read as the upstream.
    assert_eq!(
        last_known_upstream(attached.path())
            .unwrap()
            .unwrap()
            .ref_name,
        origin.tracking_ref()
    );

    assert!(matches!(
        refresh_upstream(attached.path()).await,
        UpstreamRefresh::Fetched { .. }
    ));

    assert_eq!(
        git(attached.path(), &["rev-parse", "refs/heads/victim"]),
        victim_before,
        "the fetch wrote through the symbolic kernel ref"
    );
    assert_eq!(git(attached.path(), &["rev-parse", &kernel]), tip);
    let still_symbolic = std::process::Command::new("git")
        .arg("-C")
        .arg(attached.path())
        .args(["symbolic-ref", "-q", &kernel])
        .status()
        .unwrap()
        .success();
    assert!(!still_symbolic);
}

/// A remote whose name has a `/` and a merge ref outside `refs/heads/` are
/// fetched like any other; a URL in `branch.<b>.remote` is not (no named
/// remote), and its lease reads no kernel ref.
#[tokio::test]
async fn every_named_remote_configuration_refreshes() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    let branch_key = |key: &str| format!("branch.{}.{key}", origin.branch);

    git(
        attached.path(),
        &[
            "remote",
            "add",
            "up/stream",
            origin.path().to_str().unwrap(),
        ],
    );
    git(
        attached.path(),
        &["config", &branch_key("remote"), "up/stream"],
    );
    let tip = origin.commit("upstream moved");
    let refresh = refresh_upstream(attached.path()).await;
    assert!(
        matches!(refresh, UpstreamRefresh::Fetched { .. }),
        "{refresh:?}"
    );
    assert_eq!(known_sha(attached.path()), Some(tip.clone()));

    git(origin.path(), &["update-ref", "refs/review/42", &tip]);
    let review = origin.commit("past the review ref");
    git(origin.path(), &["update-ref", "refs/review/42", &review]);
    git(
        attached.path(),
        &["config", &branch_key("remote"), "origin"],
    );
    git(
        attached.path(),
        &["config", &branch_key("merge"), "refs/review/42"],
    );
    let refresh = refresh_upstream(attached.path()).await;
    assert!(
        matches!(refresh, UpstreamRefresh::Fetched { .. }),
        "{refresh:?}"
    );
    assert_eq!(known_sha(attached.path()), Some(review));

    git(
        attached.path(),
        &[
            "config",
            &branch_key("remote"),
            origin.path().to_str().unwrap(),
        ],
    );
    let refresh = refresh_upstream(attached.path()).await;
    assert!(
        matches!(refresh, UpstreamRefresh::NotFetched { .. }),
        "{refresh:?}"
    );
}

/// One HTTP endpoint on loopback that answers every request `401` with a
/// Basic challenge: a remote that demands credentials. Returns its URL.
fn credential_demanding_endpoint() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&buf[..n]),
                }
            }
            let _ = stream.write_all(
                b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"neige\"\r\n\
                  Content-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    format!("http://127.0.0.1:{port}/repo.git")
}

/// A remote that demands credentials fails the fetch fast and prompts
/// nobody: the repository's configured `core.askPass` — a script counting
/// its invocations — is never run, because `GIT_ASKPASS` outranks it.
#[tokio::test]
async fn credential_demand_fails_fast_without_prompting() {
    let attached = attached_repo();
    let _origin = attach_origin(attached.path());
    git(
        attached.path(),
        &[
            "remote",
            "set-url",
            "origin",
            &credential_demanding_endpoint(),
        ],
    );
    let prompts = attached.path().join("askpass-invocations");
    let askpass = attached.path().join("askpass.sh");
    std::fs::write(
        &askpass,
        format!(
            "#!/bin/sh\necho prompt >> '{}'\necho secret\n",
            prompts.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        &askpass,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    git(
        attached.path(),
        &["config", "core.askPass", askpass.to_str().unwrap()],
    );
    let started = Instant::now();

    let refresh = refresh_upstream_with(
        attached.path(),
        Duration::from_secs(15),
        &FetchProvenance::default(),
        &Instant::now,
    )
    .await;

    let UpstreamRefresh::Failed { reason } = refresh else {
        panic!("a credential demand must fail the fetch, got {refresh:?}");
    };
    assert!(!reason.contains("timed out"), "{reason}");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(
        !prompts.exists(),
        "the configured askpass prompted: {:?}",
        std::fs::read_to_string(&prompts)
    );
}

/// A clock that returns the given instants in order, repeating the last.
fn ticks(times: Vec<Instant>) -> impl Fn() -> Instant + Send + Sync {
    let queue = std::sync::Mutex::new(std::collections::VecDeque::from(times));
    move || {
        let mut queue = queue.lock().unwrap();
        if queue.len() > 1 {
            queue.pop_front().unwrap()
        } else {
            *queue.front().unwrap()
        }
    }
}

/// After a failed fetch the same upstream is not fetched again for
/// [`FETCH_BACKOFF`]; past it, it is; a success clears the entry. A failure is
/// recorded at the fetch's COMPLETION: a slow failure that started at 61 s
/// and ended at 100 s backs off until 160 s, not 121 s. The clock is
/// injected; the transport witness counts real fetch attempts.
#[tokio::test]
async fn a_failed_fetch_backs_off_and_a_success_clears_it() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    let failing = witness_transport_then(attached.path(), "exit 1");
    let provenance = FetchProvenance::default();
    let bound = Duration::from_secs(15);
    let t0 = Instant::now();
    let at = |secs: u64| t0 + Duration::from_secs(secs);
    let refresh = |clock: Box<dyn Fn() -> Instant + Send + Sync>| {
        let provenance = &provenance;
        let path = attached.path().to_path_buf();
        async move { refresh_upstream_with(&path, bound, provenance, &*clock).await }
    };

    assert!(matches!(
        refresh(Box::new(move || at(0))).await,
        UpstreamRefresh::Failed { .. }
    ));
    assert_eq!(transport_count(&failing), 1);
    assert_eq!(
        refresh(Box::new(move || at(30))).await,
        UpstreamRefresh::BackedOff {
            kernel_ref: kernel_ref(&origin)
        }
    );
    assert_eq!(transport_count(&failing), 1, "backed off: no fetch");
    // Past the back-off at entry (61 s); the slow failure completes at 100 s.
    assert!(matches!(
        refresh(Box::new(ticks(vec![at(61), at(100)]))).await,
        UpstreamRefresh::Failed { .. }
    ));
    assert_eq!(transport_count(&failing), 2, "past the back-off: fetched");
    assert!(
        matches!(
            refresh(Box::new(move || at(130))).await,
            UpstreamRefresh::BackedOff { .. }
        ),
        "backed off from the failure's completion (100 s), not its start (61 s)"
    );
    assert_eq!(transport_count(&failing), 2);

    let serving = witness_transport(attached.path());
    let tip = origin.commit("upstream moved");
    assert!(matches!(
        refresh(Box::new(move || at(161))).await,
        UpstreamRefresh::Fetched { .. }
    ));
    assert_eq!(transport_count(&serving), 1);
    assert_eq!(
        git(attached.path(), &["rev-parse", &kernel_ref(&origin)]),
        tip
    );

    // The success cleared the entry: the next failure is attempted at once.
    let failing_again = witness_transport_then(attached.path(), "exit 1");
    assert!(matches!(
        refresh(Box::new(move || at(162))).await,
        UpstreamRefresh::Failed { .. }
    ));
    assert_eq!(transport_count(&failing_again), 1);
}

/// Single-flight shares only a fetch that is in flight: a caller arriving
/// after a fetch finished starts a new one.
#[tokio::test]
async fn a_caller_after_a_finished_fetch_fetches_again() {
    let attached = attached_repo();
    let _origin = attach_origin(attached.path());
    let serving = witness_transport(attached.path());
    let provenance = FetchProvenance::default();
    for expected in 1..=2 {
        assert!(matches!(
            refresh_upstream_with(
                attached.path(),
                Duration::from_secs(15),
                &provenance,
                &Instant::now
            )
            .await,
            UpstreamRefresh::Fetched { .. }
        ));
        assert_eq!(transport_count(&serving), expected);
    }
}

/// The kernel ref's loose lock path, as git names it.
fn kernel_ref_lock(repo: &Path, kernel_ref: &str) -> std::path::PathBuf {
    let printed = git(
        repo,
        &["rev-parse", "--git-path", &format!("{kernel_ref}.lock")],
    );
    let path = std::path::PathBuf::from(printed);
    if path.is_absolute() {
        path
    } else {
        repo.join(path)
    }
}

/// A fetch killed at the bound can leave `<kernel ref>.lock`; one older than
/// the fetch timeout is removed and the fetch succeeds. A fresh lock (a live
/// writer's, as far as the kernel can tell) is left alone and the fetch fails.
#[tokio::test]
async fn a_stale_kernel_ref_lock_is_cleared_and_a_fresh_one_is_not() {
    let attached = attached_repo();
    let origin = attach_origin(attached.path());
    let kernel = kernel_ref(&origin);
    let lock = kernel_ref_lock(attached.path(), &kernel);
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();

    std::fs::write(&lock, "").unwrap();
    let old = std::process::Command::new("touch")
        .args(["-d", "1 hour ago"])
        .arg(&lock)
        .status()
        .unwrap();
    assert!(old.success());
    let tip = origin.commit("upstream moved");
    let refresh = refresh_upstream_with(
        attached.path(),
        Duration::from_secs(15),
        &FetchProvenance::default(),
        &Instant::now,
    )
    .await;
    assert!(
        matches!(refresh, UpstreamRefresh::Fetched { .. }),
        "{refresh:?}"
    );
    assert!(!lock.exists(), "the stale lock was removed");
    assert_eq!(git(attached.path(), &["rev-parse", &kernel]), tip);

    std::fs::write(&lock, "").unwrap();
    origin.commit("moved again");
    let refresh = refresh_upstream_with(
        attached.path(),
        Duration::from_secs(15),
        &FetchProvenance::default(),
        &Instant::now,
    )
    .await;
    let UpstreamRefresh::Failed { reason } = refresh else {
        panic!("a fresh lock must fail the fetch, got {refresh:?}");
    };
    assert!(reason.contains("lock"), "{reason}");
    assert!(lock.exists(), "a fresh lock is left alone");
    assert_eq!(git(attached.path(), &["rev-parse", &kernel]), tip);
    std::fs::remove_file(&lock).unwrap();
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
    let started = Instant::now();

    let refresh = refresh_upstream_with(
        attached.path(),
        Duration::from_millis(1500),
        &FetchProvenance::default(),
        &Instant::now,
    )
    .await;

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
        known_sha(attached.path()),
        Some(git(attached.path(), &["rev-parse", &origin.tracking_ref()]))
    );
}

/// The fetch argv and environment, pinned: one force refspec into the kernel
/// ref, every flag that keeps the user's refs out of it, and every variable
/// that keeps a prompt out of it.
#[test]
fn fetch_args_and_env_are_pinned() {
    let upstream = Upstream {
        remote: "origin".into(),
        merge: "refs/heads/main".into(),
        url: "/srv/origin.git".into(),
        tracking_ref: None,
    };
    let kernel = upstream.kernel_ref();
    assert_eq!(
        fetch_args(&upstream, &kernel),
        [
            "fetch".to_string(),
            "--quiet".into(),
            "--no-write-fetch-head".into(),
            "--refmap=".into(),
            "--no-tags".into(),
            "--no-prune".into(),
            "--no-recurse-submodules".into(),
            "--no-auto-maintenance".into(),
            "origin".into(),
            format!("+refs/heads/main:{kernel}"),
        ]
    );
    assert_eq!(
        FETCH_ENV,
        [
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_ASKPASS", "/bin/false"),
            ("SSH_ASKPASS", "/bin/false"),
            ("SSH_ASKPASS_REQUIRE", "force"),
        ]
    );
}
