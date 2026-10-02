//! #1830 S2 — where a codex or claude worker runs: its track's `agent_cwd()`, one attempt that
//! changes it at a time, on a clean tree (`docs/architecture/1830-s2-worker-in-track-worktree.md`).
//!
//! - D1: an attached track's worker runs in its track worktree on `neige/track-<id>`, a managed
//!   track's in its managed directory on `main`; an attached track without a worktree is refused.
//! - D2: the lease row records that directory, its HEAD as the base (`BaseSource::Commit`), its
//!   realpath and its common dir.
//! - D5: `calm_truth`'s `checkout_occupancy` and `checkout_admission`, the one rule that says
//!   which attempt may use the checkout.
//! - D6: a dirty tree refuses the worker before any row is written.
//! - #1917: a read-only attempt shares the checkout with other read-only attempts. Its lease row
//!   records no base (so no delivery).
//! - #1933: a read-only attempt that declares `head` starts only while the checkout is at it, at
//!   prepare and again at spawn; its prompt states its repo, checkout, head and base.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use super::base::{
    BaseSource, LeaseBase, lease_git_common_dir, resolve_head_base, utf8_path, verify_worktree_base,
};
use super::{
    WORKSPACE_LEASE_COLUMNS, append_workspace_events_tx, release_workspace_lease_tx,
    row_to_workspace_lease, track_worktree::track_branch_for, validate_path_segment,
};
use crate::error::{CalmError, Result};
use crate::event::BroadcastEnvelope;
use crate::model::{TaskAccess, TrackWorkspaceKind};
use crate::operation::{PhaseTag, Tx, TxOutput};
use crate::plugin_host::child_process::{BoundedRunError, run_bounded};
use crate::workspace_materialize::isolated_git_command;

/// `status_detail` words of the three prepare-time refusals (G13's `spawn-failed: refused: …`).
pub(crate) const TRACK_WITHOUT_WORKTREE: &str = "track-without-worktree";
pub(crate) const TRACK_WORKTREE_DIRTY: &str = "track-worktree-dirty";
pub(crate) const TRACK_WORKTREE_UNAVAILABLE: &str = "track-worktree-unavailable";
/// #1933: the refusals of a read-only task whose declared `head` the track checkout is not at, or
/// that the repository does not have.
pub(crate) const TRACK_HEAD_MISMATCH: &str = "track-head-mismatch";
pub(crate) const TRACK_HEAD_UNKNOWN: &str = "track-head-unknown";

/// The branch a managed track's directory is on: materialization runs `git init` with
/// `init.defaultBranch=main`.
pub(crate) const MANAGED_WORKSPACE_BRANCH: &str = "main";

/// Bound on the clean-tree check's one `git status`.
const CLEAN_CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const GIT_OUTPUT_CAP: usize = 1024 * 1024;

/// Where one worker attempt runs, decided in its prepare transaction.
#[derive(Clone, Debug)]
pub(crate) struct WorkerLeasePlan {
    /// The track's `agent_cwd()`: the worker's cwd and the lease row's `path`.
    pub path: PathBuf,
    /// The branch the checkout is on (D4); spawn verifies HEAD is on it.
    pub branch: String,
    /// HEAD of `path`, its realpath and its common dir (D2).
    pub base: LeaseBase,
    /// `workspace.released` of the stuck owners' leases this prepare superseded (D7).
    pub superseded: Vec<BroadcastEnvelope>,
    /// A read-only attempt's facts; `None` for one that changes the checkout. Read from its
    /// `tasks` row in this transaction (never from the op payload, which feeds
    /// `stable_payload_hash`).
    pub reader: Option<ReaderFacts>,
}

impl WorkerLeasePlan {
    /// The attempt's `tasks.access`.
    pub(crate) fn access(&self) -> TaskAccess {
        match self.reader {
            Some(_) => TaskAccess::ReadOnly,
            None => TaskAccess::ReadWrite,
        }
    }

    /// The `head` a read-only attempt declared.
    pub(crate) fn declared_head(&self) -> Option<&str> {
        self.reader
            .as_ref()
            .and_then(|reader| reader.head.as_deref())
    }
}

/// #1933: what a read-only attempt's prompt states as typed facts.
#[derive(Clone, Debug)]
pub(crate) struct ReaderFacts {
    /// The track's remote URL ([`super::upstream::track_remote`]), or why there is none.
    pub repo: std::result::Result<String, String>,
    /// The checkout the reader shares: the plan's `path`.
    pub checkout: PathBuf,
    /// `tasks.head`: the commit the checkout must be at, checked at prepare and at spawn.
    pub head: Option<String>,
    /// `tasks.base`: the commit a review compares against.
    pub base: Option<String>,
}

/// The branch rule (D4): `neige/track-<id>` when the track has a worktree, else `main`.
pub(crate) fn worker_branch(track_id: &str, has_worktree: bool) -> Result<String> {
    if has_worktree {
        track_branch_for(track_id)
    } else {
        Ok(MANAGED_WORKSPACE_BRANCH.to_string())
    }
}

/// [`worker_branch`] of the track row.
pub(crate) async fn worker_branch_tx(tx: &mut Tx<'_>, track_id: &str) -> Result<String> {
    let (_, _, worktree) = track_workspace_tx(tx, track_id).await?;
    worker_branch(track_id, worktree.is_some())
}

async fn track_workspace_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
) -> Result<(TrackWorkspaceKind, String, Option<String>)> {
    let (kind, path, worktree): (String, String, Option<String>) = sqlx::query_as(
        "SELECT workspace_kind, workspace_path, workspace_worktree_path FROM tracks WHERE id = ?1",
    )
    .bind(track_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| CalmError::NotFound(format!("track {track_id}")))?;
    Ok((
        TrackWorkspaceKind::try_from(kind).map_err(CalmError::Internal)?,
        path,
        worktree,
    ))
}

/// The worker's directory, branch and base, inside the worker op's prepare transaction (D1, D2,
/// D6, D7 supersede), for the attempt `attempt_id` (its `tasks` row). A refusal is a `Conflict`
/// (`refused: <word>: …`), so the op fails once and the task ends `spawn-failed: refused: …`;
/// nothing is written.
pub(crate) async fn prepare_worker_lease_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
    attempt_id: &str,
    workspace_root: &Path,
) -> Result<WorkerLeasePlan> {
    let (access, head, base): (String, Option<String>, Option<String>) =
        sqlx::query_as("SELECT access, head, base FROM tasks WHERE id = ?1")
            .bind(attempt_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("task {attempt_id}")))?;
    let access = TaskAccess::try_from(access).map_err(CalmError::Internal)?;
    prepare_worker_lease_with_tx(tx, track_id, access, (head, base), workspace_root).await
}

/// [`prepare_worker_lease_tx`] for an attempt whose access is already known (a fixture lease
/// with no `tasks` row) and that declares no commits.
#[cfg(any(test, feature = "fixtures"))]
pub(crate) async fn prepare_worker_lease_as_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
    access: TaskAccess,
    workspace_root: &Path,
) -> Result<WorkerLeasePlan> {
    prepare_worker_lease_with_tx(tx, track_id, access, (None, None), workspace_root).await
}

/// `(head, base)` are the attempt's declared commits; only a read-only attempt has any (#1933,
/// enforced where the task block is validated). A declared head the checkout is not at refuses
/// the attempt here, before any row is written.
async fn prepare_worker_lease_with_tx(
    tx: &mut Tx<'_>,
    track_id: &str,
    access: TaskAccess,
    (head, base): (Option<String>, Option<String>),
    workspace_root: &Path,
) -> Result<WorkerLeasePlan> {
    validate_path_segment("track_id", track_id)?;
    let (kind, workspace_path, worktree) = track_workspace_tx(tx, track_id).await?;
    let path = match kind {
        // Last-chance materialize: track create materializes after its transaction commits, so a
        // failure there leaves a committed row pointing at a missing directory.
        TrackWorkspaceKind::Managed => {
            crate::workspace_materialize::materialize_managed_workspace(
                workspace_root,
                Path::new(&workspace_path),
                track_id,
            )?;
            PathBuf::from(&workspace_path)
        }
        TrackWorkspaceKind::Attached => match worktree.as_deref() {
            Some(worktree) => PathBuf::from(worktree),
            None => return Err(track_without_worktree()),
        },
    };
    let branch = worker_branch(track_id, worktree.is_some())?;
    ensure_clean_tree(&path).await?;
    // One HEAD sample: the declared head is compared with the base the lease records, so the
    // spawn's base check also holds the declared head.
    let base_commit = directory_base(&path)?;
    let reader = match access {
        TaskAccess::ReadWrite => None,
        TaskAccess::ReadOnly => {
            if let Some(head) = head.as_deref() {
                check_head_sample(&path, head, &base_commit.base_sha)?;
            }
            Some(ReaderFacts {
                repo: reader_repo(track_id, worktree.as_deref()),
                checkout: path.clone(),
                head,
                base,
            })
        }
    };
    let superseded = supersede_stuck_leases_tx(tx, &path).await?;
    Ok(WorkerLeasePlan {
        path,
        branch,
        base: base_commit,
        superseded,
        reader,
    })
}

/// #1933: the track's remote URL for a reader's prompt, or why there is none. Never fails the
/// launch.
fn reader_repo(track_id: &str, worktree: Option<&str>) -> std::result::Result<String, String> {
    let Some(worktree) = worktree else {
        return Err("a managed track has no remote".into());
    };
    match super::upstream::track_remote(track_id, worktree) {
        Ok((_, Some(upstream))) => Ok(upstream.url),
        Ok((target, None)) => Err(format!(
            "no upstream remote for {}",
            target.repo_root.display()
        )),
        Err(error) => Err(error.to_string()),
    }
}

/// #1933: a read-only task that declares `head` starts only while the track checkout is at it.
/// A refusal is a `Conflict` naming both commits; a head the repository does not have gets its
/// own word.
pub(crate) fn verify_declared_head(checkout: &Path, head: &str) -> Result<()> {
    check_head_sample(checkout, head, &super::base::resolve_head_base(checkout)?)
}

/// [`verify_declared_head`] against `actual`, one HEAD sample of `checkout` the caller took.
fn check_head_sample(checkout: &Path, head: &str, actual: &str) -> Result<()> {
    if actual == head {
        return Ok(());
    }
    let refusal = if super::upstream::resolve_commit(checkout, head)?.is_some() {
        format!(
            "{TRACK_HEAD_MISMATCH}: the track checkout is at {actual}, not the declared head \
             {head}"
        )
    } else {
        format!(
            "{TRACK_HEAD_UNKNOWN}: the declared head {head} is not a commit in this repository; \
             the track checkout is at {actual}"
        )
    };
    Err(CalmError::Conflict(format!(
        "refused: {refusal}. Move the checkout to the head under review, or declare the task \
         again under a new key"
    )))
}

/// The output key of a declared head, written only when there is one.
const DECLARED_HEAD: &str = "declared_head";

/// #1933: record a declared head in a prepare's output for the spawns that check it again: the
/// codex worker's (a `SpawnStarted` re-drive skips `app_server_interact`) and the Claude
/// restart's. A claude worker's spawn needs none: its base check holds the declared head.
pub(crate) fn record_declared_head(data: &mut serde_json::Value, head: Option<&str>) {
    if let (Some(head), Some(data)) = (head, data.as_object_mut()) {
        data.insert(DECLARED_HEAD.into(), head.into());
    }
}

/// #1933, the spawn side: the checkout the prepare froze (`cwd`) is still at the declared head.
pub(crate) fn verify_recorded_head(output: &TxOutput, ctx: &str) -> Result<()> {
    let Some(head) = output
        .data
        .get(DECLARED_HEAD)
        .and_then(|head| head.as_str())
    else {
        return Ok(());
    };
    verify_declared_head(Path::new(&output.output_string("cwd", ctx)?), head)
}

/// D3, the spawn side of a worker op: the checkout its prepare froze (`cwd`) is still on
/// `branch` at `base_sha`, at `canonical_path`. Nothing is created.
pub(crate) fn verify_worker_checkout(output: &TxOutput, ctx: &str) -> Result<()> {
    let path = output.output_string("cwd", ctx)?;
    let branch = output.output_string("branch", ctx)?;
    let base_sha = output.output_string("base_sha", ctx)?;
    let canonical_path = output.output_string("canonical_path", ctx)?;
    verify_worktree_base(
        Path::new(&path),
        &branch,
        &base_sha,
        Path::new(&canonical_path),
    )
}

fn track_without_worktree() -> CalmError {
    CalmError::Conflict(format!(
        "refused: {TRACK_WITHOUT_WORKTREE}: this track predates per-track worktrees; create a new \
         track to run codex or claude tasks"
    ))
}

/// HEAD of the directory, its realpath and its common dir: the base the lease row records.
pub(crate) fn directory_base(path: &Path) -> Result<LeaseBase> {
    let canonical_path = std::fs::canonicalize(path).map_err(|e| {
        CalmError::Internal(format!(
            "canonicalize worker checkout {}: {e}",
            path.display()
        ))
    })?;
    utf8_path(&canonical_path, "worker checkout realpath")?;
    Ok(LeaseBase {
        base_sha: resolve_head_base(path)?,
        base_source: BaseSource::Commit,
        base_attempt_id: None,
        canonical_path,
        git_common_dir: lease_git_common_dir(path)?,
    })
}

/// D6: a worker starts only on a clean tree. Untracked files count (they are what `git add -A`
/// would commit, whatever `status.showUntrackedFiles` says); ignored files do not.
pub(crate) async fn ensure_clean_tree(path: &Path) -> Result<()> {
    let dirty = dirty_paths(path).await?;
    if dirty.is_empty() {
        return Ok(());
    }
    Err(CalmError::Conflict(format!(
        "refused: {TRACK_WORKTREE_DIRTY}: {} uncommitted path(s). Commit them with git.commit or \
         undo them, then declare it again under a new key: {}",
        dirty.len(),
        dirty.join(", ")
    )))
}

/// The paths `git status --porcelain -z` lists, in its order; a rename or copy is named by its
/// destination.
async fn dirty_paths(path: &Path) -> Result<Vec<String>> {
    let deadline = tokio::time::Instant::now() + CLEAN_CHECK_TIMEOUT;
    let output = run_git(
        path,
        &["status", "--porcelain", "-z", "--untracked-files=normal"],
        deadline,
    )
    .await
    .map_err(unavailable)?;
    if !output.status.success() {
        return Err(unavailable(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    let mut paths = Vec::new();
    let mut entries = output.stdout.split(|byte| *byte == 0);
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        paths.push(String::from_utf8_lossy(&entry[3..]).into_owned());
        // `R` / `C` entries carry their source path as the next NUL-terminated field.
        if matches!(entry[0], b'R' | b'C') || matches!(entry[1], b'R' | b'C') {
            entries.next();
        }
    }
    Ok(paths)
}

fn unavailable(why: String) -> CalmError {
    CalmError::Conflict(format!("refused: {TRACK_WORKTREE_UNAVAILABLE}: {why}"))
}

/// One git run in `dir` under `deadline`, through the gate's bounded runner. The environment is
/// [`isolated_git_command`]'s allowlist: the repository's own configuration can select code (a
/// filter, a hook), which must not see the kernel's variables. `Err` names why git gave no answer.
pub(crate) async fn run_git(
    dir: &Path,
    args: &[&str],
    deadline: tokio::time::Instant,
) -> std::result::Result<Output, String> {
    let mut command = isolated_git_command();
    command
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.fsmonitor=false"])
        .args(args);
    let what = args.first().copied().unwrap_or("git");
    run_bounded(
        tokio::process::Command::from(command),
        deadline,
        GIT_OUTPUT_CAP,
    )
    .await
    .map_err(|error| match error {
        BoundedRunError::Spawn(error) => format!("git {what} did not start: {error}"),
        BoundedRunError::TimedOut => format!("git {what} timed out"),
        BoundedRunError::PipesMissing => format!("git {what}: output pipes missing"),
        BoundedRunError::Drain(error) => format!("git {what} output unreadable: {error}"),
        BoundedRunError::Reap(error) => format!("git {what} not reaped: {error}"),
        BoundedRunError::Oversized => format!("git {what} printed too much"),
    })
}

/// D7 supersede: a `held` lease at `path` whose owner op is `stuck` is released with no delivery
/// row — the tree was just proven clean, and a row submitted now would commit the new worker's
/// files. Keeps `workspace_leases_active_path_idx` satisfied for the INSERT that follows.
async fn supersede_stuck_leases_tx(tx: &mut Tx<'_>, path: &Path) -> Result<Vec<BroadcastEnvelope>> {
    let sql = format!(
        "SELECT {WORKSPACE_LEASE_COLUMNS} FROM workspace_leases \
         WHERE path = ?1 AND state = 'held' \
         AND lease_owner IN (SELECT id FROM operations WHERE phase = ?2)"
    );
    let rows = sqlx::query(&sql)
        .bind(path.to_string_lossy().as_ref())
        .bind(PhaseTag::Stuck.as_str())
        .fetch_all(&mut **tx)
        .await?;
    let mut events = Vec::new();
    for row in rows {
        let lease = row_to_workspace_lease(row)?;
        events.extend(release_workspace_lease_tx(tx, &lease).await?);
    }
    append_workspace_events_tx(tx, events).await
}
