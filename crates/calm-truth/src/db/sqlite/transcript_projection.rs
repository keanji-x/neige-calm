//! Canonical receipt projection and the shared transcript insertion primitive.
use super::begin_immediate_tx;
use crate::db::{TranscriptReceiptItem, TranscriptRow};
use crate::error::Result;
use crate::model::now_ms;
use sqlx::{Row, Sqlite};

#[allow(clippy::too_many_arguments)]
pub(super) async fn insert_transcript_row<'e, E: sqlx::Executor<'e, Database = Sqlite>>(
    executor: E,
    worker: &str,
    card: &str,
    track: &str,
    thread: &str,
    turn: Option<&str>,
    id: Option<&str>,
    kind: Option<&str>,
    method: &str,
    params: &str,
    segments: Option<&str>,
) -> Result<i64> {
    let row = sqlx::query(concat!(
        "INSERT INTO harness_items (worker_session_id,card_id,track_id,thread_id,turn_id,",
        "item_uuid,item_type,method,params,input_segments,created_at_ms) ",
        "VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) RETURNING id"
    ))
    .bind(worker)
    .bind(card)
    .bind(track)
    .bind(thread)
    .bind(turn)
    .bind(id)
    .bind(kind)
    .bind(method)
    .bind(params)
    .bind(segments)
    .bind(now_ms())
    .fetch_one(executor)
    .await?;
    Ok(row.get("id"))
}

pub(super) async fn restore(
    pool: &sqlx::SqlitePool,
    worker: &str,
    card: &str,
    track: &str,
    thread: &str,
    turn: &str,
    items: &[TranscriptReceiptItem],
) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let mut tx = begin_immediate_tx(pool).await?;
    let rows: Vec<TranscriptRow> = sqlx::query_as(concat!(
        "SELECT id,turn_id,item_uuid,item_type,method,params,input_segments FROM harness_items ",
        "WHERE worker_session_id=?1 AND card_id=?2 AND thread_id=?3 AND turn_id=?4 ORDER BY id"
    ))
    .bind(worker)
    .bind(card)
    .bind(thread)
    .bind(turn)
    .fetch_all(&mut *tx)
    .await?;
    let mut order = Vec::new();
    for row in &rows {
        if let Some(id) = row.item_uuid.as_deref()
            && items.iter().any(|item| item.item_uuid == id)
            && !order.contains(&id)
        {
            order.push(id);
        }
    }
    let exact = items.iter().all(|item| {
        rows.iter().any(|row| {
            row.item_uuid.as_deref() == Some(item.item_uuid.as_str())
                && row.method == item.method
                && row.params == item.params
        })
    });
    if exact
        && order
            == items
                .iter()
                .map(|item| item.item_uuid.as_str())
                .collect::<Vec<_>>()
    {
        tx.commit().await?;
        return Ok(());
    }
    // Rebuild only receipt-owned item identities. User input and other turns stay intact.
    let ids = serde_json::to_string(&items.iter().map(|item| &item.item_uuid).collect::<Vec<_>>())?;
    sqlx::query(concat!(
        "DELETE FROM harness_items WHERE worker_session_id=?1 AND card_id=?2 AND thread_id=?3 ",
        "AND turn_id=?4 AND item_uuid IN (SELECT value FROM json_each(?5)) ",
        "AND method IN ('item/started','item/completed')"
    ))
    .bind(worker)
    .bind(card)
    .bind(thread)
    .bind(turn)
    .bind(ids)
    .execute(&mut *tx)
    .await?;
    for item in items {
        insert_transcript_row(
            &mut *tx,
            worker,
            card,
            track,
            thread,
            Some(turn),
            Some(&item.item_uuid),
            Some(&item.item_type),
            &item.method,
            &item.params,
            None,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
