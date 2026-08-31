//! #1147 S2 — managed workspace roots: path derivation + materialization.
//!
//! Design `docs/1147-workspace-design.md` D2/D3. A managed wave workspace is a
//! server-created, server-owned git repository at
//! `<workspace-root>/<cove_id>/<wave_id>`. Every step below is load-bearing and
//! was measured, not guessed — see the per-step comments.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{CalmError, Result};
use crate::model::{WaveWorkspace, WaveWorkspaceKind};
use crate::operation::workspace_lease::ensure_workspace_worktree_root_excluded;

/// Author stamped on the init commit. Explicit so the produced repository does
/// not inherit whatever `user.name` git derives from the host account (D3 step
/// 3: missing identity does *not* fail the commit, it silently pollutes it).
const INIT_COMMIT_AUTHOR_NAME: &str = "neige";
const INIT_COMMIT_AUTHOR_EMAIL: &str = "neige@localhost";
const INIT_COMMIT_MESSAGE: &str = "neige workspace init";

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
pub fn materialize_workspace(workspace: &WaveWorkspace) -> Result<()> {
    match workspace.kind {
        WaveWorkspaceKind::Managed => materialize_managed_workspace(Path::new(&workspace.path)),
        WaveWorkspaceKind::Attached => Ok(()),
    }
}

/// Create (or adopt) the managed git repository at `path`. Idempotent.
///
/// Steps are D3 verbatim:
///
/// 1. `mkdir -p`; the directory must be absent, empty, or an already
///    materialized workspace of ours. Anything else is a hard failure — we
///    never reuse a directory whose contents we did not create.
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
pub fn materialize_managed_workspace(path: &Path) -> Result<()> {
    materialize_managed_workspace_inner(path, InitCommit::Create)
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

fn materialize_managed_workspace_inner(path: &Path, init_commit: InitCommit) -> Result<()> {
    if !path.is_absolute() {
        return Err(CalmError::Internal(format!(
            "managed workspace path must be absolute: {}",
            path.display()
        )));
    }
    std::fs::create_dir_all(path).map_err(|error| {
        CalmError::Internal(format!(
            "materialize workspace: create {}: {error}",
            path.display()
        ))
    })?;

    let already_ours = workspace_is_git_repo(path);
    if !already_ours && dir_has_entries(path)? {
        // "Non-empty and not ours" — hard failure. Reusing it risks handing a
        // worker somebody else's directory, and `git init` over foreign
        // content silently adopts it.
        return Err(CalmError::Internal(format!(
            "materialize workspace: {} is not empty and is not a neige-managed repository; \
             refusing to reuse it",
            path.display()
        )));
    }

    if !already_ours {
        run_git(
            path,
            &[
                "-c",
                "init.templateDir=",
                "-c",
                "init.defaultBranch=main",
                "init",
                &path.to_string_lossy(),
            ],
            "git init",
        )?;
    }

    if init_commit == InitCommit::Create && !git_head_resolves(path) {
        run_git(
            path,
            &[
                "-C",
                &path.to_string_lossy(),
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
            ],
            "git commit --allow-empty",
        )?;
    }

    ensure_workspace_worktree_root_excluded(path)?;
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

/// "Already a repository whose root is exactly `path`". A `.git` *directory*
/// check alone would be true for any subdirectory of a user repo, which is the
/// case we must refuse.
fn workspace_is_git_repo(path: &Path) -> bool {
    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let toplevel = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if toplevel.is_empty() {
        return false;
    }
    // `--show-toplevel` resolves symlinks; compare canonicalized forms so
    // `/tmp` → `/private/tmp` style indirection does not read as foreign.
    match (
        std::fs::canonicalize(Path::new(&toplevel)),
        std::fs::canonicalize(path),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => Path::new(&toplevel) == path,
    }
}

fn git_head_resolves(path: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_git(path: &Path, args: &[&str], what: &str) -> Result<()> {
    let output = Command::new("git").args(args).output().map_err(|error| {
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
