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
    /// The repository's own ref for that upstream (`%(upstream)`: the
    /// remote-tracking ref the fetch refspec maps `merge` to, or the local
    /// branch for `.`); `None` when no refspec maps it. Read, never written.
    pub tracking_ref: Option<String>,
}

impl Upstream {
    /// `refs/neige/upstream/<sha256(remote NUL merge) hex>` — where the
    /// submit-path fetch puts this upstream. Total and collision-free over
    /// every (remote, merge-ref) pair: a remote with `/`, a merge ref outside
    /// `refs/heads/`, and `.` all get their own name, and the name is one
    /// hex component whatever the inputs contain.
    pub(crate) fn kernel_ref(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.remote.as_bytes());
        hasher.update([0u8]);
        hasher.update(self.merge.as_bytes());
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
    Ok(Some(Upstream {
        remote,
        merge,
        tracking_ref,
    }))
}

/// Which value of a multi-valued config key git itself uses.
#[derive(Clone, Copy)]
enum ConfigPick {
    First,
    Last,
}

/// `git config --get-all <key>`, picking the value git uses; `Ok(None)` when
/// unset (exit 1) or empty.
fn config_value(repo_root: &Path, key: &str, pick: ConfigPick) -> Result<Option<String>> {
    let args = ["config", "-z", "--get-all", key];
    let output = git_output(repo_root, &args)?;
    match output.status.code() {
        Some(0) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut values = stdout.split('\0').filter(|value| !value.is_empty());
            let value = match pick {
                ConfigPick::First => values.next(),
                ConfigPick::Last => values.next_back(),
            };
            Ok(value.map(str::to_string))
        }
        Some(1) => Ok(None),
        _ => Err(super::git_failed(
            &format!("git {}", args.join(" ")),
            repo_root,
            &output,
        )),
    }
}

/// HEAD's upstream as last known: which upstream (`<remote> <merge>`, as a
/// human names it), the ref it was read from, and its commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KnownUpstream {
    pub name: String,
    pub ref_name: String,
    pub sha: String,
}

/// HEAD's upstream as last known, by fetch provenance — never by comparing
/// the two candidate refs' ancestry (an upstream can be rewound or
/// force-pushed; the older commit may be the right one):
///
/// - the most recent kernel fetch of this upstream succeeded
///   ([`FetchProvenance::last_fetch_succeeded`]) → the kernel ref is
///   authoritative: it is the upstream as the remote had it moments ago;
/// - otherwise (the last fetch failed or was backed off, or there is no
///   record, e.g. after a restart) → the repository's own remote-tracking ref
///   when it resolves: it is the human's view, the one a refusal can name and
///   the human can act on; the kernel ref only when no tracking ref resolves.
///
/// A symbolic kernel ref is never read (it could point anywhere).
/// `Ok(None)` when HEAD has no upstream or nothing of it resolves — the base
/// is then HEAD. Local only; never fetches.
pub(crate) fn last_known_upstream(repo_root: &Path) -> Result<Option<KnownUpstream>> {
    let Some(upstream) = head_upstream(repo_root)? else {
        return Ok(None);
    };
    let name = format!("{} {}", upstream.remote, upstream.merge);
    let kernel_ref = upstream.kernel_ref();
    let kernel = read_direct_ref(repo_root, &kernel_ref)?.map(|sha| KnownUpstream {
        name: name.clone(),
        ref_name: kernel_ref,
        sha,
    });
    let tracking = match upstream.tracking_ref {
        Some(ref_name) => resolve_commit(repo_root, &ref_name)?.map(|sha| KnownUpstream {
            name,
            ref_name,
            sha,
        }),
        None => None,
    };
    let Some(kernel) = kernel else {
        return Ok(tracking);
    };
    let key = (
        super::base::lease_git_common_dir(repo_root)?,
        kernel.ref_name.clone(),
    );
    if FetchProvenance::global().last_fetch_succeeded(&key) {
        return Ok(Some(kernel));
    }
    Ok(Some(tracking.unwrap_or(kernel)))
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
        /// Commits HEAD has that the upstream lacks.
        ahead: u64,
        /// Commits the upstream has that HEAD lacks.
        behind: u64,
    },
}

/// The prepare transaction's base decision. Local reads only: `rev-parse`,
/// `for-each-ref`, `merge-base --is-ancestor`, and — only when neither
/// ancestry check succeeds — `--is-shallow-repository` and the refusal's
/// `rev-list --count`. Only proven divergence (a complete history) refuses.
pub(crate) fn choose_lease_start(repo_root: &Path) -> Result<LeaseStart> {
    let head = super::base::resolve_head_base(repo_root)?;
    let Some(upstream) = last_known_upstream(repo_root)? else {
        return Ok(LeaseStart::Head { sha: head });
    };
    if head == upstream.sha || is_ancestor(repo_root, &head, &upstream.sha)? {
        return Ok(LeaseStart::Upstream { sha: upstream.sha });
    }
    if is_ancestor(repo_root, &upstream.sha, &head)? {
        return Ok(LeaseStart::Head { sha: head });
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
        ahead,
        behind,
    })
}

/// The refusal of a diverged checkout: a `Conflict`, so the worker op fails
/// once (a client-class failure is never re-driven) and the task fails as
/// `spawn-failed: refused: attached-repo-diverged: …` with this text as the
/// reason the Planner reads. Every fact a human needs to reconcile is in it.
pub(crate) fn diverged_refusal(
    repo_root: &Path,
    head: &str,
    upstream: &KnownUpstream,
    ahead: u64,
    behind: u64,
) -> CalmError {
    CalmError::Conflict(format!(
        "refused: {ATTACHED_REPO_DIVERGED}: the attached checkout {} (HEAD {head}) and its \
         upstream {} ({}) have diverged: {ahead} ahead, {behind} behind. Not retryable: a human \
         must reconcile the checkout (push, rebase or reset); re-dispatch works afterwards.",
        repo_root.display(),
        upstream.name,
        upstream.sha,
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
fn read_direct_ref(repo_root: &Path, ref_name: &str) -> Result<Option<String>> {
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
