//! Managed workspace roots: path derivation + materialization of server-owned git repositories at
//! `<workspace-root>/<area_id>/<track_id>`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use crate::error::{CalmError, Result};
use crate::model::{TrackWorkspace, TrackWorkspaceKind};
use crate::operation::workspace_lease::{
    ensure_git_exclude_entry, ensure_workspace_worktree_root_excluded,
};

/// Explicit so the init commit does not inherit the host account's identity.
const INIT_COMMIT_AUTHOR_NAME: &str = "neige";
const INIT_COMMIT_AUTHOR_EMAIL: &str = "neige@localhost";
const INIT_COMMIT_MESSAGE: &str = "neige workspace init";

/// Ownership marker, written inside `.git/` so it never shows up as `?? …` in the work tree;
/// `git init` preserves unknown files already under `.git/`.
const OWNER_MARKER: &str = "neige-workspace";

/// Git environment variables removed before every spawn: they redirect the repository, poison the commit
/// identity, or (`GIT_TEMPLATE_DIR`, `GIT_CONFIG*`) outrank the `-c` overrides and inject hooks.
const HOSTILE_GIT_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_INDEX_VERSION",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    // Discovery from a `-C <worktree>`: the lease module's checks read the
    // worktree git found from that directory.
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_PREFIX",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
    // These outrank the `-c` overrides.
    "GIT_TEMPLATE_DIR",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
];

/// A `git` command with the hostile ambient variables removed; every git spawn on this path must use it.
pub(crate) fn neige_git_command() -> Command {
    let mut command = Command::new("git");
    for key in HOSTILE_GIT_ENV {
        command.env_remove(key);
    }
    command
}

/// `<workspace-root>/<area_id>/<track_id>`; ids, not slugs, so renaming a track never moves its directory.
pub fn managed_workspace_path(workspace_root: &Path, area_id: &str, track_id: &str) -> PathBuf {
    workspace_root.join(area_id).join(track_id)
}

/// Short digest of a workspace path for an idempotency key: a key that does not name the path turns any
/// re-point into a permanent 409, since operation rows are never deleted.
pub(crate) fn workspace_key_digest(path: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    hex::encode(hasher.finalize())[..16].to_string()
}

/// Materialize `workspace` if — and only if — it is `Managed`; attached directories are the user's and are
/// never created or written to. Attached validation lives at the user-facing entry points instead.
pub fn materialize_workspace(
    workspace: &TrackWorkspace,
    workspace_root: &Path,
    track_id: &str,
) -> Result<()> {
    match workspace.kind {
        TrackWorkspaceKind::Managed => {
            materialize_managed_workspace(workspace_root, Path::new(&workspace.path), track_id)
        }
        TrackWorkspaceKind::Attached => Ok(()),
    }
}

/// Attached admission: absolute, exists, inside a Git work tree (a subdirectory is fine — the worker derives
/// the root itself), and the slice-branch namespace is free. Every failure is `BadRequest`.
pub fn validate_attached_workspace(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(CalmError::BadRequest(format!(
            "attached workspace: path must be absolute (start with `/`); got `{}`",
            path.display()
        )));
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(CalmError::BadRequest(format!(
                "attached workspace: `{}` is not a directory",
                path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(CalmError::BadRequest(format!(
                "attached workspace: `{}` does not exist. Neige never creates an \
                 attached directory — it is yours, so it has to be there already.",
                path.display()
            )));
        }
        Err(error) => {
            return Err(CalmError::BadRequest(format!(
                "attached workspace: cannot read `{}`: {error}",
                path.display()
            )));
        }
    }
    let output = neige_git_command()
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|error| {
            CalmError::BadRequest(format!(
                "attached workspace: cannot run git in `{}`: {error}",
                path.display()
            ))
        })?;
    if !output.status.success() {
        return Err(CalmError::BadRequest(format!(
            "attached workspace: `{}` is not inside a Git work tree, so no worker \
             could ever get a workspace lease there. git said: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    ensure_slice_branch_namespace_is_free(path)?;
    Ok(())
}

/// The ref every worker's slice branch lives under; a test ties it to `workspace_slice_branch_for`.
const SLICE_BRANCH_NAMESPACE_REF: &str = "refs/heads/neige";

/// Refuse a repository that already holds `refs/heads/neige`: git cannot hold that file and
/// `refs/heads/neige/<track>/<card>` at once, so the first worker would die in `git worktree add`.
/// Only the exact ref is checked; a `show-ref` exit other than 0/1 is refused (fail closed).
fn ensure_slice_branch_namespace_is_free(path: &Path) -> Result<()> {
    let output = neige_git_command()
        .arg("-C")
        .arg(path)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            SLICE_BRANCH_NAMESPACE_REF,
        ])
        .output()
        .map_err(|error| {
            CalmError::BadRequest(format!(
                "attached workspace: cannot run git in `{}`: {error}",
                path.display()
            ))
        })?;
    match output.status.code() {
        Some(1) => Ok(()),
        Some(0) => Err(CalmError::BadRequest(format!(
            "attached workspace: `{}` has a branch named `neige`, and \
             `{SLICE_BRANCH_NAMESPACE_REF}` collides with the branch namespace \
             every worker gets its own slice branch under \
             (`neige/<track>/<card>`). Git cannot hold both, so the first \
             worker on this track would die in `git worktree add`. Rename or \
             delete that branch (`git branch -m neige <another-name>`), then \
             attach again.",
            path.display()
        ))),
        // Fail closed: the question was not answered.
        other => Err(CalmError::BadRequest(format!(
            "attached workspace: cannot tell whether `{}` already holds \
             `{SLICE_BRANCH_NAMESPACE_REF}` (git show-ref exited with {}); \
             refusing rather than letting the first worker find out. git said: \
             {}",
            path.display(),
            other
                .map(|code| code.to_string())
                .unwrap_or_else(|| "a signal".to_string()),
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

/// Create (or re-adopt) the managed git repository at `path`; idempotent and safe to call concurrently.
/// Classify by our own ownership marker, never by "is this a git repository"; the empty initial commit is
/// required or `git worktree add` fails; exclude via `.git/info/exclude`, never `.gitignore`.
pub fn materialize_managed_workspace(
    workspace_root: &Path,
    path: &Path,
    track_id: &str,
) -> Result<()> {
    materialize_managed_workspace_inner(workspace_root, path, track_id, InitCommit::Create)
}

/// Mutation seam: production always passes `Create`; a test passes `Skip` to prove `git worktree add` then dies.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InitCommit {
    Create,
    /// Never constructed outside `#[cfg(test)]`; the `allow` is scoped to non-test builds on purpose.
    #[cfg_attr(not(test), allow(dead_code))]
    Skip,
}

/// Per-path serialization: concurrent materialization reproduced `cannot lock config file`. Entries are
/// never evicted; one mutex per track workspace is bounded by the number of tracks.
fn path_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = locks.lock().unwrap_or_else(|e| e.into_inner());
    guard.entry(path.to_path_buf()).or_default().clone()
}

fn materialize_managed_workspace_inner(
    workspace_root: &Path,
    path: &Path,
    track_id: &str,
    init_commit: InitCommit,
) -> Result<()> {
    if !path.is_absolute() {
        return Err(CalmError::Internal(format!(
            "managed workspace path must be absolute: {}",
            path.display()
        )));
    }
    let lock = path_lock(path);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    // Declared after `_guard` so it is dropped before the lock releases. Test-only.
    #[cfg(test)]
    let _overlap = tests::OverlapProbe::enter(path);

    std::fs::create_dir_all(path).map_err(|error| {
        CalmError::Internal(format!(
            "materialize workspace: create {}: {error}",
            path.display()
        ))
    })?;

    match read_owner_marker(path)? {
        // Ours, for this track: re-running below is idempotent and repairs a half-built directory.
        Some(owner) if owner == track_id => {}
        // Ours, but for a different track: unreachable (the path contains the track id), so treat as corruption.
        Some(owner) => {
            return Err(CalmError::Internal(format!(
                "materialize workspace: {} is the managed workspace of track `{owner}`, \
                 not `{track_id}`; refusing to take it over",
                path.display()
            )));
        }
        None if dir_has_entries(path)? => {
            // Unmarked and non-empty: somebody else's directory. Adopting it would arm the wholesale recycle deletion against a user's work.
            return Err(CalmError::Internal(format!(
                "materialize workspace: {} is not empty and carries no neige ownership \
                 marker, so it is not ours; refusing to reuse it",
                path.display()
            )));
        }
        None => {
            // Empty and unclaimed: claim before writing anything else, so a crash leaves a directory we can prove is ours.
            claim_owner_marker(path, track_id)?;
        }
    }

    // Steady state costs one `rev-parse` (the lease path calls this on every acquisition); otherwise `git init`
    // is idempotent on a healthy repo and rebuilds a half-built one.
    if !git_head_resolves(path) {
        // A `.git/config.lock` left by a process killed mid-`init` makes every later `git init` fail forever;
        // safe to clear because the marker proved the directory is ours and `HEAD` does not resolve.
        clear_our_stale_git_locks(path)?;
        run_git(
            path,
            neige_git_command()
                .args([
                    "-c",
                    "init.templateDir=",
                    "-c",
                    "init.defaultBranch=main",
                    "init",
                ])
                .arg(path),
            "git init",
        )?;
        // Re-assert the marker in case it was lost with a partially wiped `.git`.
        write_owner_marker(path, track_id)?;
    }

    if init_commit == InitCommit::Create && !git_head_resolves(path) {
        run_git(
            path,
            neige_git_command().arg("-C").arg(path).args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=",
                "-c",
                &format!("user.name={INIT_COMMIT_AUTHOR_NAME}"),
                "-c",
                &format!("user.email={INIT_COMMIT_AUTHOR_EMAIL}"),
                "commit",
                "--allow-empty",
                "--no-verify",
                "-m",
                INIT_COMMIT_MESSAGE,
            ]),
            "git commit --allow-empty",
        )?;
    }

    ensure_workspace_worktree_root_excluded(path)?;
    // `.neige/` is the server's own subtree inside the work tree; excluded so a worker's `git add -A` never sees it.
    ensure_git_exclude_entry(path, crate::planner_attachments::NEIGE_GIT_EXCLUDE_ENTRY)?;

    assert_physically_inside_root(workspace_root, path)?;
    Ok(())
}

/// `create_dir_all` follows symlinks, so a symlink under the root would pass every lexical prefix check
/// while the repository lives outside the tree the recycle path believes it owns.
fn assert_physically_inside_root(workspace_root: &Path, path: &Path) -> Result<()> {
    let real_path = std::fs::canonicalize(path).map_err(|error| {
        CalmError::Internal(format!(
            "materialize workspace: canonicalize {}: {error}",
            path.display()
        ))
    })?;
    // Canonicalize the root again so a non-canonical root cannot make this pass by accident.
    let real_root = std::fs::canonicalize(workspace_root).map_err(|error| {
        CalmError::Internal(format!(
            "materialize workspace: canonicalize workspace root {}: {error}",
            workspace_root.display()
        ))
    })?;
    if !real_path.starts_with(&real_root) {
        return Err(CalmError::Internal(format!(
            "materialize workspace: {} resolves to {}, which is outside the managed \
             workspace root {}. Two things reach this: a symlink under the root \
             (which would put worker output where the recycle path's prefix assertion \
             cannot see it), or a workspace root that has MOVED since this track was \
             created — `CALM_WORKSPACE_ROOT` or `$HOME` changed — in which case the \
             stored path is simply no longer under the configured root and this track \
             has no migration path (issue #1147 N4).",
            path.display(),
            real_path.display(),
            real_root.display()
        )));
    }
    Ok(())
}

fn clear_our_stale_git_locks(path: &Path) -> Result<()> {
    fn walk(dir: &Path) -> Result<()> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(CalmError::Internal(format!(
                    "materialize workspace: read {}: {error}",
                    dir.display()
                )));
            }
        };
        for entry in entries {
            let entry = entry.map_err(|error| {
                CalmError::Internal(format!(
                    "materialize workspace: read {}: {error}",
                    dir.display()
                ))
            })?;
            let entry_path = entry.path();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                walk(&entry_path)?;
            } else if entry_path.extension().is_some_and(|ext| ext == "lock") {
                match std::fs::remove_file(&entry_path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(CalmError::Internal(format!(
                            "materialize workspace: remove stale lock {}: {error}",
                            entry_path.display()
                        )));
                    }
                }
            }
        }
        Ok(())
    }
    walk(&path.join(".git"))
}

fn owner_marker_path(path: &Path) -> PathBuf {
    path.join(".git").join(OWNER_MARKER)
}

fn read_owner_marker(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(owner_marker_path(path)) {
        Ok(contents) => Ok(Some(contents.trim().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(CalmError::Internal(format!(
            "materialize workspace: read ownership marker in {}: {error}",
            path.display()
        ))),
    }
}

/// Observation seam for the crash-atomicity test; compiles to nothing in non-test builds.
#[inline]
fn claim_crash_point(path: &Path) {
    #[cfg(test)]
    tests::claim_crash_point(path);
    #[cfg(not(test))]
    let _ = path;
}

/// Prefix of the per-attempt staging directory the claim is assembled in, placed in `<path>`'s parent so a
/// crash leaves debris outside the directory whose emptiness the fence reads. Dot-prefixed: never a track id.
const CLAIM_STAGING_PREFIX: &str = ".neige-claim-";

/// How old a staging directory must be before a later claim treats it as debris; deleting a live one is fail-closed.
const STALE_CLAIM_STAGING_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Unique per attempt across threads and processes: pid + wall-clock nanos + a process-wide counter.
fn claim_attempt_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let seq = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}-{seq}", std::process::id())
}

/// Best-effort, age-gated sweep of this track's stale staging directories; a live peer's staging is never a
/// candidate, and no claimer ever recreates another's name, so the worst case fails closed.
fn reclaim_stale_claim_staging(parent: &Path, track_segment: &str, ours: &Path) {
    let fixed = format!("{CLAIM_STAGING_PREFIX}{track_segment}");
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // `track_segment` is alphanumeric-or-`_`, so `-` is an unambiguous separator.
        let mine = name == fixed
            || name
                .strip_prefix(&fixed)
                .is_some_and(|suffix| suffix.starts_with('-'));
        if !mine || entry.path() == ours {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age >= STALE_CLAIM_STAGING_AGE);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Claim `path` for `track_id` atomically: assemble the whole `.git` in a staging directory and publish it with
/// one `rename(2)`, so process death can never leave an unmarked non-empty directory the fence refuses forever.
fn claim_owner_marker(path: &Path, track_id: &str) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        CalmError::Internal(format!(
            "materialize workspace: {} has no parent directory to stage the \
             ownership claim in",
            path.display()
        ))
    })?;
    let track_segment = sanitize_path_segment(track_id);
    let staging = parent.join(format!(
        "{CLAIM_STAGING_PREFIX}{track_segment}-{}",
        claim_attempt_id()
    ));

    // Debris from this track's earlier claims; never this call's own staging.
    reclaim_stale_claim_staging(parent, &track_segment, &staging);

    let staged_git = staging.join(".git");
    std::fs::create_dir_all(&staged_git).map_err(|error| {
        CalmError::Internal(format!(
            "materialize workspace: create ownership claim {}: {error}",
            staged_git.display()
        ))
    })?;

    let published = assemble_and_publish_claim(path, parent, &staging, &staged_git, track_id);
    if published.is_err() {
        // Our own attempt's name, so this can only delete what this call built.
        let _ = std::fs::remove_dir_all(&staging);
    }
    published
}

/// Split out so every failure after `staged_git` exists routes through one cleanup; the `fsync` ordering must not change.
fn assemble_and_publish_claim(
    path: &Path,
    parent: &Path,
    staging: &Path,
    staged_git: &Path,
    track_id: &str,
) -> Result<()> {
    claim_crash_point(path);
    write_marker_file(&staged_git.join(OWNER_MARKER), track_id)?;
    claim_crash_point(path);
    fsync_dir(staged_git)?;
    fsync_dir(staging)?;
    claim_crash_point(path);

    std::fs::rename(staging, path).map_err(|error| {
        CalmError::Internal(format!(
            "materialize workspace: publish ownership claim {} onto {}: {error}",
            staging.display(),
            path.display()
        ))
    })?;
    claim_crash_point(path);
    fsync_dir(parent)?;
    claim_crash_point(path);
    Ok(())
}

/// Reduce `segment` to characters unambiguous in a file name.
fn sanitize_path_segment(segment: &str) -> String {
    segment
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn write_marker_file(target: &Path, track_id: &str) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(target).map_err(|error| {
        CalmError::Internal(format!(
            "materialize workspace: create ownership marker {}: {error}",
            target.display()
        ))
    })?;
    file.write_all(format!("{track_id}\n").as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            CalmError::Internal(format!(
                "materialize workspace: write ownership marker {}: {error}",
                target.display()
            ))
        })
}

/// `fsync` a directory so a rename inside it survives a power cut, not merely a process death.
fn fsync_dir(path: &Path) -> Result<()> {
    std::fs::File::open(path)
        .and_then(|dir| dir.sync_all())
        .map_err(|error| {
            CalmError::Internal(format!(
                "materialize workspace: fsync {}: {error}",
                path.display()
            ))
        })
}

fn write_owner_marker(path: &Path, track_id: &str) -> Result<()> {
    let marker = owner_marker_path(path);
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            CalmError::Internal(format!(
                "materialize workspace: create {}: {error}",
                parent.display()
            ))
        })?;
    }
    claim_crash_point(path);
    // Publish by `rename`: `std::fs::write` truncates first, so a crash inside it leaves a torn marker that
    // reads as a foreign owner and lands on the permanent refusal.
    let staged = marker.with_extension("staged");
    write_marker_file(&staged, track_id)?;
    claim_crash_point(path);
    std::fs::rename(&staged, &marker).map_err(|error| {
        CalmError::Internal(format!(
            "materialize workspace: publish ownership marker {}: {error}",
            marker.display()
        ))
    })?;
    claim_crash_point(path);
    if let Some(parent) = marker.parent() {
        fsync_dir(parent)?;
    }
    Ok(())
}

fn dir_has_entries(path: &Path) -> Result<bool> {
    let mut entries = std::fs::read_dir(path).map_err(|error| {
        CalmError::Internal(format!(
            "materialize workspace: read {}: {error}",
            path.display()
        ))
    })?;
    match entries.next() {
        Some(Ok(_)) => Ok(true),
        Some(Err(error)) => Err(CalmError::Internal(format!(
            "materialize workspace: read {}: {error}",
            path.display()
        ))),
        None => Ok(false),
    }
}

fn git_head_resolves(path: &Path) -> bool {
    neige_git_command()
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_git(path: &Path, command: &mut Command, what: &str) -> Result<()> {
    let output = command.output().map_err(|error| {
        CalmError::Internal(format!(
            "materialize workspace: spawn {what} for {}: {error}",
            path.display()
        ))
    })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(CalmError::Internal(format!(
        "materialize workspace: {what} for {} failed ({}): {}{}",
        path.display(),
        output.status,
        stderr.trim(),
        stdout.trim(),
    )))
}

#[cfg(test)]
mod tests;
