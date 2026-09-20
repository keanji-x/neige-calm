//! `task_candidates`: the immutable candidate one successful delivery pins
//! (`candidate_id = delivery_id`, at most one per attempt).
//!
//! Every column is a byte copy of the operation result (`commit_sha`, `branch`, `delivery_id`,
//! `base_is_ancestor`) or of the lease row (`base_sha`, `git_common_dir`, `repo_root`); nothing
//! is derived from events. `branch` is the script's one observation at its start and is for
//! humans only; downstream readers use `commit_sha` / `ref_name` (G20).

use std::path::Path;

use serde_json::Value;
use sqlx::Row;

use super::delivery::{DeliveryRow, candidate_ref_name};
use crate::error::{CalmError, Result};
use crate::operation::Tx;
use crate::operation::workspace_lease::WorkspaceLease;
use crate::workspace_materialize::neige_git_command;

/// One `task_candidates` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CandidateRow {
    pub candidate_id: String,
    pub track_id: String,
    pub producer_attempt_id: String,
    pub card_id: String,
    pub lease_id: String,
    /// The repository root the lease worktree hangs under, for humans reading the row; the
    /// inverse of `workspace_lease_path_for` on the lease's `path` (never re-derived from the
    /// Track cwd, which may have moved). Not an input to any downstream reader.
    pub repo_root: String,
    pub git_common_dir: String,
    pub branch: String,
    pub base_sha: String,
    pub commit_sha: String,
    pub base_is_ancestor: bool,
    pub ref_name: String,
    pub created_at_ms: i64,
}

const CANDIDATE_COLUMNS: &str = "candidate_id, track_id, producer_attempt_id, card_id, lease_id, \
     repo_root, git_common_dir, branch, base_sha, commit_sha, base_is_ancestor, ref_name, \
     created_at_ms";

/// The repository root a lease `path` hangs under: `path` minus its trailing
/// `.claude/worktrees/<track>/<card>` — the inverse of `workspace_lease_path_for`. `Err` when the
/// path does not end in exactly that (it is not a lease path this kernel produced).
pub(crate) fn repo_root_from_lease_path(
    path: &str,
    track_id: &str,
    card_id: &str,
) -> Result<String> {
    let suffix = format!("/.claude/worktrees/{track_id}/{card_id}");
    match path.strip_suffix(&suffix) {
        // `workspace_lease_path_for("/", ..)` is `/.claude/...`: the root is `/` itself.
        Some("") => Ok("/".to_string()),
        Some(root) if root.starts_with('/') => Ok(root.to_string()),
        _ => Err(CalmError::Internal(format!(
            "workspace lease path {path:?} does not end in {suffix:?}"
        ))),
    }
}

/// The candidate one settled delivery pins, from the forge action's result event (the
/// `worktree.committed` payload built from the script's JSON line) and the lease row. `Err` when
/// the event names another delivery or the lease carries no base.
pub(crate) fn from_operation_result(
    delivery: &DeliveryRow,
    lease: &WorkspaceLease,
    result_event: &Value,
    now_ms: i64,
) -> Result<CandidateRow> {
    let field = |name: &str| {
        result_event.get(name).ok_or_else(|| {
            CalmError::Internal(format!(
                "delivery {} result event has no {name}",
                delivery.delivery_id
            ))
        })
    };
    let string = |name: &str| -> Result<String> {
        field(name)?.as_str().map(str::to_string).ok_or_else(|| {
            CalmError::Internal(format!(
                "delivery {} result event {name} is not a string",
                delivery.delivery_id
            ))
        })
    };
    let event_delivery_id = string("delivery_id")?;
    if event_delivery_id != delivery.delivery_id {
        return Err(CalmError::Internal(format!(
            "delivery {} result event names delivery {event_delivery_id}",
            delivery.delivery_id
        )));
    }
    let commit_sha = string("commit_sha")?;
    let branch = string("branch")?;
    let base_is_ancestor = field("base_is_ancestor")?.as_bool().ok_or_else(|| {
        CalmError::Internal(format!(
            "delivery {} result event base_is_ancestor is not a bool",
            delivery.delivery_id
        ))
    })?;
    let Some(base) = lease.base.as_ref() else {
        return Err(CalmError::Internal(format!(
            "delivery {} on lease {} without a recorded base",
            delivery.delivery_id, lease.lease_id
        )));
    };
    let git_common_dir = base.git_common_dir.to_str().ok_or_else(|| {
        CalmError::Internal(format!(
            "workspace lease git_common_dir {} is not UTF-8",
            base.git_common_dir.display()
        ))
    })?;
    Ok(CandidateRow {
        candidate_id: delivery.delivery_id.clone(),
        track_id: delivery.track_id.clone(),
        producer_attempt_id: delivery.producer_attempt_id.clone(),
        card_id: delivery.card_id.clone(),
        lease_id: delivery.lease_id.clone(),
        repo_root: repo_root_from_lease_path(&lease.path, &delivery.track_id, &delivery.card_id)?,
        git_common_dir: git_common_dir.to_string(),
        branch,
        base_sha: base.base_sha.clone(),
        commit_sha,
        base_is_ancestor,
        ref_name: candidate_ref_name(&delivery.track_id, &delivery.card_id, &delivery.delivery_id),
        created_at_ms: now_ms,
    })
}

/// The candidate of one attempt, if its delivery settled as one.
pub(crate) async fn candidate_for_attempt_tx(
    tx: &mut Tx<'_>,
    producer_attempt_id: &str,
) -> Result<Option<CandidateRow>> {
    let sql =
        format!("SELECT {CANDIDATE_COLUMNS} FROM task_candidates WHERE producer_attempt_id = ?1");
    let row = sqlx::query(&sql)
        .bind(producer_attempt_id)
        .fetch_optional(&mut **tx)
        .await?;
    row.map(row_to_candidate).transpose()
}

/// Insert the candidate row; only `delivery::settle_candidate_tx` calls this, after its UPDATE
/// took, so the row and the settlement land in the same transaction or not at all.
pub(super) async fn insert_candidate_tx(tx: &mut Tx<'_>, candidate: &CandidateRow) -> Result<()> {
    sqlx::query(
        "INSERT INTO task_candidates (candidate_id, track_id, producer_attempt_id, card_id, \
         lease_id, repo_root, git_common_dir, branch, base_sha, commit_sha, base_is_ancestor, \
         ref_name, created_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )
    .bind(&candidate.candidate_id)
    .bind(&candidate.track_id)
    .bind(&candidate.producer_attempt_id)
    .bind(&candidate.card_id)
    .bind(&candidate.lease_id)
    .bind(&candidate.repo_root)
    .bind(&candidate.git_common_dir)
    .bind(&candidate.branch)
    .bind(&candidate.base_sha)
    .bind(&candidate.commit_sha)
    .bind(candidate.base_is_ancestor as i64)
    .bind(&candidate.ref_name)
    .bind(candidate.created_at_ms)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn row_to_candidate(row: sqlx::sqlite::SqliteRow) -> Result<CandidateRow> {
    let base_is_ancestor: i64 = row.try_get("base_is_ancestor")?;
    Ok(CandidateRow {
        candidate_id: row.try_get("candidate_id")?,
        track_id: row.try_get("track_id")?,
        producer_attempt_id: row.try_get("producer_attempt_id")?,
        card_id: row.try_get("card_id")?,
        lease_id: row.try_get("lease_id")?,
        repo_root: row.try_get("repo_root")?,
        git_common_dir: row.try_get("git_common_dir")?,
        branch: row.try_get("branch")?,
        base_sha: row.try_get("base_sha")?,
        commit_sha: row.try_get("commit_sha")?,
        base_is_ancestor: base_is_ancestor != 0,
        ref_name: row.try_get("ref_name")?,
        created_at_ms: row.try_get("created_at_ms")?,
    })
}

/// The commit `ref_name` resolves to in `git_common_dir` (the lease row's persisted value: the
/// candidate ref lives in the common dir, and the Track cwd may have moved since — A6c).
/// `Ok(None)` when git resolved nothing (the ref is absent); `Err` when git could not be run.
pub(crate) async fn resolve_ref_commit(
    git_common_dir: &Path,
    ref_name: &str,
) -> Result<Option<String>> {
    let mut command = neige_git_command();
    command
        .arg(format!("--git-dir={}", git_common_dir.display()))
        .args([
            "rev-parse",
            "--verify",
            "-q",
            &format!("{ref_name}^{{commit}}"),
        ]);
    let output = tokio::task::spawn_blocking(move || command.output())
        .await
        .map_err(|e| CalmError::Internal(format!("git rev-parse task: {e}")))?
        .map_err(|e| {
            CalmError::Internal(format!(
                "spawn git rev-parse in {}: {e}",
                git_common_dir.display()
            ))
        })?;
    if !output.status.success() {
        return Ok(None);
    }
    let printed = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!printed.is_empty()).then_some(printed))
}
