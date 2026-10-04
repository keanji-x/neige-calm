//! `task_candidates`: the immutable candidate one successful delivery pins
//! (`candidate_id = delivery_id`, at most one per attempt).
//!
//! Every column is a byte copy of the operation result (`commit_sha`, `branch`, `delivery_id`,
//! `base_is_ancestor`) or of the lease row (`base_sha`, `git_common_dir`, `repo_root` = `path`); nothing
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
    /// The checkout the candidate was made in (the lease's `path`, #1830 S2 D4), for humans
    /// reading the row. Not an input to any downstream reader.
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
        repo_root: lease.path.clone(),
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

/// One candidate of a track with its attempt's status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrackCandidate {
    pub attempt_id: String,
    pub commit_sha: String,
    /// The attempt's `tasks.status` wire label.
    pub status: String,
}

/// Every candidate of `track_id` with its attempt's status, newest first. The one read of "the
/// commits the kernel made for this track": publish checks the tip against it and leases against
/// it (#1830 S3 D3, #2058 D1), and a catch-up replays its newest done commit (#2058 D6).
pub(crate) async fn track_candidates<'c, E>(
    executor: E,
    track_id: &str,
) -> Result<Vec<TrackCandidate>>
where
    E: sqlx::Executor<'c, Database = sqlx::Sqlite>,
{
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT c.producer_attempt_id, c.commit_sha, t.status FROM task_candidates c \
         JOIN tasks t ON t.id = c.producer_attempt_id WHERE c.track_id = ?1 \
         ORDER BY c.created_at_ms DESC, c.rowid DESC",
    )
    .bind(track_id)
    .fetch_all(executor)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(attempt_id, commit_sha, status)| TrackCandidate {
            attempt_id,
            commit_sha,
            status,
        })
        .collect())
}

/// The newest of `candidates` (newest first, as [`track_candidates`] reads them) whose attempt is
/// done.
pub(crate) fn newest_done(candidates: &[TrackCandidate]) -> Option<&TrackCandidate> {
    candidates
        .iter()
        .find(|candidate| candidate.status == crate::model::TaskStatus::Done.wire_label())
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
