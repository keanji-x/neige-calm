//! Canonical receipt projection and shared transcript write authorization.
use super::begin_immediate_tx;
use crate::db::{TranscriptReceiptItem, TranscriptRow};
use crate::error::Result;
use crate::model::now_ms;
use sqlx::{Row, Sqlite};

/// Parameters 1..=3 always name the calling session, card and track. Every session-owned
/// transcript write embeds this one predicate, so a write from a session that is no longer the
/// card's current one (a superseded run loop, an echo or cleanup admitted before a new
/// conversation or restart) affects no row, whenever it lands. A current failed owner may still
/// settle.
pub(super) const CURRENT_SESSION_BINDING: &str = "SELECT 1 FROM worker_sessions ws \
    JOIN cards c ON c.session_id=ws.id AND c.id=ws.card_id AND c.track_id=ws.track_id \
    WHERE ws.id=?1 AND c.id=?2 AND c.track_id=?3 AND ws.state <> 'superseded'";

/// A fenced write that matched nothing because its session is not current.
pub(super) fn log_fenced(write: &str, worker: &str, card: &str) {
    tracing::debug!(
        write,
        worker_session_id = worker,
        card_id = card,
        "transcript write dropped: the session is not the card's current session"
    );
}

/// `None` when the calling session is not the card's current session.
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
) -> Result<Option<i64>> {
    let row = sqlx::query(&format!(
        "INSERT INTO harness_items (worker_session_id,card_id,track_id,thread_id,turn_id,\
         item_uuid,item_type,method,params,input_segments,created_at_ms) \
         SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11 \
         WHERE EXISTS ({CURRENT_SESSION_BINDING}) RETURNING id"
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
    .fetch_optional(executor)
    .await?;
    if row.is_none() {
        log_fenced("insert", worker, card);
    }
    Ok(row.map(|row| row.get("id")))
}

pub(super) async fn restore(
    pool: &crate::db::sqlite::SqliteDb,
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
    .fetch_all(&mut **tx)
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
    sqlx::query(&format!(
        "DELETE FROM harness_items WHERE worker_session_id=?1 AND card_id=?2 AND track_id=?3 \
         AND thread_id=?4 AND turn_id=?5 AND item_uuid IN (SELECT value FROM json_each(?6)) \
         AND method IN ('item/started','item/completed') AND EXISTS ({CURRENT_SESSION_BINDING})"
    ))
    .bind(worker)
    .bind(card)
    .bind(track)
    .bind(thread)
    .bind(turn)
    .bind(ids)
    .execute(&mut **tx)
    .await?;
    for item in items {
        // A non-current session inserts nothing either; the rebuild is all or nothing.
        if insert_transcript_row(
            &mut **tx,
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
        .await?
        .is_none()
        {
            return Ok(());
        }
    }
    tx.commit().await?;
    Ok(())
}
