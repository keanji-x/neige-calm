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
//!   fetched again for [`FETCH_BACKOFF`]: an offline remote costs one timeout
//!   per minute, not one per worker.
//! - It is single-flight per (common dir, kernel ref — whose digest includes
//!   the effective remote URL): workers dispatched in parallel share one
//!   fetch and its outcome instead of racing for the ref lock and recording
//!   false failures.
//! - Its outcome is kept as fetch provenance ([`FetchProvenance`]): a success
//!   is an atomic receipt of the commit the fetch left, taken under the
//!   single-flight lock, and it alone makes the kernel's view authoritative
//!   ([`super::upstream::last_known_upstream`]).
//! - A loose `.lock` of the kernel ref older than [`UPSTREAM_FETCH_TIMEOUT`]
//!   (left by a fetch killed at the bound) is removed before the next fetch;
//!   the kernel is that ref's only writer.
//! - The local reads before the fetch run on a blocking thread.
//! - Every (remote, merge-ref) pair of a *named* remote (or `.`) is fetched.
//!   A URL in `branch.<b>.remote` is not: there is no named remote to fetch
//!   from; such a branch reads its remote-tracking ref, if any.
//!
//! **One kernel per attached repository.** Provenance and single-flight are
//! process-local, by design (simple first): this deployment runs exactly one
//! kernel against an attached repository. If two kernel processes did share
//! one, each keeps its own receipts, so one may hold a receipt the other's
//! later fetch has superseded — a stale but real upstream commit, never a
//! commit that was not fetched; it falls back to the human's tracking ref as
//! soon as that process's own next fetch fails, and is replaced by its next
//! successful one. Their fetches are not serialized with each other; a lost
//! ref lock is an ordinary failed fetch.
//!
//! **A success receipt does not age.** It stays authoritative until the next
//! fetch of its key records another outcome: a prepare re-driven without a
//! fresh `before_insert` (the driver's short-circuit of an existing op, or
//! recovery of a pending one) still trusts the last receipt, however old. A
//! restart clears every receipt.
//!
//! Any failure is a `warn!` and leaves the lease to the last known upstream.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
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
    /// The kernel ref now holds the upstream's current commit (this call's
    /// fetch, or a concurrent one for the same key it waited for).
    Fetched { kernel_ref: String },
    /// A fetch of this upstream failed less than [`FETCH_BACKOFF`] ago; no
    /// fetch was attempted.
    BackedOff { kernel_ref: String },
    /// The fetch failed or timed out (this call's, or a concurrent one it
    /// waited for); logged at `warn`.
    Failed { reason: String },
}

/// (git common dir, kernel ref): one upstream of one repository, as fetched
/// from one URL — the kernel ref's digest includes the effective remote URL
/// ([`Upstream::kernel_ref`]), so a URL change is a new key with no receipt.
pub(crate) type FetchKey = (PathBuf, String);

/// How the most recent kernel fetch of one key ended. A success is a
/// receipt: the commit the kernel ref held when the fetch finished, recorded
/// together with the outcome under the single-flight lock, so a reader never
/// pairs an outcome with a commit from another fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LastFetch {
    Succeeded { sha: String, at: Instant },
    Failed { at: Instant, reason: String },
}

#[derive(Default)]
struct FetchEntry {
    last: Option<LastFetch>,
    /// Bumped by every recorded outcome: a caller that waited on `flight`
    /// sees whether a fetch finished meanwhile.
    generation: u64,
    /// Held for the whole fetch: concurrent refreshes of one key run one
    /// fetch and share its outcome instead of racing for the ref lock. Every
    /// caller holds a clone from registration to return.
    flight: Arc<tokio::sync::Mutex<()>>,
}

/// The kernel's in-memory fetch provenance, per [`FetchKey`]: how the most
/// recent fetch ended (the receipt [`super::upstream::last_known_upstream`]
/// reads, and the [`FETCH_BACKOFF`] clock), and the single-flight lock. Not
/// persisted: after a restart there is no receipt, and the human's
/// remote-tracking ref is authoritative until the kernel's next successful
/// fetch. Process-local — see the module docs (one kernel per repository).
#[derive(Default)]
pub(crate) struct FetchProvenance {
    entries: Mutex<HashMap<FetchKey, FetchEntry>>,
}

impl FetchProvenance {
    /// The process-wide provenance production uses.
    pub(crate) fn global() -> &'static FetchProvenance {
        static GLOBAL: OnceLock<FetchProvenance> = OnceLock::new();
        GLOBAL.get_or_init(FetchProvenance::default)
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<FetchKey, FetchEntry>> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The commit the most recent kernel fetch of `key` left, when that fetch
    /// succeeded — the one case in which the kernel's view is authoritative.
    pub(crate) fn success_receipt(&self, key: &FetchKey) -> Option<(String, Instant)> {
        match self.entries().get(key).and_then(|entry| entry.last.clone()) {
            Some(LastFetch::Succeeded { sha, at }) => Some((sha, at)),
            _ => None,
        }
    }

    /// Whether the most recent kernel fetch of `key` succeeded.
    #[cfg(test)]
    pub(crate) fn last_fetch_succeeded(&self, key: &FetchKey) -> bool {
        self.success_receipt(key).is_some()
    }

    fn flight(&self, key: &FetchKey) -> (Arc<tokio::sync::Mutex<()>>, u64) {
        let mut entries = self.entries();
        let entry = entries.entry(key.clone()).or_default();
        (entry.flight.clone(), entry.generation)
    }

    fn snapshot(&self, key: &FetchKey) -> (u64, Option<LastFetch>) {
        let entries = self.entries();
        entries
            .get(key)
            .map(|entry| (entry.generation, entry.last.clone()))
            .unwrap_or_default()
    }

    fn record(&self, key: &FetchKey, last: LastFetch) {
        let mut entries = self.entries();
        let entry = entries.entry(key.clone()).or_default();
        entry.last = Some(last);
        entry.generation += 1;
    }

    /// Test-only: callers currently registered on `key` (running its fetch or
    /// waiting for it).
    #[cfg(test)]
    pub(crate) fn registered_callers(&self, key: &FetchKey) -> usize {
        self.entries()
            .get(key)
            .map_or(0, |entry| Arc::strong_count(&entry.flight) - 1)
    }

    /// Test-only: record a success receipt directly, as a fetch would.
    #[cfg(test)]
    pub(crate) fn record_success_for_test(&self, key: &FetchKey, sha: &str) {
        self.record(
            key,
            LastFetch::Succeeded {
                sha: sha.to_string(),
                at: Instant::now(),
            },
        );
    }
}

/// The clock the provenance's timestamps come from; injectable for tests.
pub(crate) type Clock<'a> = &'a (dyn Fn() -> Instant + Send + Sync);

/// Fetch HEAD's upstream into its kernel ref with the production bound,
/// provenance and clock. Never inside a database transaction.
pub(crate) async fn refresh_upstream(repo_root: &Path) -> UpstreamRefresh {
    refresh_upstream_with(
        repo_root,
        UPSTREAM_FETCH_TIMEOUT,
        FetchProvenance::global(),
        &Instant::now,
    )
    .await
}

/// What the fetch needs, read from the repository before it.
struct PreparedFetch {
    upstream: Upstream,
    kernel_ref: String,
    key: FetchKey,
}

/// The local reads before a fetch (upstream config, fetchability, common
/// dir); run on a blocking thread.
fn prepare_fetch(repo_root: &Path) -> std::result::Result<PreparedFetch, UpstreamRefresh> {
    let upstream = match head_upstream(repo_root) {
        Ok(Some(upstream)) => upstream,
        Ok(None) => return Err(UpstreamRefresh::NoUpstream),
        Err(error) => {
            return Err(failed(
                repo_root,
                format!("reading the upstream failed: {error}"),
            ));
        }
    };
    if let Some(reason) = not_fetchable(repo_root, &upstream) {
        tracing::debug!(repo_root = %repo_root.display(), reason, "upstream not fetched");
        return Err(UpstreamRefresh::NotFetched { reason });
    }
    let kernel_ref = upstream.kernel_ref();
    let common_dir = super::base::lease_git_common_dir(repo_root)
        .map_err(|error| failed(repo_root, format!("reading the common dir failed: {error}")))?;
    Ok(PreparedFetch {
        key: (common_dir, kernel_ref.clone()),
        upstream,
        kernel_ref,
    })
}

/// [`refresh_upstream`] with the bound, the provenance and the clock
/// injected. The clock is read at entry (the back-off check) and at the
/// fetch's completion (the recorded outcome).
pub(crate) async fn refresh_upstream_with(
    repo_root: &Path,
    bound: Duration,
    provenance: &FetchProvenance,
    clock: Clock<'_>,
) -> UpstreamRefresh {
    let root = repo_root.to_path_buf();
    let prepared = match tokio::task::spawn_blocking(move || prepare_fetch(&root)).await {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(outcome)) => return outcome,
        Err(error) => return failed(repo_root, format!("upstream read task failed: {error}")),
    };
    let PreparedFetch {
        upstream,
        kernel_ref,
        key,
    } = prepared;
    let (flight, generation) = provenance.flight(&key);
    let _in_flight = flight.lock().await;
    let (current, last) = provenance.snapshot(&key);
    if current != generation {
        // A fetch of this key finished while this call waited: share it.
        return match last {
            Some(LastFetch::Failed { reason, .. }) => UpstreamRefresh::Failed { reason },
            _ => UpstreamRefresh::Fetched { kernel_ref },
        };
    }
    if let Some(LastFetch::Failed { at, .. }) = &last
        && clock().saturating_duration_since(*at) < FETCH_BACKOFF
    {
        tracing::debug!(
            repo_root = %repo_root.display(),
            kernel_ref,
            "upstream fetch failed recently; backing off, the lease base is the last known upstream"
        );
        return UpstreamRefresh::BackedOff { kernel_ref };
    }
    let root = repo_root.to_path_buf();
    let destination = kernel_ref.clone();
    let cleared = tokio::task::spawn_blocking(move || {
        clear_stale_lock(&root, &destination)?;
        clear_symbolic_destination(&root, &destination)
    })
    .await
    .unwrap_or_else(|error| Err(format!("pre-fetch check task failed: {error}")));
    let result = match cleared {
        Ok(()) => fetch_into(repo_root, &upstream, &kernel_ref, bound).await,
        Err(reason) => Err(reason),
    };
    // The receipt is read while the single-flight lock is still held: no
    // other kernel fetch of this key can move the ref in between.
    let result = match result {
        Ok(()) => {
            let root = repo_root.to_path_buf();
            let ref_name = kernel_ref.clone();
            tokio::task::spawn_blocking(move || super::upstream::read_direct_ref(&root, &ref_name))
                .await
                .map_err(|error| format!("receipt read task failed: {error}"))
                .and_then(|read| read.map_err(|error| error.to_string()))
                .and_then(|sha| {
                    sha.ok_or_else(|| format!("fetched kernel ref {kernel_ref} does not resolve"))
                })
        }
        Err(reason) => Err(reason),
    };
    let at = clock();
    match result {
        Ok(sha) => {
            provenance.record(&key, LastFetch::Succeeded { sha, at });
            UpstreamRefresh::Fetched { kernel_ref }
        }
        Err(reason) => {
            provenance.record(
                &key,
                LastFetch::Failed {
                    at,
                    reason: reason.clone(),
                },
            );
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

/// A fetch killed at [`UPSTREAM_FETCH_TIMEOUT`] can leave the kernel ref's
/// loose `<ref>.lock` behind, and every later fetch would then fail to lock
/// the ref. The kernel is that ref's only writer and this runs under its
/// single-flight lock, so a lock file older than the timeout is a leftover:
/// removed. A younger one is left alone (the fetch then fails as git decides).
/// Located with `git rev-parse --git-path`, not a hard-coded layout.
fn clear_stale_lock(repo_root: &Path, kernel_ref: &str) -> std::result::Result<(), String> {
    let lock = format!("{kernel_ref}.lock");
    let output = git_output(repo_root, &["rev-parse", "--git-path", lock.as_str()])
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "git rev-parse --git-path {lock} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let path = super::base::printed_path(&output.stdout);
    let path = if path.is_absolute() {
        path
    } else {
        repo_root.join(path)
    };
    let modified = match std::fs::metadata(&path).and_then(|metadata| metadata.modified()) {
        Ok(modified) => modified,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("inspect {}: {error}", path.display())),
    };
    let age = std::time::SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default();
    if age < UPSTREAM_FETCH_TIMEOUT {
        return Ok(());
    }
    tracing::warn!(
        repo_root = %repo_root.display(),
        lock = %path.display(),
        age_secs = age.as_secs(),
        "removing a stale kernel upstream ref lock left by a killed fetch"
    );
    std::fs::remove_file(&path).map_err(|error| format!("remove {}: {error}", path.display()))
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
    // The kernel environment is inherited (hostile git variables stripped), not
    // an allowlist: credential helpers and ssh-agent need HOME / SSH_AUTH_SOCK
    // etc., as every `neige_git_command` caller relies on.
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
    let owned_track_id = track_id.to_string();
    let cwd = track.workspace.path.clone();
    let repo_root = match tokio::task::spawn_blocking(move || {
        super::git_repo_root_for_track_cwd(&owned_track_id, &cwd)
    })
    .await
    {
        Ok(Ok(repo_root)) => repo_root,
        Ok(Err(error)) => {
            tracing::warn!(track_id, %error, "upstream refresh could not resolve the repository");
            return;
        }
        Err(error) => {
            tracing::warn!(track_id, %error, "upstream refresh repository task failed");
            return;
        }
    };
    refresh_upstream(&repo_root).await;
}
