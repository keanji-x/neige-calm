//! Git path and checkout probes.
use crate::bounded_git::{BoundedOutput, GIT_BOUND};
use crate::command::neige_git_command;
use crate::error::{Error, Result};
use crate::git::{GitWorktreeRegistration, WorkspaceLeaseTarget};
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};
/// The `&str` of a path the lease row stores and `tx_output` freezes, or an
/// `Err` naming it: nothing downstream may see a lossy rewrite (the row would
/// then name a path that does not exist) or a `json!` panic. Applied to every
/// path a reader hands the prepare tx: the canonical parent, the common dir,
/// and the toplevel (`repo_root` — a UTF-8 track cwd can be a
/// symlink into a directory that is not, and `--show-toplevel` prints the
/// physical path, so `repo_root` is UTF-8 only if the toplevel reader says so).
pub fn utf8_path<'a>(path: &'a Path, what: &str) -> Result<&'a str> {
    path.to_str().ok_or_else(|| {
        Error::Internal(format!(
            "{what} {} is not UTF-8; a lease is refused rather than recorded lossily",
            path.display()
        ))
    })
}

/// `git -C <dir> rev-parse --verify HEAD^{commit}`: the status is judged
/// before the output is used. The base a worker lease records, and the read
/// side of the spawn check.
pub fn resolve_head_base(repo_root: &Path) -> Result<String> {
    let args = ["rev-parse", "--verify", "HEAD^{commit}"];
    let printed = git_stdout_line(repo_root, &args)?;
    let sha = printed
        .to_str()
        .filter(|sha| !sha.is_empty() && !sha.contains(char::is_whitespace))
        .ok_or_else(|| {
            Error::Internal(format!(
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
/// path is not UTF-8) — refused.
pub fn lease_git_common_dir(workspace_path: &Path) -> Result<PathBuf> {
    let printed = git_stdout_line(
        workspace_path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common_dir = std::fs::canonicalize(&printed).map_err(|e| {
        Error::Internal(format!(
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
/// `.claude/worktrees` the lexical path never equals the listed one.
/// `canonicalize` when the path exists; for one that does not (a pruned
/// registration whose directory is gone) the parent's realpath with the leaf
/// as named; the path as given when even the parent is gone. Applied to the
/// listed and the target path alike, so a leaf that is a symlink resolves the
/// same on both sides and reads as the registration it points at.
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
/// A realpath match alone is not ownership: another worktree moved to the
/// target path and linked back reads, under the realpath match, as a
/// registration at the target. The registration's identity is the path git
/// recorded for it: `worktree add` records the realpath of the path it is
/// given, and the leaf does not exist yet when it is added, so that is
/// `canonicalize(parent)/<leaf>`. A record at the target's realpath is ours
/// only when it is recorded under that name; recorded under another name it
/// is an alias of someone else's worktree, [`GitWorktreeRegistration::Foreign`],
/// which is never reused, pruned or removed.
pub(crate) fn worktree_registration_in(
    listing: &[u8],
    repo_root: &Path,
    path: &Path,
) -> Result<GitWorktreeRegistration> {
    let wanted_real = registration_path(path);
    let wanted_named = named_registration_path(path);
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
                Error::Internal(format!(
                    "git worktree list in {} printed a record without a `worktree ` line: {:?}",
                    repo_root.display(),
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
/// never resolved (it does not exist when `worktree add` records it). The
/// path as given when the parent is gone.
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
) -> Error {
    Error::Internal(format!(
        "lease path {} resolves to a worktree registered as {} on {}; \
         this lease's registration would be {} on refs/heads/{}",
        target.path.display(),
        registered_as.display(),
        branch.unwrap_or("a detached HEAD"),
        named_registration_path(&target.path).display(),
        target.branch
    ))
}

/// `symlink_metadata`, never `metadata`: the leaf itself, not what it points
/// at. Absent → false.
pub(crate) fn is_symlink_leaf(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_symlink()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(Error::Internal(format!(
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
        Error::Internal(format!(
            "unlink workspace lease symlink {}: {error}",
            path.display()
        ))
    })?;
    Ok(true)
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
        .bounded_output(std::time::Instant::now() + GIT_BOUND)
        .map_err(|e| {
            Error::Internal(format!(
                "spawn git {} in {}: {e}",
                args.join(" "),
                dir.display()
            ))
        })?;
    if !output.status.success() {
        return Err(crate::git::git_failed(
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
pub fn printed_path(stdout: &[u8]) -> PathBuf {
    let printed = stdout.strip_suffix(b"\n").unwrap_or(stdout);
    PathBuf::from(OsStr::from_bytes(printed))
}

/// The spawn check (#1830 S2 D3): the worker's checkout `path` has HEAD `base_sha`, its realpath
/// is `canonical_path`, and its HEAD is on `branch` (`symbolic-ref HEAD`; a checkout someone
/// switched to another branch, or detached, at the base commit is not the worker's). Any mismatch
/// is an `Internal` error naming expected and found, which fails the spawn (the op ends
/// `spawn-failed`, the task `failed`); nothing is created or repaired here.
pub fn verify_worktree_base(
    path: &Path,
    branch: &str,
    base_sha: &str,
    canonical_path: &Path,
) -> Result<()> {
    let head = resolve_head_base(path)?;
    if head != base_sha {
        return Err(Error::Internal(format!(
            "workspace worktree {} is not at the recorded base: expected {base_sha}, found {head}",
            path.display()
        )));
    }
    let actual = std::fs::canonicalize(path).map_err(|e| {
        Error::Internal(format!(
            "canonicalize workspace worktree {}: {e}",
            path.display()
        ))
    })?;
    if actual != canonical_path {
        return Err(Error::Internal(format!(
            "workspace worktree {} is not at the recorded path: expected {}, found {}",
            path.display(),
            canonical_path.display(),
            actual.display()
        )));
    }
    let head_ref = worktree_head_ref(path)?;
    let expected_ref = format!("refs/heads/{branch}");
    if head_ref.as_deref() != Some(expected_ref.as_str()) {
        return Err(Error::Internal(format!(
            "workspace worktree {} is not on the worker branch: expected {expected_ref}, found {}",
            path.display(),
            head_ref.as_deref().unwrap_or("a detached HEAD")
        )));
    }
    Ok(())
}

/// `git -C <worktree> symbolic-ref -q HEAD`: `Some(refs/heads/..)` on a
/// branch, `None` when HEAD is detached (`-q` exits 1 silently for exactly
/// that), `Err` for anything else.
pub fn worktree_head_ref(worktree: &Path) -> Result<Option<String>> {
    let args = ["symbolic-ref", "-q", "HEAD"];
    let output = neige_git_command()
        .arg("-C")
        .arg(worktree)
        .args(args)
        .bounded_output(std::time::Instant::now() + GIT_BOUND)
        .map_err(|e| {
            Error::Internal(format!(
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
    Err(crate::git::git_failed(
        &format!("git {}", args.join(" ")),
        worktree,
        &output,
    ))
}
