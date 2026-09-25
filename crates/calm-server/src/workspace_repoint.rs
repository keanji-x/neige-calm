//! Is this managed workspace still untouched (and therefore movable)? Three git commands: `status --porcelain
//! --ignored` empty (`--ignored` because worker output is excluded), `rev-list --count --all == 1` (stashes and
//! slice-branch commits count), `worktree list` has one line (a live worktree's absolute pointers would dangle
//! after a rename). Any command that fails or cannot be parsed is `Dirty`: "cannot tell" is never "clean".

use std::path::Path;
use std::process::Command;

use crate::workspace_materialize::isolated_git_command;

/// Exactly the one empty init commit from `materialize_managed_workspace`.
const MATERIALIZE_BASELINE_COMMITS: &str = "1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PristineVerdict {
    Pristine,
    /// `check` names the clause; `detail` is operator-facing and goes into the 409 body.
    Dirty {
        check: &'static str,
        detail: String,
    },
}

impl PristineVerdict {
    pub fn is_pristine(&self) -> bool {
        matches!(self, PristineVerdict::Pristine)
    }

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

/// Cheap enough to run twice per re-point.
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

/// [`isolated_git_command`]: an inherited `GIT_DIR` would silently point these commands at a
/// different repository, and this predicate's answer is what authorises a rename; `status` also runs
/// the repository's fsmonitor and clean filters, which must not see the kernel's environment.
fn git_stdout(path: &Path, args: &[&str]) -> std::result::Result<String, String> {
    let mut command: Command = isolated_git_command();
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
