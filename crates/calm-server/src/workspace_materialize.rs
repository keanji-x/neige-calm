//! #1147 S2 — managed workspace roots: path derivation + materialization.
//!
//! Design `docs/1147-workspace-design.md` D2/D3. A managed track workspace is a
//! server-created, server-owned git repository at
//! `<workspace-root>/<area_id>/<track_id>`. Every step below is load-bearing and
//! was measured, not guessed — see the per-step comments.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use crate::error::{CalmError, Result};
use crate::model::{TrackWorkspace, TrackWorkspaceKind};
use crate::operation::workspace_lease::ensure_workspace_worktree_root_excluded;

/// Author stamped on the init commit. Explicit so the produced repository does
/// not inherit whatever `user.name` git derives from the host account (D3 step
/// 3: a missing identity does *not* fail the commit, it silently pollutes it).
const INIT_COMMIT_AUTHOR_NAME: &str = "neige";
const INIT_COMMIT_AUTHOR_EMAIL: &str = "neige@localhost";
const INIT_COMMIT_MESSAGE: &str = "neige workspace init";

/// Ownership marker, written **inside `.git/`** so it is structurally invisible
/// to design D4's "is anything on disk" predicate — a marker in the work tree
/// would show up as `?? …` forever, which is the same trap that made writing a
/// `.gitignore` illegal (D3 step 4).
///
/// `git init` preserves unknown files already present in `.git/`, which is what
/// lets us write the marker *before* `init` runs. See
/// [`materialize_managed_workspace`] for why that ordering matters.
const OWNER_MARKER: &str = "neige-workspace";

/// Git environment variables removed before every spawn on this path.
///
/// They fall into three groups, and the third is the dangerous one:
///
/// * **Redirect the repository out from under us** — `GIT_DIR`,
///   `GIT_WORK_TREE`, `GIT_COMMON_DIR`, `GIT_INDEX_FILE`,
///   `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`,
///   `GIT_NAMESPACE`, `GIT_CEILING_DIRECTORIES`. Each was measured to make
///   materialization fail outright.
/// * **Poison the commit's identity** — `GIT_AUTHOR_*`/`GIT_COMMITTER_*`. An
///   empty `GIT_COMMITTER_NAME` or a malformed `GIT_AUTHOR_DATE` fails the
///   commit; a valid one silently mis-attributes it.
/// * **Defeat the `-c` isolation entirely** — `GIT_TEMPLATE_DIR`,
///   `GIT_CONFIG_GLOBAL`, `GIT_CONFIG_SYSTEM`, `GIT_CONFIG`,
///   `GIT_CONFIG_COUNT`.
///
/// That last group is why this list exists at all, and `GIT_TEMPLATE_DIR` is
/// the sharpest edge in it. Git's precedence is
/// `--template` > `GIT_TEMPLATE_DIR` > `init.templateDir`, so D3 step 2's
/// `-c init.templateDir=` is **outranked**: with `GIT_TEMPLATE_DIR` set,
/// `git init` copies the template's `hooks/` into the new repository. The
/// init commit itself survives (it runs `--no-verify`), but every later git
/// command a worker runs inside that workspace — `worktree add`, codex's own
/// commits — would execute the injected hook. Config-file isolation without
/// environment isolation is not isolation; it just moves the hole.
const HOSTILE_GIT_ENV: &[&str] = &[
    // Repository redirection.
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_INDEX_VERSION",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    // Identity.
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
    // Outrank the `-c` overrides. `GIT_TEMPLATE_DIR` beats
    // `-c init.templateDir=` and injects hooks; the config vars beat every
    // other `-c` this module relies on.
    "GIT_TEMPLATE_DIR",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
];

/// A `git` command with the ambient environment's repository-redirecting and
/// identity-overriding variables removed. Every git spawn on the
/// materialization path must go through this.
pub(crate) fn neige_git_command() -> Command {
    let mut command = Command::new("git");
    for key in HOSTILE_GIT_ENV {
        command.env_remove(key);
    }
    command
}

/// `<workspace-root>/<area_id>/<track_id>`.
///
/// Directory names are ids, never slugs: D2 also fixes "renaming a track does
/// not move its directory", so a title-derived name would start lying the
/// first time the track is renamed.
pub fn managed_workspace_path(workspace_root: &Path, area_id: &str, track_id: &str) -> PathBuf {
    workspace_root.join(area_id).join(track_id)
}

/// Short, stable digest of a workspace path, for use inside an idempotency key
/// (#1147 S2 red-team B1, extended to child-track bootstraps in S4).
///
/// Both call sites submit a `planner-harness-start` payload that contains a `cwd`.
/// The operation runtime treats "same idempotency key, different payload hash"
/// as a permanent conflict, and operation rows are never deleted — so a key
/// that does not name the path turns any re-point of that path into a
/// permanent 409 on every later submit. Truncated because the key is also a
/// human-readable diagnostic string; a collision could only merge two
/// operations that already agree on every other key component.
pub(crate) fn workspace_key_digest(path: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    hex::encode(hasher.finalize())[..16].to_string()
}

/// Materialize `workspace` if — and only if — it is `Managed`.
///
/// `Attached` workspaces point at directories the *user* owns: never created,
/// never `git init`-ed, never written to.
///
/// # Why [`validate_attached_workspace`] is NOT called from here
///
/// It was, for one gate run. Design D3 says the attached branch "only
/// validates", and this is the single contract point every create entry shares,
/// so it looks like the right home. **Measured: 202 tests fail.** Attached
/// tracks are the default shape of nearly every fixture in the suite, and they
/// point at strings (`/parent-cwd`, `""`, a bare tempdir) rather than at real
/// Git work trees — because until #1147 S3 nothing ever looked. Making this the
/// enforcement point therefore is not "add a check", it is "rewrite every
/// attached fixture in the tree to build a real repository", which is a slice
/// of its own.
///
/// So validation lives at the two entry points where a **user** names a
/// directory — `POST /api/tracks`'s attached branch and
/// `PATCH /api/tracks/{id}` — and those are the two this slice opens. The
/// kernel-derived attached paths (an area chat track adopting an existing
/// `area_folders` claim, a child track inheriting an attached parent) are NOT
/// validated; see the design's 已知缺口 N18.
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

/// #1147 S3 — design D3's other half: *"Attached 创建只做校验：绝对路径、目录
/// 存在、是 Git 仓库"*. Until this slice only the first third existed
/// (`create_track`'s `starts_with('/')`), which was survivable while the new FE
/// had no way to attach anything. This slice adds that way, so the gap had to
/// close with it.
///
/// # Why it is checked here and not left to the worker
///
/// Without this, attaching a path that does not exist — or exists but is not a
/// repository — is accepted with a 201, and the first `kind: codex` task then
/// dies inside `git_repo_root_for_track_cwd` leaving nothing but `spawn-failed`
/// in `tasks.status_detail`. That is issue #1147's opening paragraph,
/// reproduced by the FE entry point this slice adds. The error text below is
/// git's own, surfaced verbatim.
///
/// # "Is a Git work tree", not "is a repository root"
///
/// `rev-parse --show-toplevel` succeeds from a subdirectory too, and that is
/// deliberate: the worker path derives the repository root itself
/// (`git_repo_root_for_track_cwd`), so a subdirectory is a directory work can
/// actually happen in. Refusing it would reject a legitimate cwd for a reason
/// nothing downstream cares about.
///
/// Returns `BadRequest`: every one of these is the caller naming the wrong
/// directory, not the server failing.
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
    Ok(())
}

/// Create (or re-adopt) the managed git repository at `path`. Idempotent, and
/// safe to call concurrently for the same path.
///
/// Steps are D3 verbatim, plus the three properties the S2 red team measured
/// were missing:
///
/// 1. `mkdir -p`, then classify by **our own ownership marker**, not by "is
///    this a git repository". A third-party repository sitting on the derived
///    path answers yes to the latter, and adopting it means S5 later
///    `remove_dir_all`s somebody's real work.
/// 2. `git -c init.templateDir= -c init.defaultBranch=main init <path>`.
///    The `-c` flags go *before* the subcommand: `git init -c …` is rejected
///    by git 2.39.5 (`unknown switch 'c'`).
/// 3. An **empty initial commit**. Without it `git worktree add` fails outright
///    (`not a valid object name: 'HEAD'`), so the very first codex worker in a
///    managed track cannot start — the whole point of this slice.
///    `commit.gpgsign` and `core.hooksPath` are forced off for this one
///    invocation because a global `commit.gpgsign=true` makes the empty commit
///    fail hard, and a global hooks path would run user hooks inside a
///    server-owned repo.
/// 4. Exclude `.claude/worktrees/` via `.git/info/exclude` — NOT `.gitignore`.
///    A committed/untracked `.gitignore` would make D4's "nothing on disk"
///    predicate permanently false (`?? .gitignore`), i.e. a managed workspace
///    would be un-repointable from second zero.
/// 5. The realised directory must still be **physically** inside
///    `workspace_root`. `create_dir_all` follows symlinks, and both this
///    module's invariant test and D8's recycle guard compare paths
///    *lexically* — a symlink therefore lets a managed workspace's real
///    contents sit anywhere on the filesystem while every prefix check passes.
pub fn materialize_managed_workspace(
    workspace_root: &Path,
    path: &Path,
    track_id: &str,
) -> Result<()> {
    materialize_managed_workspace_inner(workspace_root, path, track_id, InitCommit::Create)
}

/// Mutation seam for §5 test 1. Production has exactly one caller
/// ([`materialize_managed_workspace`]) and it always passes
/// [`InitCommit::Create`]; the test passes [`InitCommit::Skip`] to run the
/// real production path minus step 3 and prove `git worktree add` then dies
/// with `not a valid object name`. Deliberately private: it is a mutation
/// handle, not a supported configuration.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InitCommit {
    Create,
    /// Never constructed outside `#[cfg(test)]` — that is the point, and the
    /// `allow` is scoped to non-test builds so a future production caller
    /// would still be a compile-time-visible change rather than a silent one.
    #[cfg_attr(not(test), allow(dead_code))]
    Skip,
}

/// Per-path serialization (red-team B4).
///
/// The launchpad's `ensure` is *expected* to run concurrently — it already
/// carries a unique-index race retry — and materialization runs outside the
/// transaction, spawning three git commands with no lock of their own.
/// Four-way concurrency reproduced `cannot lock config file .git/config` and a
/// spurious "not a neige-managed repository", and a process that dies
/// mid-`init` used to leave a directory that was non-empty, unmarked, and
/// therefore permanently un-materializable — a 500 on every subsequent
/// `ensure`, forever.
///
/// Keyed by path so unrelated tracks never contend. Entries are never evicted:
/// one `Arc<Mutex<()>>` per track workspace over a process lifetime is bounded
/// by the number of tracks, and dropping entries would need reference counting
/// that buys nothing here.
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
    // Declared *after* `_guard`, so it is dropped *before* the lock releases:
    // the probe therefore measures exactly the window the lock protects.
    // Test-only; production compiles nothing for it.
    #[cfg(test)]
    let _overlap = tests::OverlapProbe::enter(path);

    std::fs::create_dir_all(path).map_err(|error| {
        CalmError::Internal(format!(
            "materialize workspace: create {}: {error}",
            path.display()
        ))
    })?;

    match read_owner_marker(path)? {
        // Ours, and for this track. Re-running the steps below is idempotent,
        // and — because the marker proves we created everything here — it is
        // also safe to repair a half-built directory left by a crash.
        Some(owner) if owner == track_id => {}
        // Ours, but for a *different* track. Should be unreachable (the path
        // contains the track id), so treat it as corruption rather than
        // something to clean up.
        Some(owner) => {
            return Err(CalmError::Internal(format!(
                "materialize workspace: {} is the managed workspace of track `{owner}`, \
                 not `{track_id}`; refusing to take it over",
                path.display()
            )));
        }
        None if dir_has_entries(path)? => {
            // Unmarked and non-empty. This is somebody else's directory — very
            // possibly a real git repository, which is exactly the case a
            // "is it a git repo?" test would have waved through. Do not `git
            // init` it, do not append to its `.git/info/exclude`, do not put a
            // `neige` commit in it. S5 deletes managed directories wholesale;
            // adopting this one would arm that deletion against a user's work.
            return Err(CalmError::Internal(format!(
                "materialize workspace: {} is not empty and carries no neige ownership \
                 marker, so it is not ours; refusing to reuse it",
                path.display()
            )));
        }
        None => {
            // Empty and unclaimed: claim it *before* writing anything else, so
            // a crash at any later point leaves a directory we can prove is
            // ours and repair, instead of an unmarked non-empty brick.
            //
            // #1427: the claim itself has to be crash-atomic, or the very act
            // of making it opens the window it exists to close. See
            // [`claim_owner_marker`].
            claim_owner_marker(path, track_id)?;
        }
    }

    // Steady state — ours, and a resolvable `HEAD` means `init` and the initial
    // commit both completed — costs exactly one `rev-parse`. That matters
    // because the worker lease path calls this on every acquisition (red-team
    // B5) purely so an un-materialized track repairs itself rather than
    // spawn-failing forever.
    //
    // Otherwise run `git init`: it is idempotent on a healthy repository and
    // rebuilds a half-built one, which is how a directory left behind by a
    // crash mid-materialize gets repaired instead of bricking every later call.
    // Safe precisely because the marker proved the directory is ours.
    if !git_head_resolves(path) {
        // Third leg of D3 contract (3): mutex + marker + **clear our own
        // half-built state**. The first two alone still brick the workspace.
        //
        // Measured: marker present, `.git/config.lock` left behind by a
        // process killed mid-`init` (this host's `neige-killer.log` shows that
        // is not a theoretical risk) — `git init` then fails with
        // `could not lock config file` on *every* subsequent call, forever.
        // Same permanent 500 the contract exists to abolish, entered through
        // a lock file instead of an unmarked directory. On the launchpad that
        // is a permanently dead Today panel.
        //
        // Safe precisely because the marker proved the directory is ours AND
        // `HEAD` does not resolve, so there is no repository state worth
        // preserving and no live worker that could own these locks.
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
        // Re-assert the marker: `git init` preserves unknown files under an
        // existing `.git/`, but rewriting it costs nothing and covers the case
        // where the marker was lost along with a partially wiped `.git`.
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

    assert_physically_inside_root(workspace_root, path)?;
    Ok(())
}

/// Red-team B3 — the realised directory must be inside the root *after*
/// symlinks are resolved, not just lexically.
///
/// `create_dir_all` happily walks a symlink, so `<root>/<area>` being a link to
/// `/somewhere/else` yields a stored path that passes every `starts_with`
/// check while the repository — and everything a worker writes into it — lives
/// outside the tree S5 believes it owns. Checked after materialization so the
/// directory that gets validated is the one that actually exists.
fn assert_physically_inside_root(workspace_root: &Path, path: &Path) -> Result<()> {
    let real_path = std::fs::canonicalize(path).map_err(|error| {
        CalmError::Internal(format!(
            "materialize workspace: canonicalize {}: {error}",
            path.display()
        ))
    })?;
    // The root is canonicalized at boot (`AppState::new`), but canonicalize it
    // again so a caller that passed a non-canonical root cannot make this
    // check pass by accident.
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

/// Remove `*.lock` files anywhere under `.git/`. See the call site for why
/// this is both necessary and safe.
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

/// Observation seam for #1427's crash-atomicity test. Called at every point
/// where the ownership claim has just changed what a `read_dir` of `<path>`
/// would report, i.e. every point at which process death would freeze the
/// workspace directory in whatever state it is currently in. In non-test
/// builds it compiles to nothing.
#[inline]
fn claim_crash_point(path: &Path) {
    #[cfg(test)]
    tests::claim_crash_point(path);
    #[cfg(not(test))]
    let _ = path;
}

/// Name of the staging directory the ownership claim is assembled in, before
/// it is published over `<path>` with a single `rename`. Dot-prefixed so it can
/// never collide with a track id (ids are never dot-prefixed — the same
/// reasoning [`crate::workspace_recycle::TRASH_DIR_NAME`] relies on), and it
/// lives in `<path>`'s **parent** so a crash leaves the debris outside the
/// directory whose emptiness the fence reads.
const CLAIM_STAGING_PREFIX: &str = ".neige-claim-";

/// Claim `path` for `track_id` **atomically** (#1427).
///
/// The claim is the one write on this path that must not be interruptible: it
/// is the step that decides whether `<path>` is a directory we can prove is
/// ours (repairable on every later call) or an unmarked non-empty directory
/// that the fence at [`materialize_managed_workspace_inner`] refuses *forever*
/// — which, since #1384, permanently poisons the create's `Idempotency-Key`.
///
/// A temp-file-plus-`rename` **inside** `<path>/.git` does not close that
/// window: it still needs `<path>/.git` to exist first, and death right there
/// is construction 1 of the issue verbatim. So the unit that gets renamed is
/// the whole `.git` directory, assembled out of the way and published in one
/// `rename(2)`:
///
/// * `rename` replaces an **empty** destination directory atomically (POSIX:
///   `newpath` may be an existing empty directory), and `<path>` is empty on
///   this arm — that was just measured by `dir_has_entries`. A non-empty
///   `<path>` makes the rename fail with `ENOTEMPTY` instead of clobbering it,
///   which is the fail-closed direction: this arm never overwrites bytes.
/// * The staging directory sits in `<path>`'s parent, so it is on the same
///   filesystem (`rename` is only atomic within one) and, crucially, is *not*
///   an entry of `<path>`: debris from a crashed claim cannot itself trip the
///   "non-empty and unmarked" refusal.
/// * The staging name is fixed per track, so a crashed claim's debris is
///   reclaimed by the next attempt rather than accumulating. Only this track's
///   own claims can ever use that name.
///
/// The marker keeps its historical location and format
/// (`<path>/.git/neige-workspace`, `"<track id>\n"`), which is load-bearing:
/// [`crate::workspace_recycle`] reads it independently as guard 3 of the
/// recycle fence. Workspaces created before #1427 are therefore unaffected —
/// there is nothing to migrate and no second location to read.
///
/// # Durability
///
/// The marker file and both staging directories are `fsync`ed before the
/// publishing `rename`, and `<path>`'s parent after it. A `rename` whose
/// operands were never synced can still be lost to a power cut, which would
/// resurrect exactly the state this function exists to abolish; the claim runs
/// once per workspace, so the cost is not on any hot path. The re-init and
/// commit steps that follow are deliberately *not* synced: they are
/// reconstructible by the repair path, which the marker is what unlocks.
///
/// # What a crashed claim leaves behind
///
/// Death between staging and the publishing `rename` leaves
/// `.neige-claim-<track>` in the area directory. The next materialize of this
/// track reclaims it — debris exists only while `<path>` is still empty and
/// unmarked, which is the arm that routes here, and the first thing this
/// function does is `remove_dir_all(&staging)`. A track that is never retried
/// keeps its entry: one per track that crashed mid-claim and was never
/// retried, and it stays there.
///
/// The one reader that acts on it is `remove_empty_area_dir`
/// (`workspace_recycle.rs:698`): its `rmdir` answers `ENOTEMPTY`, so
/// `area_dir_removed` comes back `false` and the area directory is left in
/// place — classed as cosmetic at that call site's own doc comment. Every
/// other reader of that directory either filters it (the listing route drops
/// leading-dot names, `routes/fs.rs:184-187`) or never reaches it.
///
/// That residue is not a new cost. Before #1427 the same crash left
/// `<path>/.git` unmarked, which blocked that same `rmdir` identically *and*
/// poisoned the create's `Idempotency-Key`; what this version leaves is a
/// strict subset of what it replaced.
///
/// # Two claimers, one track
///
/// The staging name is shared per track, so a second claimer entering this
/// function can `remove_dir_all` a first one's staging mid-flight. The per-path
/// mutex in [`materialize_managed_workspace_inner`] makes that unreachable
/// within one instance; a second **process** holds no such mutex.
///
/// #1430 drove both interleavings through `claim_crash_point` and **pinned
/// them** (`tests.rs`), which corrected what this paragraph used to claim:
///
/// * If the peer wipes a staging that is still being assembled, the loser's
///   marker write fails and it returns `Internal` having damaged nothing —
///   `remove_dir_all` is called on `staging` and nowhere else in this module,
///   and `<path>` is never a `remove_dir_all` target here. What `<path>` holds
///   afterwards is the *winner's* correctly marked workspace, so the next
///   materialization succeeds
///   (`a_claim_that_loses_the_staging_race_fails_closed_onto_the_winners_marker`).
/// * If the peer wipes a staging that is **fully assembled** — marker written,
///   both fsyncs done — and `create_dir_all` puts a bare `.git` back under the
///   same name, the first claimer renames *that* onto `<path>` and returns
///   `Ok`. `<path>` is then non-empty and unmarked: the brick state this
///   function exists to abolish, reached through a peer instead of through
///   process death
///   (`a_concurrent_claim_can_make_its_peer_publish_an_unmarked_workspace`).
///   This is a **known defect**, recorded as
///   `docs/design-1384-track-idempotency.md` §9 KNOWN GAP 13; the test
///   characterizes it and must be inverted by the fix, not deleted.
///
/// So: crash-atomic against process death (#1427), **not** atomic against a
/// concurrent second claimer on the same staging name.
fn claim_owner_marker(path: &Path, track_id: &str) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        CalmError::Internal(format!(
            "materialize workspace: {} has no parent directory to stage the \
             ownership claim in",
            path.display()
        ))
    })?;
    let staging = parent.join(format!(
        "{CLAIM_STAGING_PREFIX}{}",
        sanitize_path_segment(track_id)
    ));

    // Anything under this name is debris from one of *our* earlier claims for
    // *this* track: nothing else ever writes it, and the per-path mutex is
    // held. Removing it is what keeps repeated crashes from accumulating
    // staging directories in the area folder.
    match std::fs::remove_dir_all(&staging) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(CalmError::Internal(format!(
                "materialize workspace: clear stale ownership claim {}: {error}",
                staging.display()
            )));
        }
    }

    let staged_git = staging.join(".git");
    std::fs::create_dir_all(&staged_git).map_err(|error| {
        CalmError::Internal(format!(
            "materialize workspace: create ownership claim {}: {error}",
            staged_git.display()
        ))
    })?;
    claim_crash_point(path);
    write_marker_file(&staged_git.join(OWNER_MARKER), track_id)?;
    claim_crash_point(path);
    fsync_dir(&staged_git)?;
    fsync_dir(&staging)?;
    claim_crash_point(path);

    std::fs::rename(&staging, path).map_err(|error| {
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

/// Reduce `segment` to characters that are unambiguous in a file name. Track
/// ids are already used as path components by [`managed_workspace_path`], so
/// this is belt-and-braces; a collision between two sanitized ids would only
/// mean two of *our* claims sharing a staging name, which the mutex and the
/// `rename`'s fail-closed behaviour already handle.
fn sanitize_path_segment(segment: &str) -> String {
    segment
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Write `track_id` to `target` and `fsync` it. See [`claim_owner_marker`] for
/// why the sync is here.
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

/// `fsync` a directory, so a rename or creation inside it survives a power
/// cut and not merely a process death.
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
    // Publish by `rename`, never by writing the destination in place (#1427
    // construction 2): `std::fs::write` truncates first, so a crash inside it
    // leaves a torn — in practice empty — marker at the published path, which
    // reads back as an owner that is not this track and lands on the
    // foreign-owner arm's permanent refusal. The temp file is a sibling inside
    // `.git/`, so the rename stays within one filesystem and any debris is
    // invisible to the fence (which reads `<path>`, not `<path>/.git`).
    //
    // Unlike [`claim_owner_marker`] this is only ever reached with `.git`
    // already present, so it does not — and cannot — close construction 1 on
    // its own.
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
