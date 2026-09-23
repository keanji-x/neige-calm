//! The attached repository's upstream as a lease base (#1777): the local half.
//!
//! An attached repository is a human's working copy: its HEAD sits wherever
//! that human left it, usually behind the branch's upstream, so a lease based
//! on HEAD starts stale and gets staler as other work lands upstream. When the
//! branch HEAD is on has a configured upstream (`branch.<b>.remote` +
//! `branch.<b>.merge`), [`choose_lease_start`] compares HEAD with that
//! upstream as last known (U):
//!
//! | relation | base | `base_source` |
//! |---|---|---|
//! | no upstream, or nothing of it resolves | HEAD | `head` |
//! | HEAD == U, or HEAD is an ancestor of U (behind) | U | `upstream` |
//! | U is an ancestor of HEAD (ahead: unpushed local commits) | HEAD, which contains U | `head` |
//! | diverged (neither contains the other) in a complete history | refused: [`ATTACHED_REPO_DIVERGED`] | — |
//! | neither check succeeds in a shallow history (unknown) | HEAD, with a `warn!` | `head` |
//!
//! A diverged checkout is a human decision (push, rebase or reset); the lease
//! is refused rather than guessed at ([`diverged_refusal`]).
//!
//! Everything here is local `git` only — it runs inside the prepare
//! transaction and on the `plan.list` read. The network half, the bounded
//! fetch into [`Upstream::kernel_ref`] on the submit path, is
//! [`super::upstream_fetch`].

use std::path::Path;

use sha2::{Digest, Sha256};

use super::upstream_fetch::FetchProvenance;
use crate::error::{CalmError, Result};
use crate::workspace_materialize::neige_git_command;

/// The prefix of every kernel-owned upstream ref.
pub(crate) const KERNEL_UPSTREAM_REF_PREFIX: &str = "refs/neige/upstream/";

/// The machine code of the refusal a diverged attached checkout gets. Stable:
/// the Planner prompt and the failure text both name it.
pub(crate) const ATTACHED_REPO_DIVERGED: &str = "attached-repo-diverged";

/// The upstream of the branch the repository's HEAD is on, as git config
/// names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Upstream {
    /// `branch.<b>.remote` — a remote name, `.` for a local upstream (or a URL,
    /// which the kernel does not fetch: there is no named remote).
    pub remote: String,
    /// `branch.<b>.merge` — the ref on that remote, e.g. `refs/heads/main`.
    pub merge: String,
    /// The effective fetch URL of `remote` for this checkout, as
    /// `git ls-remote --get-url` resolves it (per-worktree config and
    /// `url.<base>.insteadOf` applied, no network); the remote string itself
    /// for `.` or a URL remote.
    pub url: String,
    /// The repository's own ref for that upstream (`%(upstream)`: the
    /// remote-tracking ref the fetch refspec maps `merge` to, or the local
    /// branch for `.`); `None` when no refspec maps it. Read, never written.
    pub tracking_ref: Option<String>,
}

impl Upstream {
    /// `refs/neige/upstream/<sha256(remote NUL merge NUL url) hex>` — where
    /// the submit-path fetch puts this upstream. Total and collision-free over
    /// every (remote, merge-ref, URL) triple: a remote with `/`, a merge ref
    /// outside `refs/heads/`, and `.` all get their own name, and the name is
    /// one hex component whatever the inputs contain. The URL is part of it
    /// because linked worktrees sharing one common dir can fetch the same
    /// remote name from different URLs, and a human can re-point a remote: a
    /// new URL is a new kernel ref and a new provenance key, with no receipt.
    pub(crate) fn kernel_ref(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.remote.as_bytes());
        hasher.update([0u8]);
        hasher.update(self.merge.as_bytes());
        hasher.update([0u8]);
        hasher.update(self.url.as_bytes());
        format!(
            "{KERNEL_UPSTREAM_REF_PREFIX}{}",
            hex::encode(hasher.finalize())
        )
    }
}

/// The upstream of the branch `repo_root`'s HEAD is on, read from its
/// config (`branch.<b>.remote` / `branch.<b>.merge`) — every configuration
/// counts, including a merge ref no fetch refspec maps, which git's own
/// `%(upstream)` leaves empty. `Ok(None)`: HEAD is detached, or the branch
/// has no complete upstream config.
pub(crate) fn head_upstream(repo_root: &Path) -> Result<Option<Upstream>> {
    let Some(head_ref) = super::base::worktree_head_ref(repo_root)? else {
        return Ok(None);
    };
    let Some(branch) = head_ref.strip_prefix("refs/heads/") else {
        return Ok(None);
    };
    let (Some(remote), Some(merge)) = (
        // As git resolves `@{upstream}`: the remote is the last value (plain
        // config), the merge ref the FIRST of `branch.<b>.merge`'s values.
        config_value(
            repo_root,
            &format!("branch.{branch}.remote"),
            ConfigPick::Last,
        )?,
        config_value(
            repo_root,
            &format!("branch.{branch}.merge"),
            ConfigPick::First,
        )?,
    ) else {
        return Ok(None);
    };
    let args = [
        "for-each-ref",
        "--format=%(refname)%00%(upstream)",
        head_ref.as_str(),
    ];
    let stdout = git_success(repo_root, &args)?;
    // `for-each-ref <pattern>` also lists refs under `<pattern>/`; only the
    // exact branch is HEAD's. An unborn branch lists nothing: no tracking ref.
    let mut tracking_ref = None;
    for line in stdout.lines() {
        let fields: Vec<&str> = line.split('\0').collect();
        let [refname, tracking] = fields[..] else {
            return Err(CalmError::Internal(format!(
                "git for-each-ref in {} printed {line:?}, not two NUL-separated fields",
                repo_root.display()
            )));
        };
        if refname == head_ref && !tracking.is_empty() {
            tracking_ref = Some(tracking.to_string());
        }
    }
    let url = effective_url(repo_root, &remote)?;
    Ok(Some(Upstream {
        remote,
        merge,
        url,
        tracking_ref,
    }))
}

/// `git ls-remote --get-url <remote>`: the URL a fetch of `remote` would
/// use from this checkout (no network). A remote git would read as an option
/// is returned as is; it is never fetched.
fn effective_url(repo_root: &Path, remote: &str) -> Result<String> {
    if remote.starts_with('-') {
        return Ok(remote.to_string());
    }
    let printed = git_success(repo_root, &["ls-remote", "--get-url", remote])?;
    Ok(printed.trim_end_matches('\n').to_string())
}

/// Which value of a multi-valued config key git itself uses.
#[derive(Clone, Copy)]
enum ConfigPick {
    First,
    Last,
}

/// `git config --get-all <key>`, picking the value git uses; `Ok(None)` when
/// unset (exit 1), or when the picked value is empty (git then has no
/// upstream either — an empty value is kept while picking, as git keeps it).
fn config_value(repo_root: &Path, key: &str, pick: ConfigPick) -> Result<Option<String>> {
    let args = ["config", "-z", "--get-all", key];
    let output = git_output(repo_root, &args)?;
    match output.status.code() {
        Some(0) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // `-z`: every value ends in NUL, empty values included.
            let mut values = stdout.strip_suffix('\0').unwrap_or(&stdout).split('\0');
            let value = match pick {
                ConfigPick::First => values.next(),
                ConfigPick::Last => values.next_back(),
            };
            Ok(value.filter(|value| !value.is_empty()).map(str::to_string))
        }
        Some(1) => Ok(None),
        _ => Err(super::git_failed(
            &format!("git {}", args.join(" ")),
            repo_root,
            &output,
        )),
    }
}

/// Where the upstream commit a lease is measured against was observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpstreamSource {
    /// The receipt of the kernel's most recent fetch, which succeeded.
    KernelFetch,
    /// The human's own remote-tracking ref.
    TrackingRef,
    /// The kernel ref with no success receipt (no tracking ref resolves).
    KernelRef,
}

/// HEAD's upstream as last known: which upstream (`<remote> <merge>`, as a
/// human names it), the ref it was read from, where it was observed, and its
/// commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KnownUpstream {
    pub name: String,
    pub ref_name: String,
    pub source: UpstreamSource,
    pub sha: String,
}

/// The authoritative upstream plus the human's tracking ref, which
/// [`choose_lease_start`] needs apart: it says which commits of HEAD are the
/// human's own (unpushed).
#[derive(Clone, Debug, PartialEq, Eq)]
struct UpstreamView {
    authoritative: KnownUpstream,
    tracking_sha: Option<String>,
}

/// HEAD's upstream as last known, by fetch provenance — never by comparing
/// the two candidate refs' ancestry (an upstream can be rewound or
/// force-pushed; the older commit may be the right one):
///
/// - the kernel's most recent fetch of this upstream succeeded → its receipt
///   ([`FetchProvenance::success_receipt`]) is authoritative: the commit the
///   fetch left, recorded under the single-flight lock together with the
///   outcome. The kernel ref itself is not read then — a concurrent fetch
///   could move it between two reads and pair an old commit with a new
///   outcome;
/// - otherwise (the last fetch failed or was backed off, or there is no
///   receipt, e.g. after a restart or a URL change) → the repository's own
///   remote-tracking ref when it resolves: it is the human's view, the one a
///   refusal can name and the human can act on; the kernel ref only when no
///   tracking ref resolves.
///
/// A symbolic kernel ref is never read (it could point anywhere).
/// `Ok(None)` when HEAD has no upstream or nothing of it resolves — the base
/// is then HEAD. Local only; never fetches, never waits for a fetch.
pub(crate) fn last_known_upstream(repo_root: &Path) -> Result<Option<KnownUpstream>> {
    Ok(upstream_view(repo_root)?.map(|view| view.authoritative))
}

fn upstream_view(repo_root: &Path) -> Result<Option<UpstreamView>> {
    let Some(upstream) = head_upstream(repo_root)? else {
        return Ok(None);
    };
    let name = format!("{} {}", upstream.remote, upstream.merge);
    let kernel_ref = upstream.kernel_ref();
    let tracking = match &upstream.tracking_ref {
        Some(ref_name) => resolve_commit(repo_root, ref_name)?.map(|sha| KnownUpstream {
            name: name.clone(),
            ref_name: ref_name.clone(),
            source: UpstreamSource::TrackingRef,
            sha,
        }),
        None => None,
    };
    let tracking_sha = tracking.as_ref().map(|tracking| tracking.sha.clone());
    let key = (
        super::base::lease_git_common_dir(repo_root)?,
        kernel_ref.clone(),
    );
    let authoritative = if let Some((sha, at)) = FetchProvenance::global().success_receipt(&key) {
        tracing::debug!(
            repo_root = %repo_root.display(),
            sha,
            receipt_age_ms = at.elapsed().as_millis() as u64,
            "upstream from the kernel's fetch receipt"
        );
        Some(KnownUpstream {
            name,
            ref_name: kernel_ref,
            source: UpstreamSource::KernelFetch,
            sha,
        })
    } else if tracking.is_some() {
        tracking
    } else {
        read_direct_ref(repo_root, &kernel_ref)?.map(|sha| KnownUpstream {
            name,
            ref_name: kernel_ref,
            source: UpstreamSource::KernelRef,
            sha,
        })
    };
    Ok(authoritative.map(|authoritative| UpstreamView {
        authoritative,
        tracking_sha,
    }))
}

/// Where a new lease starts, by the relation of HEAD to its upstream as last
/// known (the module table).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LeaseStart {
    /// `base_source = 'head'`: no known upstream, or HEAD already contains it.
    Head { sha: String },
    /// `base_source = 'upstream'`: HEAD equals the upstream or is behind it.
    Upstream { sha: String },
    /// Neither contains the other: no lease.
    Diverged {
        head: String,
        upstream: KnownUpstream,
        /// The human's own commits: HEAD's commits their tracking ref lacks
        /// (`ahead` when no tracking ref resolves).
        unpushed: u64,
        /// Commits HEAD has that the upstream lacks.
        ahead: u64,
        /// Commits the upstream has that HEAD lacks.
        behind: u64,
    },
}

/// The prepare transaction's base decision. Local reads only: `rev-parse`,
/// `for-each-ref`, `merge-base --is-ancestor`, and — only when neither
/// ancestry check succeeds — the human's unpushed-commit count against their
/// tracking ref, `--is-shallow-repository` and the refusal's
/// `rev-list --count`.
///
/// Only real local work refuses: HEAD not related to the upstream either way
/// is a human decision only when HEAD carries commits of the human's own
/// (unpushed relative to their tracking ref). A checkout in sync with its
/// tracking ref after an upstream force-push has nothing to lose and bases on
/// the upstream. A shallow history that cannot show the relation starts from
/// HEAD. Everything else that diverged in a complete history is refused.
pub(crate) fn choose_lease_start(repo_root: &Path) -> Result<LeaseStart> {
    let head = super::base::resolve_head_base(repo_root)?;
    let Some(UpstreamView {
        authoritative: upstream,
        tracking_sha,
    }) = upstream_view(repo_root)?
    else {
        return Ok(LeaseStart::Head { sha: head });
    };
    if head == upstream.sha || is_ancestor(repo_root, &head, &upstream.sha)? {
        return Ok(LeaseStart::Upstream { sha: upstream.sha });
    }
    if is_ancestor(repo_root, &upstream.sha, &head)? {
        return Ok(LeaseStart::Head { sha: head });
    }
    let unpushed = match &tracking_sha {
        Some(tracking) => Some(commits_behind(repo_root, tracking, &head)?),
        None => None,
    };
    if unpushed == Some(0) {
        // The human has no commits of their own: nothing to lose.
        return Ok(LeaseStart::Upstream { sha: upstream.sha });
    }
    if is_shallow(repo_root)? {
        // Neither check succeeded, but a shallow history cannot prove
        // divergence: the relation is unknown. HEAD, as before #1777.
        tracing::warn!(
            repo_root = %repo_root.display(),
            head,
            upstream = upstream.sha,
            "attached repository is shallow: HEAD's relation to its upstream is unknown; \
             the lease starts from HEAD"
        );
        return Ok(LeaseStart::Head { sha: head });
    }
    let (ahead, behind) = ahead_behind(repo_root, &head, &upstream.sha)?;
    Ok(LeaseStart::Diverged {
        head,
        upstream,
        unpushed: unpushed.unwrap_or(ahead),
        ahead,
        behind,
    })
}

/// The refusal of a diverged checkout: a `Conflict`, so the worker op fails
/// once (a client-class failure is never re-driven) and the task fails as
/// `spawn-failed: refused: attached-repo-diverged: …` with this text as the
/// reason the Planner reads. It names where the upstream commit was observed
/// (a commit only the kernel's fetch has seen needs a `git fetch` before the
/// human can see it) and asks for what reconciles unpushed work: rebase it
/// onto the upstream (then push) or reset to the upstream — never a bare
/// push, which would undo an upstream rewrite.
pub(crate) fn diverged_refusal(
    repo_root: &Path,
    head: &str,
    upstream: &KnownUpstream,
    unpushed: u64,
    ahead: u64,
    behind: u64,
) -> CalmError {
    let (seen, fetch_first) = match upstream.source {
        UpstreamSource::KernelFetch => ("kernel fetch", "run `git fetch`, then "),
        UpstreamSource::TrackingRef => ("your tracking ref", ""),
        UpstreamSource::KernelRef => ("kernel ref", "run `git fetch`, then "),
    };
    // The checkout path goes last: `status_detail` keeps 480 characters of
    // the reason, and the path is the one fact the Planner already has.
    CalmError::Conflict(format!(
        "refused: {ATTACHED_REPO_DIVERGED}: HEAD {head} ({unpushed} unpushed) and upstream {} \
         at {} (per {seen}) diverged: {ahead} ahead, {behind} behind. Not retryable: a human must \
         {fetch_first}rebase the unpushed commits onto the upstream and push, or reset to it; \
         re-dispatch works afterwards. Checkout: {}",
        upstream.name,
        upstream.sha,
        repo_root.display(),
    ))
}

/// `git merge-base --is-ancestor <ancestor> <descendant>`: exit 0 yes, 1 no,
/// anything else an error.
pub(crate) fn is_ancestor(repo_root: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let args = ["merge-base", "--is-ancestor", ancestor, descendant];
    let output = git_output(repo_root, &args)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(super::git_failed(
            &format!("git {}", args.join(" ")),
            repo_root,
            &output,
        )),
    }
}

/// `git rev-parse --is-shallow-repository`.
fn is_shallow(repo_root: &Path) -> Result<bool> {
    let printed = git_success(repo_root, &["rev-parse", "--is-shallow-repository"])?;
    Ok(printed.trim() == "true")
}

/// `git rev-list --left-right --count <head>...<upstream>`: (ahead, behind).
fn ahead_behind(repo_root: &Path, head: &str, upstream: &str) -> Result<(u64, u64)> {
    let range = format!("{head}...{upstream}");
    let args = ["rev-list", "--left-right", "--count", range.as_str()];
    let printed = git_success(repo_root, &args)?;
    let counts: Vec<u64> = printed
        .split_whitespace()
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .map_err(|error| {
            CalmError::Internal(format!(
                "git {} printed {printed:?}: {error}",
                args.join(" ")
            ))
        })?;
    match counts[..] {
        [ahead, behind] => Ok((ahead, behind)),
        _ => Err(CalmError::Internal(format!(
            "git {} printed {printed:?}, not two counts",
            args.join(" ")
        ))),
    }
}

/// The commit a non-symbolic ref points at (a tag peeled to its commit);
/// `None` when the ref is absent, symbolic, or not a commit.
pub(super) fn read_direct_ref(repo_root: &Path, ref_name: &str) -> Result<Option<String>> {
    let args = [
        "for-each-ref",
        "--format=%(refname)%00%(symref)%00%(objecttype)%00%(objectname)%00%(*objecttype)%00%(*objectname)",
        ref_name,
    ];
    let stdout = git_success(repo_root, &args)?;
    for line in stdout.lines() {
        let fields: Vec<&str> = line.split('\0').collect();
        let [refname, symref, kind, sha, peeled_kind, peeled_sha] = fields[..] else {
            return Err(CalmError::Internal(format!(
                "git for-each-ref in {} printed {line:?}, not six NUL-separated fields",
                repo_root.display()
            )));
        };
        if refname != ref_name {
            continue;
        }
        if !symref.is_empty() {
            return Ok(None);
        }
        return Ok(match (kind, peeled_kind) {
            ("commit", _) => Some(sha.to_string()),
            ("tag", "commit") => Some(peeled_sha.to_string()),
            _ => None,
        });
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
    let printed = git_success(repo_root, &args)?;
    printed.trim().parse().map_err(|error| {
        CalmError::Internal(format!(
            "git {} in {} printed {printed:?}: {error}",
            args.join(" "),
            repo_root.display()
        ))
    })
}

/// stdout of a git command that must succeed.
pub(super) fn git_success(repo_root: &Path, args: &[&str]) -> Result<String> {
    let output = git_output(repo_root, args)?;
    if !output.status.success() {
        return Err(super::git_failed(
            &format!("git {}", args.join(" ")),
            repo_root,
            &output,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub(super) fn git_output(repo_root: &Path, args: &[&str]) -> Result<std::process::Output> {
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
