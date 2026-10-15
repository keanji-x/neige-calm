//! Attempt-owned Planner messages. Callers admit authority in the same write transaction.
use crate::error::{CalmError, Result};
use crate::model::{new_id, now_ms};
use crate::track_fs_dto::TaskPlannerMessage;
use sqlx::{Sqlite, SqlitePool, Transaction};

pub async fn planner_message_insert_tx(
    tx: &mut Transaction<'_, Sqlite>,
    attempt_id: &str,
    key: &str,
    text: &str,
) -> Result<String> {
    let prior: Option<(String, String)> = sqlx::query_as(
        "SELECT message_id,text FROM task_planner_messages WHERE attempt_id=? AND idempotency_key=?"
    ).bind(attempt_id).bind(key).fetch_optional(&mut **tx).await?;
    if let Some((id, original)) = prior {
        if original != text {
            return Err(CalmError::BadRequest(
                "input idempotency_key reused with different arguments",
            ));
        }
        return Ok(id);
    }
    let id = new_id();
    sqlx::query(
        "INSERT INTO task_planner_messages
         (message_id,attempt_id,seq,text,idempotency_key,created_at_ms)
         SELECT ?,?,COALESCE(MAX(seq),0)+1,?,?,?
         FROM task_planner_messages WHERE attempt_id=?",
    )
    .bind(&id)
    .bind(attempt_id)
    .bind(text)
    .bind(key)
    .bind(now_ms())
    .bind(attempt_id)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

pub async fn planner_messages_unread_tx(
    tx: &mut Transaction<'_, Sqlite>,
    attempt_id: &str,
) -> Result<Vec<TaskPlannerMessage>> {
    fetch_messages(&mut **tx, attempt_id, None, true).await
}

pub async fn planner_messages_for_attempt_tx(
    tx: &mut Transaction<'_, Sqlite>,
    attempt_id: &str,
) -> Result<Vec<TaskPlannerMessage>> {
    fetch_messages(&mut **tx, attempt_id, None, false).await
}

pub async fn planner_messages_read_tx(
    tx: &mut Transaction<'_, Sqlite>,
    attempt_id: &str,
) -> Result<Vec<TaskPlannerMessage>> {
    sqlx::query(
        "UPDATE task_planner_messages SET read_at_ms=? WHERE attempt_id=? AND read_at_ms IS NULL",
    )
    .bind(now_ms())
    .bind(attempt_id)
    .execute(&mut **tx)
    .await?;
    planner_messages_for_attempt_tx(tx, attempt_id).await
}

pub async fn planner_messages_for_attempt(
    pool: &SqlitePool,
    track_id: &str,
    attempt_id: &str,
) -> Result<Vec<TaskPlannerMessage>> {
    fetch_messages(pool, attempt_id, Some(track_id), false).await
}

async fn fetch_messages<'e>(
    executor: impl sqlx::Executor<'e, Database = Sqlite>,
    attempt_id: &str,
    track_id: Option<&str>,
    unread_only: bool,
) -> Result<Vec<TaskPlannerMessage>> {
    let mut query = sqlx::QueryBuilder::<Sqlite>::new(
        "SELECT m.message_id,m.seq,m.text,m.created_at_ms,m.read_at_ms FROM task_planner_messages m",
    );
    if track_id.is_some() {
        query.push(" JOIN task_attempt_allocations a ON a.attempt_id=m.attempt_id");
    }
    query.push(" WHERE m.attempt_id=").push_bind(attempt_id);
    if let Some(track) = track_id {
        query.push(" AND a.track_id=").push_bind(track);
    }
    if unread_only {
        query.push(" AND m.read_at_ms IS NULL");
    }
    query.push(" ORDER BY m.seq");
    Ok(query
        .build_query_as::<(String, i64, String, i64, Option<i64>)>()
        .fetch_all(executor)
        .await?
        .into_iter()
        .map(decode)
        .collect())
}

fn decode(row: (String, i64, String, i64, Option<i64>)) -> TaskPlannerMessage {
    TaskPlannerMessage {
        message_id: row.0,
        seq: row.1,
        text: row.2,
        created_at_ms: row.3,
        read_at_ms: row.4,
    }
}
