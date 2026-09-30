//! Execution allocations outlive the deletable pending task projection.
//! Caller authority and declaration admission belong to the server service.

use calm_types::task_recovery::{TaskAttemptAllocation, TaskAttemptOrigin};
use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

use super::task::TASK_COLUMNS;
use crate::error::{CalmError, Result};
use crate::model::Task;

#[derive(sqlx::FromRow)]
struct AllocationRow {
    attempt_id: String,
    track_id: String,
    key: String,
    generation: i64,
    origin_json: String,
    created_at_ms: i64,
}

impl AllocationRow {
    fn decode(self) -> Result<TaskAttemptAllocation> {
        let origin: TaskAttemptOrigin = serde_json::from_str(&self.origin_json)?;
        if let TaskAttemptOrigin::Recovery { constraint, .. } = &origin {
            constraint
                .validate(&self.track_id)
                .map_err(CalmError::Internal)?;
        }
        Ok(TaskAttemptAllocation {
            attempt_id: self.attempt_id,
            track_id: self.track_id,
            key: self.key,
            generation: self.generation,
            origin,
            created_at_ms: self.created_at_ms,
        })
    }
}

async fn current_on(
    conn: &mut SqliteConnection,
    track_id: &str,
    key: &str,
) -> Result<Option<TaskAttemptAllocation>> {
    sqlx::query_as::<_, AllocationRow>(
        "SELECT * FROM task_attempt_allocations WHERE track_id=?1 AND key=?2 \
         ORDER BY generation DESC LIMIT 1",
    )
    .bind(track_id)
    .bind(key)
    .fetch_optional(conn)
    .await?
    .map(AllocationRow::decode)
    .transpose()
}

pub async fn task_attempt_current_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    key: &str,
) -> Result<Option<TaskAttemptAllocation>> {
    current_on(tx, track_id, key).await
}

pub async fn task_attempt_current_pool(
    pool: &SqlitePool,
    track_id: &str,
    key: &str,
) -> Result<Option<TaskAttemptAllocation>> {
    current_on(&mut *pool.acquire().await?, track_id, key).await
}

/// Page in logical key order, passing the last key as the next exclusive cursor.
async fn current_by_track_on(
    conn: &mut SqliteConnection,
    track_id: &str,
    after_key: Option<&str>,
    limit: i64,
) -> Result<Vec<TaskAttemptAllocation>> {
    let rows = sqlx::query_as::<_, AllocationRow>(
        "SELECT * FROM current_task_attempt_allocations WHERE track_id=?1 \
         AND (?2 IS NULL OR key>?2) ORDER BY key ASC LIMIT ?3",
    )
    .bind(track_id)
    .bind(after_key)
    .bind(limit.clamp(1, 500))
    .fetch_all(conn)
    .await?;
    rows.into_iter().map(AllocationRow::decode).collect()
}

pub async fn task_attempt_current_by_track_pool(
    pool: &SqlitePool,
    track_id: &str,
    after_key: Option<&str>,
    limit: i64,
) -> Result<Vec<TaskAttemptAllocation>> {
    current_by_track_on(&mut *pool.acquire().await?, track_id, after_key, limit).await
}

/// Use this variant while enumerating multiple pages in one read snapshot.
pub async fn task_attempt_current_by_track_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    after_key: Option<&str>,
    limit: i64,
) -> Result<Vec<TaskAttemptAllocation>> {
    current_by_track_on(tx, track_id, after_key, limit).await
}

pub async fn task_attempt_get_tx(
    tx: &mut Transaction<'_, Sqlite>,
    attempt_id: &str,
) -> Result<Option<TaskAttemptAllocation>> {
    sqlx::query_as::<_, AllocationRow>("SELECT * FROM task_attempt_allocations WHERE attempt_id=?1")
        .bind(attempt_id)
        .fetch_optional(&mut **tx)
        .await?
        .map(AllocationRow::decode)
        .transpose()
}

pub async fn task_current_get_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    key: &str,
) -> Result<Option<Task>> {
    let sql = format!("SELECT {TASK_COLUMNS} FROM current_tasks WHERE track_id=?1 AND key=?2");
    Ok(sqlx::query_as(&sql)
        .bind(track_id)
        .bind(key)
        .fetch_optional(&mut **tx)
        .await?)
}

pub async fn task_current_get_pool(
    pool: &SqlitePool,
    track_id: &str,
    key: &str,
) -> Result<Option<Task>> {
    let sql = format!("SELECT {TASK_COLUMNS} FROM current_tasks WHERE track_id=?1 AND key=?2");
    Ok(sqlx::query_as(&sql)
        .bind(track_id)
        .bind(key)
        .fetch_optional(pool)
        .await?)
}

pub async fn task_history_by_key_pool(
    pool: &SqlitePool,
    track_id: &str,
    key: &str,
) -> Result<Vec<Task>> {
    let sql = format!(
        "SELECT {TASK_COLUMNS} FROM tasks WHERE track_id=?1 AND key=?2 \
        ORDER BY (SELECT generation FROM task_attempt_allocations a WHERE a.attempt_id=tasks.id)"
    );
    Ok(sqlx::query_as(&sql)
        .bind(track_id)
        .bind(key)
        .fetch_all(pool)
        .await?)
}
