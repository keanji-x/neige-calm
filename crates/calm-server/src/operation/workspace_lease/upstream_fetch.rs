//! The attached repository's upstream as a lease base (#1777): the network
//! half.
//!
//! [`refresh_upstream`] runs a bounded `git fetch` of HEAD's one upstream into
//! the kernel-owned ref [`Upstream::kernel_ref`], on the worker-op submit path
//! before the op row is inserted
//! ([`crate::operation::ProviderAdapter::before_insert`]) — never inside a
//! transaction; the prepare transaction then reads it locally
//! ([`super::upstream::choose_lease_start`]).
//!
//! - It writes no ref of the user's: no `refs/remotes/*` (`--refmap=` turns off
//!   the opportunistic remote-tracking update), no `FETCH_HEAD`
//!   (`--no-write-fetch-head`), no tags (`--no-tags`); a kernel ref that is
//!   symbolic is deleted (`update-ref --no-deref -d`) before the fetch, so the
//!   fetch cannot write through it into a branch. Objects land in the shared
//!   object store, nothing else.
//! - It never prompts: `GIT_TERMINAL_PROMPT=0`, and `GIT_ASKPASS` /
//!   `SSH_ASKPASS` are `/bin/false` (with `SSH_ASKPASS_REQUIRE=force`), which
//!   outrank a configured `core.askPass`. Credential helpers and ssh-agent keys
//!   still work: they do not prompt.
//! - It is bounded ([`UPSTREAM_FETCH_TIMEOUT`], the fetch's whole process
//!   group killed past it), and a repository whose fetch just failed is not
//!   fetched again for [`FETCH_BACKOFF`] ([`FetchBackoff`]): an offline remote
//!   costs one timeout per minute, not one per worker.
//! - Every (remote, merge-ref) pair of a *named* remote (or `.`) is fetched.
//!   A URL in `branch.<b>.remote` is not: there is no named remote to fetch
//!   from; such a branch reads its remote-tracking ref, if any.
//!
//! Any failure is a `warn!` and leaves the lease to the last known upstream.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::upstream::{Upstream, git_output, head_upstream};
use crate::model::TrackWorkspaceKind;
use crate::plugin_host::child_process::{
    ChildFinishError, SpawnTimedOut, finish_within, read_capped, set_process_group_leader,
    spawn_within,
};
use crate::workspace_materialize::neige_git_command;

/// The hard bound on one upstream fetch, spawn to reap.
pub(crate) const UPSTREAM_FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a failed fetch keeps the same (repository, kernel ref) from being
/// fetched again.
pub(crate) const FETCH_BACKOFF: Duration = Duration::from_secs(60);

/// Stderr kept for the warning a failed fetch logs; the rest is drained unread.
const FETCH_OUTPUT_CAP: usize = 64 * 1024;

/// The environment every upstream fetch runs with, on top of
/// `neige_git_command()`'s hostile-variable strip: nothing may prompt.
pub(crate) const FETCH_ENV: [(&str, &str); 4] = [
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_ASKPASS", "/bin/false"),
    ("SSH_ASKPASS", "/bin/false"),
    ("SSH_ASKPASS_REQUIRE", "force"),
];

/// What one [`refresh_upstream`] did. Informational: every arm leaves the
/// lease to [`super::upstream::last_known_upstream`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UpstreamRefresh {
    /// HEAD has no upstream (detached, unborn, or no config): nothing to fetch.
    NoUpstream,
    /// The upstream is not one the kernel fetches (a URL remote, or a config
    /// value git itself would not accept); the remote-tracking ref, if any, is
    /// what the lease reads.
    NotFetched { reason: String },
    /// The kernel ref now holds the upstream's current commit.
    Fetched { kernel_ref: String },
    /// A fetch of this upstream failed less than [`FETCH_BACKOFF`] ago; no
    /// fetch was attempted.
    BackedOff { kernel_ref: String },
    /// The fetch failed or timed out; logged at `warn`.
    Failed { reason: String },
}

/// The in-memory negative cache: (git common dir, kernel ref) → when its last
/// fetch failed. A success clears the entry.
#[derive(Default)]
pub(crate) struct FetchBackoff {
    failed_at: Mutex<HashMap<(PathBuf, String), Instant>>,
}

impl FetchBackoff {
    /// The process-wide cache production uses.
    pub(crate) fn global() -> &'static FetchBackoff {
        static GLOBAL: OnceLock<FetchBackoff> = OnceLock::new();
        GLOBAL.get_or_init(FetchBackoff::default)
    }

    fn backing_off(&self, key: &(PathBuf, String), now: Instant) -> bool {
        let mut failed_at = self.failed_at.lock().unwrap_or_else(|p| p.into_inner());
        match failed_at.get(key) {
            Some(at) if now.saturating_duration_since(*at) < FETCH_BACKOFF => true,
            Some(_) => {
                failed_at.remove(key);
                false
            }
            None => false,
        }
    }

    fn record(&self, key: (PathBuf, String), outcome: std::result::Result<(), Instant>) {
        let mut failed_at = self.failed_at.lock().unwrap_or_else(|p| p.into_inner());
        match outcome {
            Ok(()) => {
                failed_at.remove(&key);
            }
            Err(at) => {
                failed_at.insert(key, at);
            }
        }
    }
}

/// Fetch HEAD's upstream into its kernel ref with the production bound, cache
/// and clock. Never inside a database transaction.
pub(crate) async fn refresh_upstream(repo_root: &Path) -> UpstreamRefresh {
    refresh_upstream_with(
        repo_root,
        UPSTREAM_FETCH_TIMEOUT,
        FetchBackoff::global(),
        Instant::now(),
    )
    .await
}

/// [`refresh_upstream`] with the bound, the negative cache and the clock
/// injected.
pub(crate) async fn refresh_upstream_with(
    repo_root: &Path,
    bound: Duration,
    backoff: &FetchBackoff,
    now: Instant,
) -> UpstreamRefresh {
    let upstream = match head_upstream(repo_root) {
        Ok(Some(upstream)) => upstream,
        Ok(None) => return UpstreamRefresh::NoUpstream,
        Err(error) => return failed(repo_root, format!("reading the upstream failed: {error}")),
    };
    if let Some(reason) = not_fetchable(repo_root, &upstream) {
        tracing::debug!(repo_root = %repo_root.display(), reason, "upstream not fetched");
        return UpstreamRefresh::NotFetched { reason };
    }
    let kernel_ref = upstream.kernel_ref();
    let common_dir = match super::base::lease_git_common_dir(repo_root) {
        Ok(common_dir) => common_dir,
        Err(error) => return failed(repo_root, format!("reading the common dir failed: {error}")),
    };
    let key = (common_dir, kernel_ref.clone());
    if backoff.backing_off(&key, now) {
        tracing::debug!(
            repo_root = %repo_root.display(),
            kernel_ref,
            "upstream fetch failed recently; backing off, the lease base is the last known upstream"
        );
        return UpstreamRefresh::BackedOff { kernel_ref };
    }
    let result = match clear_symbolic_destination(repo_root, &kernel_ref) {
        Ok(()) => fetch_into(repo_root, &upstream, &kernel_ref, bound).await,
        Err(reason) => Err(reason),
    };
    match result {
        Ok(()) => {
            backoff.record(key, Ok(()));
            UpstreamRefresh::Fetched { kernel_ref }
        }
        Err(reason) => {
            backoff.record(key, Err(now));
            failed(repo_root, reason)
        }
    }
}

fn failed(repo_root: &Path, reason: String) -> UpstreamRefresh {
    tracing::warn!(
        repo_root = %repo_root.display(),
        reason,
        "upstream refresh failed; the lease base is the last known upstream (no retry for {FETCH_BACKOFF:?})"
    );
    UpstreamRefresh::Failed { reason }
}

/// Why this upstream is not fetched, if it is not: a remote that is neither
/// `.` nor a configured remote name (a URL — out of scope), one git would read
/// as an option, or a merge ref / kernel ref `git check-ref-format` refuses.
fn not_fetchable(repo_root: &Path, upstream: &Upstream) -> Option<String> {
    if upstream.remote.starts_with('-') {
        return Some(format!("remote {:?} reads as an option", upstream.remote));
    }
    if upstream.remote != "." {
        let key = format!("remote.{}.url", upstream.remote);
        let named = git_output(repo_root, &["config", "--get", key.as_str()])
            .is_ok_and(|output| output.status.success());
        if !named {
            return Some(format!(
                "branch remote {:?} is not a named remote (a URL is not fetched)",
                upstream.remote
            ));
        }
    }
    let kernel_ref = upstream.kernel_ref();
    for (name, flags) in [
        (upstream.merge.as_str(), &["--allow-onelevel"][..]),
        (kernel_ref.as_str(), &[][..]),
    ] {
        let mut args = vec!["check-ref-format"];
        args.extend_from_slice(flags);
        args.push(name);
        let valid = git_output(repo_root, &args).is_ok_and(|output| output.status.success());
        if !valid {
            return Some(format!("{name:?} is not a valid ref name"));
        }
    }
    None
}

/// A kernel ref that is symbolic would make the fetch write through it into
/// whatever it points at (a branch of the user's). Deleted — the link, not
/// its target — before the fetch.
fn clear_symbolic_destination(
    repo_root: &Path,
    kernel_ref: &str,
) -> std::result::Result<(), String> {
    let symbolic = git_output(repo_root, &["symbolic-ref", "-q", kernel_ref])
        .map_err(|error| error.to_string())?;
    if !symbolic.status.success() {
        return Ok(());
    }
    tracing::warn!(
        repo_root = %repo_root.display(),
        kernel_ref,
        "kernel upstream ref is symbolic; deleting the link before fetching"
    );
    let deleted = git_output(repo_root, &["update-ref", "--no-deref", "-d", kernel_ref])
        .map_err(|error| error.to_string())?;
    if deleted.status.success() {
        Ok(())
    } else {
        Err(format!(
            "could not delete symbolic kernel ref {kernel_ref}: {}",
            String::from_utf8_lossy(&deleted.stderr).trim()
        ))
    }
}

/// The fetch argv after `git -C <repo_root>`: one refspec, force-updating the
/// kernel ref only.
pub(crate) fn fetch_args(upstream: &Upstream, kernel_ref: &str) -> Vec<String> {
    vec![
        "fetch".into(),
        "--quiet".into(),
        "--no-write-fetch-head".into(),
        // No opportunistic `refs/remotes/<remote>/*` update for the refspec.
        "--refmap=".into(),
        "--no-tags".into(),
        "--no-prune".into(),
        "--no-recurse-submodules".into(),
        "--no-auto-maintenance".into(),
        upstream.remote.clone(),
        format!("+{}:{kernel_ref}", upstream.merge),
    ]
}

/// Run the fetch as its own process group with stdin closed and
/// [`FETCH_ENV`]; past `bound` the group is killed (`kill_on_drop` + the
/// [`GroupChild`] sweep — the slice-4 sampler's precedent).
///
/// [`GroupChild`]: crate::plugin_host::child_process::GroupChild
async fn fetch_into(
    repo_root: &Path,
    upstream: &Upstream,
    kernel_ref: &str,
    bound: Duration,
) -> std::result::Result<(), String> {
    let deadline = tokio::time::Instant::now() + bound;
    let timed_out = || format!("git fetch {} timed out after {bound:?}", upstream.remote);
    let mut std_command = neige_git_command();
    std_command
        .arg("-C")
        .arg(repo_root)
        .args(fetch_args(upstream, kernel_ref))
        .envs(FETCH_ENV);
    let mut command = tokio::process::Command::from(std_command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    set_process_group_leader(&mut command);
    let mut child = match spawn_within(command, deadline).await {
        Ok(Ok(child)) => child,
        Ok(Err(error)) => return Err(format!("git fetch could not be spawned: {error}")),
        Err(SpawnTimedOut) => return Err(timed_out()),
    };
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout(), child.stderr()) else {
        return Err("git fetch output pipes missing".into());
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let finished = finish_within(
        deadline,
        async {
            let (o, e) = tokio::join!(
                read_capped(&mut stdout, FETCH_OUTPUT_CAP, &mut out),
                read_capped(&mut stderr, FETCH_OUTPUT_CAP, &mut err),
            );
            o?;
            e?;
            Ok::<(), std::io::Error>(())
        },
        child.wait_and_release_group(),
    )
    .await;
    let (status, released) = match finished {
        Ok(value) => value,
        Err(ChildFinishError::Drain(error)) => {
            return Err(format!("git fetch output could not be read: {error}"));
        }
        Err(ChildFinishError::TimedOut) => return Err(timed_out()),
    };
    released.sweep();
    let status = status.map_err(|error| format!("git fetch could not be reaped: {error}"))?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&err[..err.len().min(FETCH_OUTPUT_CAP)]);
        return Err(format!(
            "git fetch {} {} exited {status}: {}",
            upstream.remote,
            upstream.merge,
            stderr.trim()
        ));
    }
    Ok(())
}

/// The submit-path half for a worker op: resolve the Track's repository and
/// refresh its upstream, outside every transaction. A managed workspace has
/// no upstream of the user's to follow and is skipped; any failure to find
/// the repository is logged and left to the prepare transaction, which
/// reports it properly.
pub(crate) async fn refresh_track_upstream(repo: &dyn crate::db::RouteRepo, track_id: &str) {
    let track = match repo.track_get(track_id).await {
        Ok(Some(track)) => track,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(track_id, %error, "upstream refresh could not read the track");
            return;
        }
    };
    if track.workspace.kind != TrackWorkspaceKind::Attached {
        return;
    }
    let repo_root = match super::git_repo_root_for_track_cwd(track_id, &track.workspace.path) {
        Ok(repo_root) => repo_root,
        Err(error) => {
            tracing::warn!(track_id, %error, "upstream refresh could not resolve the repository");
            return;
        }
    };
    refresh_upstream(&repo_root).await;
}
