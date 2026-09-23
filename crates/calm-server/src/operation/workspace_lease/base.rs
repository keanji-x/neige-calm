//! The base a workspace lease's worktree starts from, and the check that the
//! provisioned worktree really is that base at that path.
//!
//! A lease row used to say only *where* a worker's worktree is; the worktree
//! itself was created with `git worktree add -b <branch> <path>` — no
//! commit-ish — so its starting point was whatever the attached repository's
//! HEAD happened to be when the spawn ran, not when the attempt was prepared,
//! and a branch that already existed (crash residue, a hand-made branch) was
//! taken at its tip without a look. This module resolves the base inside the
//! worker op's prepare transaction, writes it into the lease row in the same
//! INSERT ([`super::acquire_workspace_lease_tx`]), and makes provisioning pin
//! `worktree add` to it and refuse a worktree whose HEAD or realpath differs
//! (design D4 / oracle rows 2, 3, 3n).
//!
//! `base_source`: [`BaseSource::Upstream`] and [`BaseSource::Head`] have a
//! producer ([`resolve_lease_base`]: the upstream of the branch HEAD is on
//! when HEAD is at or behind it, HEAD when HEAD is ahead or there is none, a
//! refusal when the two diverged — [`super::upstream`], #1777). `Commit`
//! and `Attempt` (`TaskDeclaration.base`) are written by slice 5; the column
//! round-trip covers all four so the CHECK-accepted shapes and the Rust type
//! never disagree.

use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
};

use sqlx::{Row, Sqlite, sqlite::SqliteRow};

use super::upstream::LeaseStart;
use super::{
    GitWorktreeRegistration, WorkspaceLease, WorkspaceLeaseDirectoryMode, WorkspaceLeaseTarget,
    create_workspace_lease_directory,
};
use crate::error::{CalmError, Result};
use crate::operation::TxOutput;
use crate::workspace_materialize::neige_git_command;

/// `workspace_leases.base_source`: how `base_sha` was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BaseSource {
    /// The attached repository's HEAD at prepare time (`TaskDeclaration.base`
    /// absent, and the branch HEAD is on has no upstream, no ref of that
    /// upstream resolves, or HEAD is ahead of it — HEAD then contains it).
    Head,
    /// The last known commit of the upstream of the branch HEAD is on, which
    /// HEAD equals or is behind (`TaskDeclaration.base` absent): the fresher
    /// of the kernel-fetched `refs/neige/upstream/<digest>` and the
    /// repository's own remote-tracking ref
    /// ([`super::upstream::last_known_upstream`], #1777).
    Upstream,
    /// `TaskDeclaration.base: {commit}` — slice 5.
    Commit,
    /// `TaskDeclaration.base: {attempt}`; `base_attempt_id` names the
    /// producing attempt — slice 5.
    Attempt,
}

impl BaseSource {
    pub(crate) fn as_column(self) -> &'static str {
        match self {
            BaseSource::Head => "head",
            BaseSource::Upstream => "upstream",
            BaseSource::Commit => "commit",
            BaseSource::Attempt => "attempt",
        }
    }

    pub(crate) fn from_column(value: &str) -> Result<Self> {
        match value {
            "head" => Ok(BaseSource::Head),
            "upstream" => Ok(BaseSource::Upstream),
            "commit" => Ok(BaseSource::Commit),
            "attempt" => Ok(BaseSource::Attempt),
            other => Err(CalmError::Internal(format!(
                "workspace lease base_source {other:?} is not head, upstream, commit or attempt"
            ))),
        }
    }
}

/// `workspace_leases.delivery_policy`: who commits and pins this lease's
/// worktree. `Kernel` (`'kernel'`) is the one criterion for candidate
/// binding (#1727 S4 D2); a NULL column — a lease claimed before migration
/// 0113, or the fixtures-only plain lease — is legacy and reads as `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeliveryPolicy {
    Kernel,
}

impl DeliveryPolicy {
    pub(crate) fn as_column(self) -> &'static str {
        match self {
            DeliveryPolicy::Kernel => "kernel",
        }
    }

    /// Decode the column of one row: NULL is legacy, `'kernel'` is `Kernel`
    /// (and the row has a base — the CHECK admits nothing else); any other
    /// shape is reported rather than guessed at.
    pub(crate) fn from_row(row: &SqliteRow) -> Result<Option<DeliveryPolicy>> {
        let value: Option<String> = row.try_get("delivery_policy")?;
        let base_sha: Option<String> = row.try_get("base_sha")?;
        match (value.as_deref(), base_sha) {
            (None, _) => Ok(None),
            (Some("kernel"), Some(_)) => Ok(Some(DeliveryPolicy::Kernel)),
            (Some("kernel"), None) => Err(CalmError::Internal(
                "workspace lease delivery_policy is kernel on a row without a base".into(),
            )),
            (Some(other), _) => Err(CalmError::Internal(format!(
                "workspace lease delivery_policy {other:?} is not kernel"
            ))),
        }
    }
}

/// The five base columns of a lease row, all present. A row written before
/// migration 0111 (or by the fixtures-only plain lease) has none of them and
/// reads as `None` at [`super::WorkspaceLease::base`]; the tuple CHECK keeps
/// a partially NULL row out of the table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LeaseBase {
    /// The commit the worktree is pinned to.
    pub base_sha: String,
    pub base_source: BaseSource,
    /// Set iff `base_source == Attempt`.
    pub base_attempt_id: Option<String>,
    /// `canonicalize(<repo_root>/.claude/worktrees/<track>)/<card>` — the
    /// realpath the provisioned worktree must resolve to. The parent is
    /// resolved (it exists before the row does), the leaf is not: a leaf that
    /// is itself a symlink is exactly the moved-then-linked worktree the check
    /// exists to refuse. UTF-8 by construction ([`lease_canonical_path`]
    /// refuses a realpath that is not): the value is stored as TEXT and
    /// frozen into `tx_output.data` through `json!`, which panics on a path
    /// it cannot serialise.
    pub canonical_path: PathBuf,
    /// `canonicalize(git rev-parse --path-format=absolute --git-common-dir)`
    /// of the repository the lease anchors to. UTF-8 by construction, as
    /// `canonical_path` ([`lease_git_common_dir`]).
    pub git_common_dir: PathBuf,
}

impl LeaseBase {
    /// Decode the five columns of one `workspace_leases` row. `Ok(None)` for
    /// the all-NULL legacy shape; `Err` for any shape the CHECK does not
    /// accept (unreachable through the migration, reported rather than
    /// guessed at).
    pub(crate) fn from_row(row: &SqliteRow) -> Result<Option<LeaseBase>> {
        let base_sha: Option<String> = row.try_get("base_sha")?;
        let base_source: Option<String> = row.try_get("base_source")?;
        let base_attempt_id: Option<String> = row.try_get("base_attempt_id")?;
        let canonical_path: Option<String> = row.try_get("canonical_path")?;
        let git_common_dir: Option<String> = row.try_get("git_common_dir")?;
        let lease_id: String = row.try_get("lease_id")?;
        match (base_sha, base_source, canonical_path, git_common_dir) {
            (None, None, None, None) if base_attempt_id.is_none() => Ok(None),
            (Some(base_sha), Some(base_source), Some(canonical_path), Some(git_common_dir)) => {
                let base_source = BaseSource::from_column(&base_source)?;
                if (base_source == BaseSource::Attempt) != base_attempt_id.is_some() {
                    return Err(CalmError::Internal(format!(
                        "workspace lease {lease_id} base_source {} disagrees with base_attempt_id {base_attempt_id:?}",
                        base_source.as_column()
                    )));
                }
                Ok(Some(LeaseBase {
                    base_sha,
                    base_source,
                    base_attempt_id,
                    canonical_path: PathBuf::from(canonical_path),
                    git_common_dir: PathBuf::from(git_common_dir),
                }))
            }
            _ => Err(CalmError::Internal(format!(
                "workspace lease {lease_id} base columns are partially NULL"
            ))),
        }
    }

    /// Append the five base columns to the lease INSERT, in the INSERT's own
    /// order (`base_sha, base_source, base_attempt_id, canonical_path,
    /// git_common_dir`); `None` binds the legacy all-NULL tuple (the
    /// fixtures-only plain lease). Both paths are bound as the `&str` they
    /// are by construction; a `LeaseBase` holding a non-UTF-8 path (none of
    /// the constructors produces one) is an `Err` here, never a lossily
    /// rewritten row.
    pub(crate) fn bind_columns<'q>(
        query: sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
        base: Option<&'q LeaseBase>,
    ) -> Result<sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments<'q>>> {
        let paths = base
            .map(|base| {
                Ok::<_, CalmError>((
                    utf8_path(&base.canonical_path, "workspace lease canonical_path")?,
                    utf8_path(&base.git_common_dir, "workspace lease git_common_dir")?,
                ))
            })
            .transpose()?;
        Ok(query
            .bind(base.map(|base| base.base_sha.as_str()))
            .bind(base.map(|base| base.base_source.as_column()))
            .bind(base.and_then(|base| base.base_attempt_id.as_deref()))
            .bind(paths.map(|(canonical_path, _)| canonical_path))
            .bind(paths.map(|(_, git_common_dir)| git_common_dir)))
    }
}

/// The `&str` of a path the lease row stores and `tx_output` freezes, or an
/// `Err` naming it: nothing downstream may see a lossy rewrite (the row would
/// then name a path that does not exist) or a `json!` panic. Applied to every
/// path a reader hands the prepare tx: the canonical parent, the common dir,
/// and the toplevel (`repo_root` — a UTF-8 track cwd can be a
/// symlink into a directory that is not, and `--show-toplevel` prints the
/// physical path, so `repo_root` is UTF-8 only if the toplevel reader says so).
pub(super) fn utf8_path<'a>(path: &'a Path, what: &str) -> Result<&'a str> {
    path.to_str().ok_or_else(|| {
        CalmError::Internal(format!(
            "{what} {} is not UTF-8; a lease is refused rather than recorded lossily",
            path.display()
        ))
    })
}

/// Resolve the base for a lease target inside the prepare transaction, by
/// the relation of the attached repository's HEAD to the upstream of the
/// branch it is on, as last known ([`super::upstream::choose_lease_start`]):
/// behind or equal → that upstream (`upstream`); ahead, or no upstream → HEAD
/// (`head`); diverged → refused ([`super::upstream::diverged_refusal`], a
/// human decision). `git_common_dir` and `canonical_path` as the row will
/// carry them. Local reads only — the upstream was fetched on the submit
/// path, before this transaction began ([`super::upstream_fetch`]). Creates
/// the worktree parent directory first (the same `ParentOnly` step the lease
/// INSERT takes, so the parent is a real directory before it is
/// canonicalized).
pub(crate) fn resolve_lease_base(target: &WorkspaceLeaseTarget) -> Result<LeaseBase> {
    let (base_sha, base_source) = match super::upstream::choose_lease_start(&target.repo_root)? {
        LeaseStart::Head { sha } => (sha, BaseSource::Head),
        LeaseStart::Upstream { sha } => (sha, BaseSource::Upstream),
        LeaseStart::Diverged {
            head,
            upstream,
            ahead,
            behind,
        } => {
            return Err(super::upstream::diverged_refusal(
                &target.repo_root,
                &head,
                &upstream,
                ahead,
                behind,
            ));
        }
    };
    let git_common_dir = lease_git_common_dir(&target.repo_root)?;
    create_workspace_lease_directory(&target.path, WorkspaceLeaseDirectoryMode::ParentOnly)?;
    let parent = target.path.parent().ok_or_else(|| {
        CalmError::Internal(format!(
            "workspace lease path {} has no parent",
            target.path.display()
        ))
    })?;
    let card_id = target
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "workspace lease path {} has no card leaf",
                target.path.display()
            ))
        })?;
    Ok(LeaseBase {
        base_sha,
        base_source,
        base_attempt_id: None,
        canonical_path: lease_canonical_path(parent, card_id)?,
        git_common_dir,
    })
}

/// `git -C <repo_root> rev-parse --verify HEAD^{commit}`: the status is judged
/// before the output is used. Also the read side of the provisioning check
/// (`git -C <worktree> rev-parse --verify HEAD^{commit}`).
pub(crate) fn resolve_head_base(repo_root: &Path) -> Result<String> {
    let args = ["rev-parse", "--verify", "HEAD^{commit}"];
    let printed = git_stdout_line(repo_root, &args)?;
    let sha = printed
        .to_str()
        .filter(|sha| !sha.is_empty() && !sha.contains(char::is_whitespace))
        .ok_or_else(|| {
            CalmError::Internal(format!(
                "git {} in {} printed {printed:?}, not a single object name",
                args.join(" "),
                repo_root.display()
            ))
        })?;
    Ok(sha.to_string())
}

/// `canonicalize(git -C <workspace_path> rev-parse --path-format=absolute --git-common-dir)`.
/// `--path-format` exists since git 2.31 (the module's floor is 2.36, set by
/// `worktree list -z`); every CI runner (`ubuntu-latest`, `ubuntu-22.04`, the
/// self-hosted push-to-main box) and this box have ≥ 2.36. The printed path
/// is canonicalized as the bytes git printed; only the result is required to
/// be UTF-8 (a symlink into a non-UTF-8 directory, or a repository whose own
/// path is not UTF-8) — refused, as [`lease_canonical_path`] refuses it.
pub(crate) fn lease_git_common_dir(workspace_path: &Path) -> Result<PathBuf> {
    let printed = git_stdout_line(
        workspace_path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common_dir = std::fs::canonicalize(&printed).map_err(|e| {
        CalmError::Internal(format!(
            "canonicalize git common dir {} for {}: {e}",
            Path::new(&printed).display(),
            workspace_path.display()
        ))
    })?;
    utf8_path(&common_dir, "workspace lease git_common_dir")?;
    Ok(common_dir)
}

/// The shape both sides of a `git worktree list --porcelain` match take.
/// Git records a worktree at its realpath, so under a symlinked
/// `.claude/worktrees` the lexical lease path never equals the listed one:
/// every provisioning after the first read the registration as absent and
/// the stale-directory cleanup deleted the worker's uncommitted worktree
/// before re-adding it. `canonicalize` when the path exists; for one that
/// does not (a pruned registration whose directory is gone) the parent's
/// realpath with the leaf as named; the path as given when even the parent is
/// gone. Applied to the listed and the lease path alike, so a leaf that is a
/// symlink resolves the same on both sides and reads as the registration it
/// points at — which [`clear_symlink_leaf_before_provision`] refuses.
pub(crate) fn registration_path(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map(|parent| parent.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

/// The registration state of a lease target in `git worktree list
/// --porcelain -z` output. `-z` because a path is printed raw: with the line
/// format an embedded newline split the path over two lines, the first never
/// matched, and the registration read as absent — the arm that deletes and
/// rebuilds the directory. In `-z` output every attribute ends in NUL and a
/// record ends in a second NUL. A record that does not start with `worktree `
/// is a parse failure and an `Err`, never `Absent`. Paths are compared as the
/// bytes git printed: a record of someone else's worktree at a non-UTF-8 path
/// is a record that does not match, not a reason to fail every lease of the
/// repository.
///
/// A realpath match alone is not ownership: lease A's
/// worktree moved to lease B's path and linked back from A's reads, under
/// the realpath match, as a registration at B's path — with B's base as its
/// HEAD and B's `canonical_path` as its realpath, so both post-add checks
/// pass and B would start its worker in A's directory on A's branch. The
/// registration's identity is the path git recorded for it: `worktree add`
/// records the realpath of the path it is given, and the leaf does not exist
/// yet when it is added, so that is `canonicalize(parent)/<leaf>` — for a
/// lease of this slice, its `canonical_path`. A record at the lease path's
/// realpath is ours only when it is recorded under that name; recorded under
/// another name it is an alias of someone else's worktree,
/// [`GitWorktreeRegistration::Foreign`], which neither arm of provisioning
/// reuses, prunes or removes. The branch is not the identity: a worker
/// checks out and commits on whatever branch it delivers on, and its
/// worktree must still be its worktree at release and teardown.
pub(crate) fn worktree_registration_in(
    listing: &[u8],
    target: &WorkspaceLeaseTarget,
) -> Result<GitWorktreeRegistration> {
    let wanted_real = registration_path(&target.path);
    let wanted_named = named_registration_path(&target.path);
    let mut ours = None;
    let mut foreign = None;
    let mut rest = listing;
    while !rest.is_empty() {
        let (record, tail) = match rest.windows(2).position(|pair| pair == [0, 0]) {
            Some(end) => (&rest[..end], &rest[end + 2..]),
            None => (rest, &[][..]),
        };
        rest = tail;
        let mut attributes = record.split(|byte| *byte == 0);
        let listed = attributes
            .next()
            .and_then(|first| first.strip_prefix(b"worktree "))
            .ok_or_else(|| {
                CalmError::Internal(format!(
                    "git worktree list in {} printed a record without a `worktree ` line: {:?}",
                    target.repo_root.display(),
                    String::from_utf8_lossy(record)
                ))
            })?;
        let listed = Path::new(OsStr::from_bytes(listed));
        if registration_path(listed) != wanted_real {
            continue;
        }
        let mut prunable = false;
        let mut branch = None;
        for attribute in attributes {
            if attribute == b"prunable" || attribute.starts_with(b"prunable ") {
                prunable = true;
            } else if let Some(name) = attribute.strip_prefix(b"branch ") {
                branch = Some(String::from_utf8_lossy(name).into_owned());
            }
        }
        if listed != wanted_named {
            // Fail closed: an alias anywhere in the listing outranks our own
            // record, whichever git printed first.
            foreign.get_or_insert(GitWorktreeRegistration::Foreign {
                registered_as: listed.to_path_buf(),
                branch,
            });
            continue;
        }
        ours = Some(if prunable {
            GitWorktreeRegistration::Prunable
        } else {
            GitWorktreeRegistration::Present
        });
    }
    Ok(foreign.or(ours).unwrap_or(GitWorktreeRegistration::Absent))
}

/// The path git recorded, or would record, for a worktree added at `path`:
/// the parent's realpath joined with the leaf as named — the leaf itself is
/// never resolved (it does not exist when `worktree add` records it, and a
/// leaf that is a symlink later is a different question, answered by
/// [`clear_symlink_leaf_before_provision`]). The path as given when the
/// parent is gone.
pub(crate) fn named_registration_path(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map(|parent| parent.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

/// The refusal every path gives a [`GitWorktreeRegistration::Foreign`]
/// registration: the lease path resolves to someone else's worktree, named
/// by the path and branch git recorded it under, against the registration
/// this lease would own.
pub(crate) fn foreign_registration_refusal(
    target: &WorkspaceLeaseTarget,
    registered_as: &Path,
    branch: Option<&str>,
) -> CalmError {
    CalmError::Internal(format!(
        "lease path {} resolves to a worktree registered as {} on {}; \
         this lease's registration would be {} on refs/heads/{}",
        target.path.display(),
        registered_as.display(),
        branch.unwrap_or("a detached HEAD"),
        named_registration_path(&target.path).display(),
        target.branch
    ))
}

/// A lease leaf that is a symlink is never a registration of ours, whatever
/// it points at: following it would put the checkout, and later the removal,
/// into the link's target (an external directory, the main checkout). This is
/// the provisioning half of that rule, run before any registration or
/// emptiness check looks through the link. Unregistered (nothing resolves to
/// the link) → the link alone is unlinked and provisioning continues as for
/// an absent leaf; registered (the link resolves to a worktree of the
/// repository, on whatever branch) → refused, both arms, without looking at
/// the base.
pub(crate) fn clear_symlink_leaf_before_provision(target: &WorkspaceLeaseTarget) -> Result<()> {
    if !is_symlink_leaf(&target.path)? {
        return Ok(());
    }
    super::ensure_lease_owned_worktree_target(target)?;
    match super::git_worktree_registration(target)? {
        GitWorktreeRegistration::Absent => {
            unlink_symlink_leaf(&target.path)?;
            Ok(())
        }
        GitWorktreeRegistration::Present
        | GitWorktreeRegistration::Prunable
        | GitWorktreeRegistration::Foreign { .. } => Err(CalmError::Internal(format!(
            "workspace lease path {} is a symlink resolving to {}, a registered worktree of {}; \
                 a symlink leaf is never a valid registration",
            target.path.display(),
            registration_path(&target.path).display(),
            target.repo_root.display()
        ))),
    }
}

/// `symlink_metadata`, never `metadata`: the leaf itself, not what it points
/// at. Absent → false.
pub(crate) fn is_symlink_leaf(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_symlink()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(CalmError::Internal(format!(
            "inspect workspace lease path {}: {error}",
            path.display()
        ))),
    }
}

/// The removal half of the rule: a symlink leaf is unlinked — `remove_file`
/// on the link itself, never a `git worktree remove` or `remove_dir_all`
/// through it. `Ok(true)` when a link was removed; `Ok(false)` when the leaf
/// is not a symlink (absent, a directory, a file), which the caller then
/// handles as before.
pub(crate) fn unlink_symlink_leaf(path: &Path) -> Result<bool> {
    if !is_symlink_leaf(path)? {
        return Ok(false);
    }
    std::fs::remove_file(path).map_err(|error| {
        CalmError::Internal(format!(
            "unlink workspace lease symlink {}: {error}",
            path.display()
        ))
    })?;
    Ok(true)
}

/// `canonicalize(<parent>)/<card_id>` — the parent must already exist. The
/// realpath must be UTF-8 (`<parent>` may be a symlink into a directory that
/// is not, while `repo_root` and git's own output are): the value is stored
/// as TEXT and frozen into `tx_output.data` through `json!`, whose
/// `to_value(..).unwrap()` would panic inside the prepare transaction — the
/// op would then stay `Pending` and panic again on every drive instead of
/// failing. Refused here, the same way a leaf that is not UTF-8 is.
pub(crate) fn lease_canonical_path(parent: &Path, card_id: &str) -> Result<PathBuf> {
    let parent = std::fs::canonicalize(parent).map_err(|e| {
        CalmError::Internal(format!(
            "canonicalize workspace lease parent {}: {e}",
            parent.display()
        ))
    })?;
    utf8_path(&parent, "workspace lease parent realpath")?;
    Ok(parent.join(card_id))
}

/// git's stdout as [`printed_path`] reads it: the bytes git printed, minus
/// the one `\n` git appends, decoded by no one. A path may contain whitespace
/// (`git_common_dir` of a repository under `/home/u/my repos/`) and even a
/// newline (`rev-parse --git-common-dir` prints it literally, so the output
/// spans two lines and a "single line" rule would refuse a repository that
/// `worktree add` accepts), so this neither splits nor trims; the
/// single-token rule lives in the sha reader ([`resolve_head_base`]) only.
fn git_stdout_line(dir: &Path, args: &[&str]) -> Result<OsString> {
    let output = neige_git_command()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| {
            CalmError::Internal(format!(
                "spawn git {} in {}: {e}",
                args.join(" "),
                dir.display()
            ))
        })?;
    if !output.status.success() {
        return Err(super::git_failed(
            &format!("git {}", args.join(" ")),
            dir,
            &output,
        ));
    }
    Ok(printed_path(&output.stdout).into_os_string())
}

/// A path git printed on stdout (`--git-common-dir`, `--show-toplevel`),
/// kept as bytes until something canonicalizes or stores it: exactly the one
/// `\n` git appends is removed, nothing else. Decoding first would let a
/// byte that is not UTF-8 become U+FFFD, and `canonicalize` would then
/// resolve a sibling directory that happens to carry that name instead of
/// failing; stripping `\r\n` would cut the last byte off a name that ends in
/// `\r`. Both are paths git accepts and prints raw.
pub(crate) fn printed_path(stdout: &[u8]) -> PathBuf {
    let printed = stdout.strip_suffix(b"\n").unwrap_or(stdout);
    PathBuf::from(OsStr::from_bytes(printed))
}

/// What [`super::provision_workspace_worktree`] pins the worktree to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorktreeBase {
    /// A lease taken by this slice or later: `worktree add` gets `base_sha`
    /// as its commit-ish and every registration state ends in
    /// [`verify_worktree_base`].
    Pinned {
        base_sha: String,
        canonical_path: PathBuf,
    },
    /// Recovery of a worker op frozen before slice 1: its `tx_output` has
    /// `repo_root` / `slice_branch` but no `base_sha`, so there is nothing to
    /// pin to or check against — the pre-slice behaviour, unchanged (design
    /// D12 (d)). Not a choice a new op can make.
    LegacyUnpinned,
}

impl WorktreeBase {
    /// The spawn side's branch on the frozen `tx_output`: `base_sha` and
    /// `canonical_path` both present → `Pinned`, both absent → the frozen
    /// pre-slice-1 op → `LegacyUnpinned`; one without the other is an error.
    pub(crate) fn from_tx_output(output: &TxOutput, ctx: &str) -> Result<WorktreeBase> {
        let base_sha = output.output_optional_string("base_sha", ctx)?;
        let canonical_path = output.output_optional_string("canonical_path", ctx)?;
        match (base_sha, canonical_path) {
            (Some(base_sha), Some(canonical_path)) => Ok(WorktreeBase::Pinned {
                base_sha,
                canonical_path: PathBuf::from(canonical_path),
            }),
            (None, None) => Ok(WorktreeBase::LegacyUnpinned),
            _ => Err(CalmError::Internal(format!(
                "{ctx} tx_output carries one of base_sha / canonical_path without the other"
            ))),
        }
    }

    /// Production provisions from the frozen `tx_output` (`from_tx_output`);
    /// fixtures that hold the `LeaseBase` they just wrote pin from it directly.
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn from_lease_base(base: &LeaseBase) -> WorktreeBase {
        WorktreeBase::Pinned {
            base_sha: base.base_sha.clone(),
            canonical_path: base.canonical_path.clone(),
        }
    }

    /// The commit-ish `git worktree add -b <branch> <path>` is pinned to.
    pub(crate) fn push_commit_ish(&self, command: &mut Command) {
        if let WorktreeBase::Pinned { base_sha, .. } = self {
            command.arg(base_sha);
        }
    }
}

/// Identity before any destructive step. A lease names its
/// worktree twice: the lexical path the row stores and the realpath that
/// path resolved to when the row was written (`canonical_path`). Between the
/// two moments the path's parent — `.claude/worktrees` may be a symlink —
/// can be retargeted at another tree, where the same `<track>/<card>` is
/// someone else's registered worktree with uncommitted files, or an
/// unregistered directory. Every step that deletes (the stale-directory
/// cleanup before `worktree add`, `worktree remove --force`, `branch -D`,
/// `remove_dir_all`) is addressed by the lexical path, so each entry that can
/// reach a recorded identity checks it first and does nothing when the path
/// no longer resolves to it. This is the provisioning half: computed on the
/// parent (`canonicalize(parent)/<leaf>`), not the leaf, so it holds before
/// the leaf is looked at at all — a symlink leaf in the retargeted tree is
/// not unlinked either. `LegacyUnpinned` recorded nothing and skips.
pub(crate) fn verify_lease_path_identity_before_provision(
    target: &WorkspaceLeaseTarget,
    base: &WorktreeBase,
) -> Result<()> {
    let WorktreeBase::Pinned { canonical_path, .. } = base else {
        return Ok(());
    };
    let (Some(parent), Some(leaf)) = (target.path.parent(), target.path.file_name()) else {
        return Err(CalmError::Internal(format!(
            "workspace lease path {} has no parent or leaf",
            target.path.display()
        )));
    };
    let found = std::fs::canonicalize(parent)
        .map_err(|e| {
            CalmError::Internal(format!(
                "canonicalize workspace lease parent {}: {e}",
                parent.display()
            ))
        })?
        .join(leaf);
    if found != *canonical_path {
        return Err(CalmError::Internal(format!(
            "refused: lease path {} no longer resolves to its recorded worktree: \
             expected {}, found {}; nothing was removed",
            target.path.display(),
            canonical_path.display(),
            found.display()
        )));
    }
    Ok(())
}

/// The sweep half of the identity rule. A track teardown
/// enumerates `<repo_root>/.claude/worktrees/<track>` and removes every
/// lease-shaped entry, including entries no row names — those have no
/// per-entry identity to check, so the check the removal half makes per row
/// left them to `worktree remove --force` under a retargeted parent. The
/// identity is therefore checked once, on the track root, before anything is
/// enumerated: every captured row that recorded a `canonical_path` recorded
/// it as `canonicalize(track root)/<card>`, so the track root must still
/// resolve to that parent. A mismatch refuses the whole sweep — no entry
/// removal, no `branch -D`, no `worktree.removed`. Only rows whose lexical
/// parent is this track root constrain it; rows without a base recorded
/// nothing. A track root that no longer exists is not a mismatch: nothing
/// under it can be removed and the branch sweep is what remains of the
/// teardown (the idempotent second sweep after the first removed the root).
pub(crate) fn verify_track_root_identity_before_sweep(
    repo_root: &Path,
    track_id: &str,
    leases: &[WorkspaceLease],
) -> Result<()> {
    let track_root = repo_root.join(".claude").join("worktrees").join(track_id);
    let mut actual: Option<PathBuf> = None;
    for lease in leases {
        let Some(base) = lease.base.as_ref() else {
            continue;
        };
        if Path::new(&lease.path).parent() != Some(track_root.as_path()) {
            continue;
        }
        let Some(recorded) = base.canonical_path.parent() else {
            return Err(CalmError::Internal(format!(
                "refused: workspace lease {} recorded canonical_path {} without a parent",
                lease.lease_id,
                base.canonical_path.display()
            )));
        };
        let actual = match &actual {
            Some(actual) => actual,
            None => match std::fs::canonicalize(&track_root) {
                Ok(resolved) => actual.insert(resolved),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => {
                    return Err(CalmError::Internal(format!(
                        "canonicalize workspace track root {}: {error}",
                        track_root.display()
                    )));
                }
            },
        };
        if actual != recorded {
            return Err(CalmError::Internal(format!(
                "refused: track root {} no longer resolves to the recorded worktrees: \
                 expected {}, found {}; nothing was removed",
                track_root.display(),
                recorded.display(),
                actual.display()
            )));
        }
    }
    Ok(())
}

/// The removal half of the identity rule: a lease-aware removal (the
/// compensation step, the by-id release, the track sweep where a row names
/// the entry) refuses when `canonicalize(lease path)` exists and is not the
/// row's `canonical_path`. A path that does not exist has nothing to remove
/// and the caller's registration/branch cleanup runs as before; a row without
/// a base (written before the columns existed) recorded nothing and skips.
pub(crate) fn verify_lease_path_identity_before_removal(
    lease_path: &Path,
    base: Option<&LeaseBase>,
) -> Result<()> {
    let Some(base) = base else {
        return Ok(());
    };
    let actual = match std::fs::canonicalize(lease_path) {
        Ok(actual) => actual,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(CalmError::Internal(format!(
                "canonicalize workspace lease path {}: {error}",
                lease_path.display()
            )));
        }
    };
    if actual != base.canonical_path {
        return Err(CalmError::Internal(format!(
            "refused: lease path {} no longer resolves to its recorded worktree: \
             expected {}, found {}; nothing was removed",
            lease_path.display(),
            base.canonical_path.display(),
            actual.display()
        )));
    }
    Ok(())
}

/// The check every registration state of provisioning ends in: the worktree's
/// HEAD is `base_sha`, its realpath is `canonical_path`, and its HEAD is the
/// lease's slice branch (`symbolic-ref HEAD`; a worktree someone checked out
/// onto another branch, or detached, at the base commit is not the
/// lease's). Any mismatch is an `Internal` error naming expected and
/// found, which fails the spawn (the op ends `spawn-failed`, the task
/// `failed`); nothing is repaired here.
pub(crate) fn verify_worktree_base(
    target: &WorkspaceLeaseTarget,
    base: &WorktreeBase,
) -> Result<()> {
    let WorktreeBase::Pinned {
        base_sha,
        canonical_path,
    } = base
    else {
        return Ok(());
    };
    let head = resolve_head_base(&target.path)?;
    if head != *base_sha {
        return Err(CalmError::Internal(format!(
            "workspace worktree {} is not at the recorded base: expected {base_sha}, found {head}",
            target.path.display()
        )));
    }
    let actual = std::fs::canonicalize(&target.path).map_err(|e| {
        CalmError::Internal(format!(
            "canonicalize workspace worktree {}: {e}",
            target.path.display()
        ))
    })?;
    if actual != *canonical_path {
        return Err(CalmError::Internal(format!(
            "workspace worktree {} is not at the recorded path: expected {}, found {}",
            target.path.display(),
            canonical_path.display(),
            actual.display()
        )));
    }
    let head_ref = worktree_head_ref(&target.path)?;
    let expected_ref = format!("refs/heads/{}", target.branch);
    if head_ref.as_deref() != Some(expected_ref.as_str()) {
        return Err(CalmError::Internal(format!(
            "workspace worktree {} is not on the lease branch: expected {expected_ref}, found {}",
            target.path.display(),
            head_ref.as_deref().unwrap_or("a detached HEAD")
        )));
    }
    Ok(())
}

/// `git -C <worktree> symbolic-ref -q HEAD`: `Some(refs/heads/..)` on a
/// branch, `None` when HEAD is detached (`-q` exits 1 silently for exactly
/// that), `Err` for anything else.
pub(super) fn worktree_head_ref(worktree: &Path) -> Result<Option<String>> {
    let args = ["symbolic-ref", "-q", "HEAD"];
    let output = neige_git_command()
        .arg("-C")
        .arg(worktree)
        .args(args)
        .output()
        .map_err(|e| {
            CalmError::Internal(format!(
                "spawn git {} in {}: {e}",
                args.join(" "),
                worktree.display()
            ))
        })?;
    if output.status.success() {
        let printed = printed_path(&output.stdout);
        return utf8_path(&printed, "workspace worktree HEAD ref")
            .map(|name| Some(name.to_string()));
    }
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    Err(super::git_failed(
        &format!("git {}", args.join(" ")),
        worktree,
        &output,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_source_round_trips_all_values() {
        for source in [
            BaseSource::Head,
            BaseSource::Upstream,
            BaseSource::Commit,
            BaseSource::Attempt,
        ] {
            assert_eq!(BaseSource::from_column(source.as_column()).unwrap(), source);
        }
        assert_eq!(BaseSource::Head.as_column(), "head");
        assert_eq!(BaseSource::Upstream.as_column(), "upstream");
        assert_eq!(BaseSource::Commit.as_column(), "commit");
        assert_eq!(BaseSource::Attempt.as_column(), "attempt");
        let err = BaseSource::from_column("bogus").unwrap_err();
        assert!(matches!(err, CalmError::Internal(_)), "{err}");
        assert!(BaseSource::from_column("").is_err());
        assert!(BaseSource::from_column("HEAD").is_err());
    }

    /// The lossy rewrite is gone: a `LeaseBase` holding a non-UTF-8 path
    /// (no constructor produces one) is refused at the bind, never stored as
    /// a path that does not exist.
    #[test]
    fn bind_columns_refuses_a_non_utf8_path() {
        use std::os::unix::ffi::OsStringExt;
        let base = LeaseBase {
            base_sha: "abc".into(),
            base_source: BaseSource::Head,
            base_attempt_id: None,
            canonical_path: PathBuf::from(std::ffi::OsString::from_vec(b"/wt-\xff/c".to_vec())),
            git_common_dir: PathBuf::from("/repo/.git"),
        };
        let Err(err) = LeaseBase::bind_columns(sqlx::query("SELECT 1"), Some(&base)) else {
            panic!("a non-UTF-8 canonical_path must be refused at the bind");
        };
        assert!(err.to_string().contains("not UTF-8"), "{err}");
        assert!(LeaseBase::bind_columns(sqlx::query("SELECT 1"), None).is_ok());
    }

    #[test]
    fn worktree_base_from_tx_output_needs_both_or_neither() {
        let output = |data: serde_json::Value| {
            let mut output = TxOutput::new("card", None, serde_json::Value::Null);
            output.data = data;
            output
        };
        assert_eq!(
            WorktreeBase::from_tx_output(
                &output(serde_json::json!({"base_sha": "abc", "canonical_path": "/real/path"})),
                "test"
            )
            .unwrap(),
            WorktreeBase::Pinned {
                base_sha: "abc".into(),
                canonical_path: PathBuf::from("/real/path"),
            }
        );
        assert_eq!(
            WorktreeBase::from_tx_output(
                &output(serde_json::json!({"repo_root": "/r", "slice_branch": "b"})),
                "test"
            )
            .unwrap(),
            WorktreeBase::LegacyUnpinned
        );
        assert!(
            WorktreeBase::from_tx_output(&output(serde_json::json!({"base_sha": "abc"})), "test")
                .is_err()
        );
        assert!(
            WorktreeBase::from_tx_output(
                &output(serde_json::json!({"canonical_path": "/real/path"})),
                "test"
            )
            .is_err()
        );
    }
}
