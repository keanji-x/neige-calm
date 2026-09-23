//! The attached repository's upstream as a lease base (#1777).
//!
//! An attached repository is a human's working copy: its HEAD sits wherever
//! that human left it, usually behind the branch's upstream, so a lease based
//! on HEAD starts stale and gets staler as other work lands upstream. When the
//! branch HEAD is on has a configured upstream (`branch.<b>.remote` +
//! `branch.<b>.merge`), a lease starts from that upstream instead
//! ([`super::base::BaseSource::Upstream`]); without one it keeps HEAD
//! (`BaseSource::Head`). Which path was taken is written to `base_source`.
//!
//! Two halves, split by the kernel's one write transaction:
//!
//! - [`refresh_upstream`] — the network half. A bounded `git fetch` of that one
//!   upstream into the kernel-owned ref [`Upstream::kernel_ref`]
//!   (`refs/neige/upstream/<remote>/<branch>`), run on the worker-op submit
//!   path before the op row is inserted ([`crate::operation::ProviderAdapter::before_insert`]),
//!   never inside a transaction. It writes no ref of the user's: no
//!   `refs/remotes/*` (`--refmap=` turns off the opportunistic remote-tracking
//!   update), no `FETCH_HEAD` (`--no-write-fetch-head`), no tags (`--no-tags`).
//!   Only objects land in the shared object store. A failure (timeout,
//!   offline, auth) is a `warn!`; the base then comes from the last known
//!   upstream.
//! - [`last_known_upstream`] — the local half, run inside the prepare
//!   transaction and by the `plan.list` read: `rev-parse` of the kernel ref,
//!   else of the repository's own remote-tracking ref for that upstream (read,
//!   never written). Both are "the upstream as last known"; neither resolving
//!   means the base is HEAD.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use crate::error::{CalmError, Result};
use crate::model::TrackWorkspaceKind;
use crate::plugin_host::child_process::{
    ChildFinishError, SpawnTimedOut, finish_within, read_capped, set_process_group_leader,
    spawn_within,
};
use crate::workspace_materialize::neige_git_command;

/// The hard bound on one upstream fetch, spawn to reap. Past it the fetch's
/// whole process group (the `git fetch` and every transport helper it forked)
/// is killed and the lease falls back to the last known upstream.
pub(crate) const UPSTREAM_FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// Stderr kept for the warning a failed fetch logs; the rest is drained unread.
const FETCH_OUTPUT_CAP: usize = 64 * 1024;

/// The prefix of every kernel-owned upstream ref.
pub(crate) const KERNEL_UPSTREAM_REF_PREFIX: &str = "refs/neige/upstream/";

/// The upstream of the branch the repository's HEAD is on, as git config
/// names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Upstream {
    /// `branch.<b>.remote` — a remote name, `.` for a local upstream.
    pub remote: String,
    /// `branch.<b>.merge` — the ref on that remote, e.g. `refs/heads/main`.
    pub merge: String,
    /// The repository's own ref for that upstream (`%(upstream)`: the
    /// remote-tracking ref the fetch refspec maps `merge` to, or the local
    /// branch for `.`); `None` when no refspec maps it. Read, never written.
    pub tracking_ref: Option<String>,
}

impl Upstream {
    /// `refs/neige/upstream/<remote>/<branch>` — where [`refresh_upstream`]
    /// fetches this upstream to. `None` when the upstream is not something the
    /// kernel fetches: a local (`.`) upstream, a remote that is not a plain
    /// name (a URL, a name with `/`), or a `merge` outside `refs/heads/`.
    pub(crate) fn kernel_ref(&self) -> Option<String> {
        if !is_plain_remote_name(&self.remote) {
            return None;
        }
        let branch = self.merge.strip_prefix("refs/heads/")?;
        if !is_plain_ref_path(branch) {
            return None;
        }
        Some(format!(
            "{KERNEL_UPSTREAM_REF_PREFIX}{}/{branch}",
            self.remote
        ))
    }
}

/// A remote name that can stand as one ref component and as `git fetch`'s
/// repository argument without being read as an option or a path.
fn is_plain_remote_name(remote: &str) -> bool {
    !remote.is_empty()
        && !remote.starts_with(['.', '-'])
        && !remote.ends_with(".lock")
        && !remote.contains("..")
        && remote
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// A branch path (`merge` minus `refs/heads/`) that is safe inside a refspec
/// and a ref name: no refspec or revision syntax, no empty or dot-led
/// component.
fn is_plain_ref_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains("..")
        && !path.contains("@{")
        && !path.ends_with(".lock")
        && path.split('/').all(|component| {
            !component.is_empty() && !component.starts_with('.') && !component.ends_with(".lock")
        })
        && path.bytes().all(|byte| {
            byte > b' '
                && byte != 0x7f
                && !matches!(byte, b':' | b'~' | b'^' | b'?' | b'*' | b'[' | b'\\')
        })
}

/// The upstream of the branch `repo_root`'s HEAD is on. `Ok(None)`: HEAD is
/// detached, the branch is unborn, or it has no complete upstream config.
pub(crate) fn head_upstream(repo_root: &Path) -> Result<Option<Upstream>> {
    let Some(head_ref) = super::base::worktree_head_ref(repo_root)? else {
        return Ok(None);
    };
    let args = [
        "for-each-ref",
        "--format=%(refname)%00%(upstream:remotename)%00%(upstream:remoteref)%00%(upstream)",
        head_ref.as_str(),
    ];
    let output = git_output(repo_root, &args)?;
    if !output.status.success() {
        return Err(super::git_failed(
            &format!("git {}", args.join(" ")),
            repo_root,
            &output,
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // `for-each-ref <pattern>` also lists refs under `<pattern>/`; only the
    // exact branch is HEAD's.
    for line in stdout.lines() {
        let mut fields = line.split('\0');
        let (Some(refname), Some(remote), Some(merge), Some(tracking)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(CalmError::Internal(format!(
                "git for-each-ref in {} printed {line:?}, not four NUL-separated fields",
                repo_root.display()
            )));
        };
        if refname != head_ref {
            continue;
        }
        if remote.is_empty() || merge.is_empty() {
            return Ok(None);
        }
        return Ok(Some(Upstream {
            remote: remote.to_string(),
            merge: merge.to_string(),
            tracking_ref: (!tracking.is_empty()).then(|| tracking.to_string()),
        }));
    }
    Ok(None)
}

/// The commit of HEAD's upstream as last known: the kernel ref
/// ([`Upstream::kernel_ref`]) when it resolves, else the repository's own
/// remote-tracking ref (read-only). `Ok(None)` when HEAD has no upstream or
/// neither ref resolves — the base is then HEAD. Local only: this runs inside
/// the prepare transaction and on the `plan.list` read, and never fetches.
pub(crate) fn last_known_upstream(repo_root: &Path) -> Result<Option<String>> {
    let Some(upstream) = head_upstream(repo_root)? else {
        return Ok(None);
    };
    for candidate in [upstream.kernel_ref(), upstream.tracking_ref]
        .into_iter()
        .flatten()
    {
        if let Some(sha) = resolve_commit(repo_root, &candidate)? {
            return Ok(Some(sha));
        }
    }
    Ok(None)
}

/// `git rev-parse --verify -q <ref>^{commit}`: `Some(sha)` when the ref names
/// a commit, `None` when it does not resolve.
fn resolve_commit(repo_root: &Path, ref_name: &str) -> Result<Option<String>> {
    let rev = format!("{ref_name}^{{commit}}");
    let args = ["rev-parse", "--verify", "-q", rev.as_str()];
    let output = git_output(repo_root, &args)?;
    if !output.status.success() {
        return Ok(None);
    }
    let printed = String::from_utf8_lossy(&output.stdout);
    let sha = printed.trim_end_matches('\n');
    if sha.is_empty() || sha.contains(char::is_whitespace) {
        return Err(CalmError::Internal(format!(
            "git {} in {} printed {printed:?}, not a single object name",
            args.join(" "),
            repo_root.display()
        )));
    }
    Ok(Some(sha.to_string()))
}

/// `git rev-list --count <base>..<upstream>`: how many commits `upstream` has
/// that `base` does not.
pub(crate) fn commits_behind(repo_root: &Path, base_sha: &str, upstream_sha: &str) -> Result<u64> {
    let range = format!("{base_sha}..{upstream_sha}");
    let args = ["rev-list", "--count", range.as_str()];
    let output = git_output(repo_root, &args)?;
    if !output.status.success() {
        return Err(super::git_failed(
            &format!("git {}", args.join(" ")),
            repo_root,
            &output,
        ));
    }
    let printed = String::from_utf8_lossy(&output.stdout);
    printed.trim().parse().map_err(|error| {
        CalmError::Internal(format!(
            "git {} in {} printed {printed:?}: {error}",
            args.join(" "),
            repo_root.display()
        ))
    })
}

fn git_output(repo_root: &Path, args: &[&str]) -> Result<std::process::Output> {
    neige_git_command()
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .map_err(|e| {
            CalmError::Internal(format!(
                "spawn git {} in {}: {e}",
                args.join(" "),
                repo_root.display()
            ))
        })
}

/// What one [`refresh_upstream`] did. Informational: every arm leaves the
/// lease to [`last_known_upstream`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UpstreamRefresh {
    /// HEAD has no upstream (detached, unborn, or no config): nothing to fetch.
    NoUpstream,
    /// The upstream is not one the kernel fetches ([`Upstream::kernel_ref`] is
    /// `None`); the remote-tracking ref, if any, is what the lease reads.
    NotFetched,
    /// The kernel ref now holds the upstream's current commit.
    Fetched { kernel_ref: String },
    /// The fetch failed or timed out; logged at `warn`.
    Failed { reason: String },
}

/// Fetch HEAD's upstream into its kernel ref, bounded by
/// [`UPSTREAM_FETCH_TIMEOUT`]. Never inside a database transaction: this is
/// network I/O of unbounded latency short of the timeout, and the prepare
/// transaction is the kernel's only writer.
pub(crate) async fn refresh_upstream(repo_root: &Path) -> UpstreamRefresh {
    refresh_upstream_within(repo_root, UPSTREAM_FETCH_TIMEOUT).await
}

pub(crate) async fn refresh_upstream_within(repo_root: &Path, bound: Duration) -> UpstreamRefresh {
    let upstream = match head_upstream(repo_root) {
        Ok(Some(upstream)) => upstream,
        Ok(None) => return UpstreamRefresh::NoUpstream,
        Err(error) => {
            return failed(repo_root, format!("reading the upstream failed: {error}"));
        }
    };
    let Some(kernel_ref) = upstream.kernel_ref() else {
        return UpstreamRefresh::NotFetched;
    };
    match fetch_into(repo_root, &upstream, &kernel_ref, bound).await {
        Ok(()) => UpstreamRefresh::Fetched { kernel_ref },
        Err(reason) => failed(repo_root, reason),
    }
}

fn failed(repo_root: &Path, reason: String) -> UpstreamRefresh {
    tracing::warn!(
        repo_root = %repo_root.display(),
        reason,
        "upstream refresh failed; the lease base is the last known upstream"
    );
    UpstreamRefresh::Failed { reason }
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
/// `GIT_TERMINAL_PROMPT=0`, so no credential prompt can wait on a terminal;
/// past `bound` the group is killed (`kill_on_drop` + the [`GroupChild`]
/// sweep — the slice-4 sampler's precedent).
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
        .env("GIT_TERMINAL_PROMPT", "0");
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
