//! Execution allocations outlive the deletable pending task projection (#1501).
//! Caller authority and declaration admission belong to the server service.

use calm_types::ids::ActorId;
use calm_types::task_recovery::{
    TaskAttemptAllocation, TaskAttemptOrigin, TaskRecoveryConstraint, TaskRecoveryReceipt,
    TaskRecoveryRequest,
};
use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

use super::task::{TASK_COLUMNS, task_get_tx};
use crate::error::{CalmError, Result};
use crate::model::{Task, TaskStatus, new_id, now_ms};

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
        Ok(TaskAttemptAllocation {
            attempt_id: self.attempt_id,
            track_id: self.track_id,
            key: self.key,
            generation: self.generation,
            origin: serde_json::from_str(&self.origin_json)?,
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

/// Lookup precedes current-attempt admission so a lost-response retry returns
/// the original receipt, even after the allocated execution has finished.
pub async fn task_recovery_lookup_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    key: &str,
    idempotency_key: &str,
    request_fingerprint: &str,
) -> Result<Option<TaskRecoveryReceipt>> {
    let row = sqlx::query_as::<_, AllocationRow>(
        "SELECT * FROM task_attempt_allocations WHERE track_id=?1 \
         AND json_extract(origin_json,'$.idempotency_key')=?2",
    )
    .bind(track_id)
    .bind(idempotency_key)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(allocation) = row.map(AllocationRow::decode).transpose()? else {
        return Ok(None);
    };
    if let TaskAttemptOrigin::Recovery {
        request_fingerprint: stored,
        ..
    } = &allocation.origin
        && allocation.key == key
        && stored == request_fingerprint
    {
        return Ok(allocation.recovery_receipt());
    }
    Err(CalmError::Conflict(
        "recovery idempotency key was used for a different request".into(),
    ))
}

/// Allocates exactly one successor. Does NOT create a task projection or emit
/// events: the service must project the admitted declaration and append its
/// recovery/plan events in this same IMMEDIATE transaction before committing.
pub async fn task_recovery_allocate_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    key: &str,
    request: &TaskRecoveryRequest,
    request_fingerprint: &str,
    constraint: &TaskRecoveryConstraint,
    actor: &ActorId,
) -> Result<TaskRecoveryReceipt> {
    if request.expected_attempt_id.is_empty()
        || request.idempotency_key.trim().is_empty()
        || request.reason.trim().is_empty()
        || request_fingerprint.is_empty()
    {
        return Err(CalmError::BadRequest(
            "recovery fields must be nonempty".into(),
        ));
    }
    if let Some(receipt) = task_recovery_lookup_tx(
        tx,
        track_id,
        key,
        &request.idempotency_key,
        request_fingerprint,
    )
    .await?
    {
        return Ok(receipt);
    }
    constraint
        .validate(track_id)
        .map_err(CalmError::BadRequest)?;
    let current = task_attempt_current_tx(tx, track_id, key)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("task {track_id}:{key}")))?;
    if current.attempt_id != request.expected_attempt_id {
        return Err(CalmError::Conflict(
            "recovery expected attempt is no longer current".into(),
        ));
    }
    let previous = task_get_tx(tx, &current.attempt_id).await?.ok_or_else(|| {
        CalmError::Conflict("recovery current attempt has no execution row".into())
    })?;
    if previous.status != TaskStatus::Failed {
        return Err(CalmError::Conflict(
            "only a failed execution can be recovered".into(),
        ));
    }
    let generation = current
        .generation
        .checked_add(1)
        .ok_or_else(|| CalmError::Conflict("task generation exhausted".into()))?;
    let allocation = TaskAttemptAllocation {
        attempt_id: new_id(),
        track_id: track_id.into(),
        key: key.into(),
        generation,
        origin: TaskAttemptOrigin::Recovery {
            previous_attempt_id: current.attempt_id,
            idempotency_key: request.idempotency_key.clone(),
            request_fingerprint: request_fingerprint.into(),
            reason: request.reason.clone(),
            actor: actor.clone(),
            constraint: constraint.clone(),
        },
        created_at_ms: now_ms(),
    };
    sqlx::query(
        "INSERT INTO task_attempt_allocations \
         (attempt_id,track_id,key,generation,origin_json,created_at_ms) VALUES (?1,?2,?3,?4,?5,?6)",
    )
    .bind(&allocation.attempt_id)
    .bind(track_id)
    .bind(key)
    .bind(generation)
    .bind(serde_json::to_string(&allocation.origin)?)
    .bind(allocation.created_at_ms)
    .execute(&mut **tx)
    .await?;
    Ok(allocation.recovery_receipt().expect("recovery allocation"))
}

/// None is structurally the initial-allocation case; an unknown ID is an error.
pub async fn task_recovery_constraint_tx(
    tx: &mut Transaction<'_, Sqlite>,
    attempt_id: &str,
) -> Result<Option<TaskRecoveryConstraint>> {
    let allocation = task_attempt_get_tx(tx, attempt_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("task attempt {attempt_id}")))?;
    Ok(match allocation.origin {
        TaskAttemptOrigin::Initial => None,
        TaskAttemptOrigin::Recovery { constraint, .. } => Some(constraint),
    })
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
