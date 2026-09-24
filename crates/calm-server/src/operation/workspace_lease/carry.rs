//! The carry branch of the lease base (#1785 S2, design §4.5). A `calm.task.replace` successor
//! whose receipt names a candidate starts from a kernel carry commit `C'`: the candidate merged
//! onto the base `U` the ordinary resolution chose (the upstream, or HEAD when the checkout is
//! ahead of it or has none), with `U` as its only parent. Two
//! bounded `git` runs inside the prepare transaction, object store only (no worktree, no ref):
//!
//! 1. `git merge-tree --write-tree --name-only --no-messages -z <U> <cand>` (the two-argument
//!    form git 2.38+ has; git picks `merge-base(U, cand)`, the lineage's upstream part, so every
//!    earlier carry in a chain is kept). Exit 0 prints `<tree>\0`; exit 1 with `<tree>\0<path>\0…`
//!    is a conflict; anything else is infrastructure (a missing `<cand>` exits 1 printing nothing).
//! 2. `git commit-tree <tree> -p <U>` under a fixed kernel identity dated from the receipt, so the
//!    same receipt over the same `U` always yields the same `C'`.
//!
//! The lease row then records `base_sha = C'`, `base_source = 'attempt'` and the source attempt.

use std::path::Path;
use std::time::Duration;

use super::WorkspaceLeaseTarget;
use super::base::{BaseSource, LeaseBase, resolve_lease_base};
use crate::error::{CalmError, Result};
use crate::operation::Tx;
use crate::plugin_host::child_process::{BoundedRunError, run_bounded};
use crate::task_replace::receipt::{CarryPlan, carry_plan_tx};
use crate::workspace_materialize::neige_git_command;

/// Total bound on both git runs of one carry.
pub(crate) const CARRY_TIMEOUT: Duration = Duration::from_secs(4);
const CARRY_OUTPUT_CAP: usize = 1024 * 1024;
const CARRY_AUTHOR_NAME: &str = "neige kernel";
const CARRY_AUTHOR_EMAIL: &str = "kernel@neige.invalid";
/// The only variables of the server's environment a carry git run inherits.
const CARRY_INHERITED_ENV: [&str; 3] = ["PATH", "HOME", "XDG_CONFIG_HOME"];

/// What the worker prompt says about a carried worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CarryNotice {
    pub candidate_sha: String,
    /// The commit the candidate was merged onto (`C'^1`) and where it came from.
    pub base_sha: String,
    pub base_source: BaseSource,
}

impl CarryNotice {
    /// What the merged-onto commit is: the ordinary base resolution picks the upstream, or the
    /// attached checkout's HEAD when it is ahead of the upstream or has none.
    fn base_label(&self) -> &'static str {
        match self.base_source {
            BaseSource::Upstream => "the upstream",
            BaseSource::Head => "the attached checkout's HEAD",
            BaseSource::Commit => "the pinned commit",
            BaseSource::Attempt => "the carried base",
        }
    }

    /// The fixed line appended to a carried attempt's worker prompt.
    pub(crate) fn render(&self) -> String {
        include_str!("../../../prompts/worker/carry-notice.md")
            .trim_end()
            .replace("{candidate_sha}", &self.candidate_sha)
            .replace("{base_label}", self.base_label())
            .replace("{base_sha}", &self.base_sha)
    }
}

/// [`resolve_lease_base`], then the carry branch for an attempt whose replacement receipt names a
/// candidate. The one base resolution every worker prepare runs.
pub(crate) async fn resolve_task_lease_base_tx(
    tx: &mut Tx<'_>,
    target: &WorkspaceLeaseTarget,
    attempt_id: &str,
) -> Result<(LeaseBase, Option<CarryNotice>)> {
    let base = resolve_lease_base(target)?;
    let Some(plan) = carry_plan_tx(tx, attempt_id).await? else {
        return Ok((base, None));
    };
    let carry_sha = carry_commit(&target.repo_root, &base.base_sha, &plan).await?;
    let notice = CarryNotice {
        candidate_sha: plan.candidate_sha,
        base_sha: base.base_sha.clone(),
        base_source: base.base_source,
    };
    Ok((
        LeaseBase {
            base_sha: carry_sha,
            base_source: BaseSource::Attempt,
            base_attempt_id: Some(plan.source_attempt_id),
            ..base
        },
        Some(notice),
    ))
}

/// How `merge-tree` ended.
#[derive(Debug, PartialEq, Eq)]
enum Merged {
    Clean { tree: String },
    Conflict { paths: Vec<String> },
}

fn is_oid(token: &str) -> bool {
    matches!(token.len(), 40 | 64) && token.bytes().all(|b| b.is_ascii_hexdigit())
}

fn infra(why: impl std::fmt::Display) -> CalmError {
    CalmError::Conflict(crate::task_replace::carry_failure(
        crate::task_replace::CarryFailure::Infra,
        &why.to_string(),
    ))
}

/// Read `merge-tree -z --name-only --no-messages` by exit status and token shape alone (never by
/// its localized messages).
fn classify_merge(output: &std::process::Output) -> Result<Merged> {
    let tokens: Vec<&str> = output
        .stdout
        .split(|b| *b == 0)
        .filter(|token| !token.is_empty())
        .map(|token| std::str::from_utf8(token).unwrap_or_default())
        .collect();
    match (output.status.code(), tokens.split_first()) {
        (Some(0), Some((tree, []))) if is_oid(tree) => Ok(Merged::Clean {
            tree: (*tree).to_string(),
        }),
        (Some(1), Some((tree, paths))) if is_oid(tree) && !paths.is_empty() => {
            let mut paths: Vec<String> = paths.iter().map(|path| (*path).to_string()).collect();
            paths.dedup();
            Ok(Merged::Conflict { paths })
        }
        (code, _) => Err(infra(format_args!(
            "merge-tree exited {code:?} printing {} token(s)",
            tokens.len()
        ))),
    }
}

async fn carry_commit(repo_root: &Path, upstream: &str, plan: &CarryPlan) -> Result<String> {
    let deadline = tokio::time::Instant::now() + CARRY_TIMEOUT;
    let merge_args = [
        "merge-tree",
        "--write-tree",
        "--name-only",
        "--no-messages",
        "-z",
        upstream,
        plan.candidate_sha.as_str(),
    ];
    let merged = run_git(repo_root, &merge_args, &[], deadline).await?;
    let tree = match classify_merge(&merged)? {
        Merged::Clean { tree } => tree,
        Merged::Conflict { paths } => {
            return Err(CalmError::Conflict(format!(
                "refused: {}",
                crate::task_replace::carry_failure(
                    crate::task_replace::CarryFailure::Conflict,
                    &paths.join(", "),
                )
            )));
        }
    };
    let date = format!("@{} +0000", plan.created_at_ms.div_euclid(1000));
    let identity = [
        ("GIT_AUTHOR_NAME", CARRY_AUTHOR_NAME),
        ("GIT_AUTHOR_EMAIL", CARRY_AUTHOR_EMAIL),
        ("GIT_AUTHOR_DATE", date.as_str()),
        ("GIT_COMMITTER_NAME", CARRY_AUTHOR_NAME),
        ("GIT_COMMITTER_EMAIL", CARRY_AUTHOR_EMAIL),
        ("GIT_COMMITTER_DATE", date.as_str()),
    ];
    let message = format!("neige carry {}", plan.receipt_id);
    let commit_args = [
        "commit-tree",
        "--no-gpg-sign",
        tree.as_str(),
        "-p",
        upstream,
        "-m",
        message.as_str(),
    ];
    let committed = run_git(repo_root, &commit_args, &identity, deadline).await?;
    let printed = String::from_utf8_lossy(&committed.stdout)
        .trim()
        .to_string();
    if committed.status.success() && is_oid(&printed) {
        return Ok(printed);
    }
    Err(infra(format_args!(
        "commit-tree exited {:?}",
        committed.status.code()
    )))
}

/// One git run under the carry deadline, through the gate's bounded runner. The environment is an
/// allowlist, not the server's: `merge-tree` runs any merge driver the attached repository
/// configures, and such repository-selected code must not see the kernel's own variables. Kept:
/// `PATH` (git and the driver resolve binaries), `HOME` and `XDG_CONFIG_HOME` when set (git reads
/// the user's global config there, e.g. `safe.directory`), the C locale, and the caller's commit
/// identity.
async fn run_git(
    repo_root: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    deadline: tokio::time::Instant,
) -> Result<std::process::Output> {
    let mut command = neige_git_command();
    command.env_clear();
    for key in CARRY_INHERITED_ENV {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .envs([("LANG", "C"), ("LC_ALL", "C")])
        .envs(env.iter().copied())
        .arg("-C")
        .arg(repo_root)
        .args(["-c", "core.fsmonitor=false"])
        .args(args);
    let what = args.first().copied().unwrap_or("git");
    run_bounded(
        tokio::process::Command::from(command),
        deadline,
        CARRY_OUTPUT_CAP,
    )
    .await
    .map_err(|error| match error {
        BoundedRunError::Spawn(error) => infra(format_args!("git {what} did not start: {error}")),
        BoundedRunError::TimedOut => infra(format_args!("git {what} timed out")),
        BoundedRunError::PipesMissing => infra(format_args!("git {what}: output pipes missing")),
        BoundedRunError::Drain(error) => {
            infra(format_args!("git {what} output unreadable: {error}"))
        }
        BoundedRunError::Reap(error) => infra(format_args!("git {what} not reaped: {error}")),
        BoundedRunError::Oversized => infra(format_args!("git {what} printed too much")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn output(code: i32, stdout: &[u8]) -> std::process::Output {
        std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        }
    }

    const TREE: &str = "73d81cf03cddc3231d36554c14e2aa739d82a41a";

    #[test]
    fn merge_tree_output_is_read_by_exit_and_shape() {
        let clean = output(0, format!("{TREE}\0").as_bytes());
        assert_eq!(
            classify_merge(&clean).unwrap(),
            Merged::Clean { tree: TREE.into() }
        );
        let conflict = output(1, format!("{TREE}\0f\0g\0").as_bytes());
        assert_eq!(
            classify_merge(&conflict).unwrap(),
            Merged::Conflict {
                paths: vec!["f".into(), "g".into()]
            }
        );
        for infra_case in [
            output(1, b""),
            output(1, format!("{TREE}\0").as_bytes()),
            output(128, format!("{TREE}\0").as_bytes()),
            output(0, b"not-an-oid\0"),
        ] {
            let error = classify_merge(&infra_case).unwrap_err().to_string();
            assert!(error.contains("carry-infra"), "{error}");
        }
    }

    #[test]
    fn carry_notice_names_the_candidate_and_the_base_it_was_merged_onto() {
        for (source, label) in [
            (BaseSource::Upstream, "the upstream"),
            (BaseSource::Head, "the attached checkout's HEAD"),
        ] {
            let line = CarryNotice {
                candidate_sha: "c".repeat(40),
                base_sha: "b".repeat(40),
                base_source: source,
            }
            .render();
            assert!(line.contains(&"c".repeat(40)), "{line}");
            assert!(
                line.contains(&format!("{label} {}", "b".repeat(40))),
                "{line}"
            );
            assert!(!line.contains('\n'), "one line: {line}");
        }
    }
}
