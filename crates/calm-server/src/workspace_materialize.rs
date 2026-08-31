//! #1147 S2 — managed workspace roots: path derivation + materialization.
//!
//! Design `docs/1147-workspace-design.md` D2/D3. A managed wave workspace is a
//! server-created, server-owned git repository at
//! `<workspace-root>/<cove_id>/<wave_id>`. Every step below is load-bearing and
//! was measured, not guessed — see the per-step comments.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use crate::error::{CalmError, Result};
use crate::model::{WaveWorkspace, WaveWorkspaceKind};
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

/// `<workspace-root>/<cove_id>/<wave_id>`.
///
/// Directory names are ids, never slugs: D2 also fixes "renaming a wave does
/// not move its directory", so a title-derived name would start lying the
/// first time the wave is renamed.
pub fn managed_workspace_path(workspace_root: &Path, cove_id: &str, wave_id: &str) -> PathBuf {
    workspace_root.join(cove_id).join(wave_id)
}

/// Materialize `workspace` if — and only if — it is `Managed`.
///
/// `Attached` workspaces point at directories the *user* owns. D3: attached
/// gets validated, never created, never `git init`-ed, never written to.
pub fn materialize_workspace(
    workspace: &WaveWorkspace,
    workspace_root: &Path,
    wave_id: &str,
) -> Result<()> {
    match workspace.kind {
        WaveWorkspaceKind::Managed => {
            materialize_managed_workspace(workspace_root, Path::new(&workspace.path), wave_id)
        }
        WaveWorkspaceKind::Attached => Ok(()),
    }
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
///    managed wave cannot start — the whole point of this slice.
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
    wave_id: &str,
) -> Result<()> {
    materialize_managed_workspace_inner(workspace_root, path, wave_id, InitCommit::Create)
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
/// Keyed by path so unrelated waves never contend. Entries are never evicted:
/// one `Arc<Mutex<()>>` per wave workspace over a process lifetime is bounded
/// by the number of waves, and dropping entries would need reference counting
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
    wave_id: &str,
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
        // Ours, and for this wave. Re-running the steps below is idempotent,
        // and — because the marker proves we created everything here — it is
        // also safe to repair a half-built directory left by a crash.
        Some(owner) if owner == wave_id => {}
        // Ours, but for a *different* wave. Should be unreachable (the path
        // contains the wave id), so treat it as corruption rather than
        // something to clean up.
        Some(owner) => {
            return Err(CalmError::Internal(format!(
                "materialize workspace: {} is the managed workspace of wave `{owner}`, \
                 not `{wave_id}`; refusing to take it over",
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
            write_owner_marker(path, wave_id)?;
        }
    }

    // Steady state — ours, and a resolvable `HEAD` means `init` and the initial
    // commit both completed — costs exactly one `rev-parse`. That matters
    // because the worker lease path calls this on every acquisition (red-team
    // B5) purely so an un-materialized wave repairs itself rather than
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
        write_owner_marker(path, wave_id)?;
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
/// `create_dir_all` happily walks a symlink, so `<root>/<cove>` being a link to
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
             cannot see it), or a workspace root that has MOVED since this wave was \
             created — `CALM_WORKSPACE_ROOT` or `$HOME` changed — in which case the \
             stored path is simply no longer under the configured root and this wave \
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

fn write_owner_marker(path: &Path, wave_id: &str) -> Result<()> {
    let marker = owner_marker_path(path);
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            CalmError::Internal(format!(
                "materialize workspace: create {}: {error}",
                parent.display()
            ))
        })?;
    }
    std::fs::write(&marker, format!("{wave_id}\n")).map_err(|error| {
        CalmError::Internal(format!(
            "materialize workspace: write ownership marker {}: {error}",
            marker.display()
        ))
    })
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
