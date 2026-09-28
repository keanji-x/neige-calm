//! `report_tags` rows: one per `(track_id, tag)`, ordered by `ordinal`. The only writer is
//! [`apply_tx`], which bumps the report card's `updated_at` in the same transaction whenever the
//! tag set changes, so the report's own update time covers its tags.

use sqlx::{Sqlite, Transaction};

use super::MAX_TAGS_PER_REPORT;
use crate::db::sqlite::card_update_tx;
use crate::error::CalmError;
use crate::model::{Card, CardPatch};

/// A track's tags in insertion order.
pub async fn list<'e, E>(executor: E, track_id: &str) -> Result<Vec<String>, CalmError>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    let tags = sqlx::query_scalar(
        "SELECT tag FROM report_tags WHERE track_id = ?1 ORDER BY ordinal ASC, tag ASC",
    )
    .bind(track_id)
    .fetch_all(executor)
    .await?;
    Ok(tags)
}

/// What one [`apply_tx`] left behind.
#[derive(Debug)]
pub struct Applied {
    pub tags: Vec<String>,
    /// The report card after its `updated_at` bump; `None` when the call changed no row.
    pub touched_report: Option<Card>,
}

/// Add `add` (an existing tag keeps its place), then remove `remove` (an absent tag is a no-op).
/// Tags must already be normalized. Refused when the track has no report card or the result would
/// exceed [`MAX_TAGS_PER_REPORT`]; the caller's transaction then rolls every row back.
pub async fn apply_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    add: &[String],
    remove: &[String],
) -> Result<Applied, CalmError> {
    let report_card_id: Option<String> =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id = ?1 AND kind = 'track-report'")
            .bind(track_id)
            .fetch_optional(&mut **tx)
            .await?;
    let Some(report_card_id) = report_card_id else {
        return Err(CalmError::BadRequest(format!(
            "track {track_id} has no report card to tag"
        )));
    };
    let mut changed = false;
    for tag in add {
        let inserted = sqlx::query(concat!(
            "INSERT INTO report_tags (track_id, tag, ordinal) ",
            "SELECT ?1, ?2, COALESCE(MAX(ordinal), 0) + 1 FROM report_tags WHERE track_id = ?1 ",
            "ON CONFLICT (track_id, tag) DO NOTHING"
        ))
        .bind(track_id)
        .bind(tag)
        .execute(&mut **tx)
        .await?;
        changed |= inserted.rows_affected() > 0;
    }
    for tag in remove {
        let deleted = sqlx::query("DELETE FROM report_tags WHERE track_id = ?1 AND tag = ?2")
            .bind(track_id)
            .bind(tag)
            .execute(&mut **tx)
            .await?;
        changed |= deleted.rows_affected() > 0;
    }
    let tags = list(&mut **tx, track_id).await?;
    if tags.len() > MAX_TAGS_PER_REPORT {
        return Err(CalmError::BadRequest(format!(
            "a report carries at most {MAX_TAGS_PER_REPORT} tags; this change would leave {}",
            tags.len()
        )));
    }
    let touched_report = if changed {
        Some(card_update_tx(tx, &report_card_id, CardPatch::default()).await?)
    } else {
        None
    };
    Ok(Applied {
        tags,
        touched_report,
    })
}

/// Fork: copy every tag of `source_track_id` to `target_track_id`, order included, inside the fork's own transaction.
pub async fn copy_rows_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source_track_id: &str,
    target_track_id: &str,
) -> Result<u64, CalmError> {
    let result = sqlx::query(
        "INSERT INTO report_tags (track_id, tag, ordinal) \
         SELECT ?1, tag, ordinal FROM report_tags WHERE track_id = ?2",
    )
    .bind(target_track_id)
    .bind(source_track_id)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected())
}
