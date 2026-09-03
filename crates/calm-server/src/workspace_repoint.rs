//! #1147 S3 — is this workspace still untouched, and may it be moved?
//!
//! The predicate design §更换与冻结 settles on is deliberately **not** an
//! enumeration of "who might have written here". It asks the filesystem one
//! question — *is there anything on disk?* — through three git commands that
//! together cover every writer the earlier drafts kept forgetting: the planner
//! harness runs `workspace-write` from its first message
//! (`planner_harness_start_adapter.rs`), MCP forge actions run git directly in the
//! workspace, and worker leases add worktrees. None of them has to register
//! anywhere; their output shows up here.
//!
//! ```text
//! git status --porcelain --ignored   → empty
//! git rev-list --count --all         → exactly 1   (the materialize baseline)
//! git worktree list                  → exactly 1 line (the main worktree)
//! ```
//!
//! Each clause is load bearing and none subsumes another:
//!
//! * `--ignored` is what makes worker output count. `.git/info/exclude` hides
//!   `.claude/worktrees/` from a plain `status`, which is the whole point of
//!   writing the exclude there (S2) — so without `--ignored` a workspace full
//!   of worker output reads as clean. Measured in S2 and re-stated in the
//!   design: an *empty* `.claude/worktrees/` directory does **not** make
//!   `--ignored` non-empty, so this does not make the predicate vacuously
//!   false from second zero.
//! * `rev-list --count --all == 1` catches commits that leave no working-tree
//!   trace: a slice branch the worker committed on, or a `git stash` (stashes
//!   are commits reachable from `--all`). The `1` is the empty init commit S2
//!   creates precisely so that this comparison has a baseline.
//! * `worktree list` catches a live lease worktree whose files live outside
//!   this directory. It also protects the move itself: after a `rename` the
//!   `<wt>/.git` ↔ `<repo>/.git/worktrees/<n>/gitdir` pointers are absolute
//!   and dangle in both directions, so a workspace with worktrees cannot be
//!   moved into the trash and still be a usable repository.
//!
//! # Fail-closed
//!
//! Any command that cannot be spawned, exits non-zero, or produces output this
//! module cannot parse is [`PristineVerdict::Dirty`]. "Cannot tell" is not
//! "clean": the consequence of a wrong `Pristine` is that a directory with
//! work in it is renamed into `.trash`, which is the one outcome this slice
//! exists to prevent.

use std::path::Path;
use std::process::Command;

use crate::workspace_materialize::neige_git_command;

/// The commit count a freshly materialized managed workspace has: exactly the
/// one empty init commit from `materialize_managed_workspace`.
const MATERIALIZE_BASELINE_COMMITS: &str = "1";

/// Result of the "is anything on disk" predicate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PristineVerdict {
    /// All three clauses hold.
    Pristine,
    /// One clause did not hold, or could not be evaluated. `check` names the
    /// clause; `detail` is operator-facing and goes into the 409 body.
    Dirty { check: &'static str, detail: String },
}

impl PristineVerdict {
    pub fn is_pristine(&self) -> bool {
        matches!(self, PristineVerdict::Pristine)
    }

    /// The 409 message for a non-pristine workspace.
    pub fn conflict_message(&self, path: &Path) -> String {
        match self {
            PristineVerdict::Pristine => String::new(),
            PristineVerdict::Dirty { check, detail } => format!(
                "workspace {} is no longer empty ({check}: {detail}); a workspace can only be \
                 changed before any work has happened in it",
                path.display()
            ),
        }
    }
}

/// Run the three-clause predicate against `path`.
///
/// Cheap enough to run twice per re-point (design §更换与冻结 step 2 requires
/// exactly that): three short-lived git processes on a repository with one
/// commit and no working files.
pub fn workspace_pristine(path: &Path) -> PristineVerdict {
    let status = match git_stdout(path, &["status", "--porcelain", "--ignored"]) {
        Ok(out) => out,
        Err(detail) => {
            return PristineVerdict::Dirty {
                check: "git status --porcelain --ignored",
                detail,
            };
        }
    };
    if !status.trim().is_empty() {
        return PristineVerdict::Dirty {
            check: "git status --porcelain --ignored",
            detail: first_line(&status),
        };
    }

    let commits = match git_stdout(path, &["rev-list", "--count", "--all"]) {
        Ok(out) => out,
        Err(detail) => {
            return PristineVerdict::Dirty {
                check: "git rev-list --count --all",
                detail,
            };
        }
    };
    if commits.trim() != MATERIALIZE_BASELINE_COMMITS {
        return PristineVerdict::Dirty {
            check: "git rev-list --count --all",
            detail: format!(
                "expected exactly {MATERIALIZE_BASELINE_COMMITS} commit (the materialize \
                 baseline), found `{}`",
                commits.trim()
            ),
        };
    }

    let worktrees = match git_stdout(path, &["worktree", "list"]) {
        Ok(out) => out,
        Err(detail) => {
            return PristineVerdict::Dirty {
                check: "git worktree list",
                detail,
            };
        }
    };
    let lines: Vec<&str> = worktrees.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() != 1 {
        return PristineVerdict::Dirty {
            check: "git worktree list",
            detail: format!(
                "expected exactly 1 worktree (the main one), found {}: {}",
                lines.len(),
                lines.join(" | ")
            ),
        };
    }

    PristineVerdict::Pristine
}

/// `git -C <path> <args>` with the hostile-environment scrub the whole #1147
/// git surface uses, returning stdout or an operator-facing failure string.
///
/// Sharing [`neige_git_command`] with materialization is not cosmetic: a
/// `GIT_DIR` or `GIT_CEILING_DIRECTORIES` inherited from the server's
/// environment would silently point these three commands at a *different*
/// repository, and this predicate's answer is what authorises a rename.
fn git_stdout(path: &Path, args: &[&str]) -> std::result::Result<String, String> {
    let mut command: Command = neige_git_command();
    let output = command
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|error| {
            format!(
                "spawn `git {}` in {}: {error}",
                args.join(" "),
                path.display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "`git {}` in {} failed ({}): {}",
            args.join(" "),
            path.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn first_line(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests;
