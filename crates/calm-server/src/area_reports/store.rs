//! The SQL behind `area/reports/`. Every statement is keyed by the caller's area and runs inside the
//! one read transaction its caller opens, so a listing, its tags and a body read agree.

use std::collections::HashMap;

use sqlx::{Sqlite, Transaction};

/// One report of the area: the track it belongs to, its title, and the report card's own update time.
pub(super) struct Row {
    pub track_id: String,
    pub card_id: String,
    pub title: String,
    /// `cards.updated_at` of the track-report card (ms). Tag changes and body writes move it;
    /// other track activity moves only `tracks.updated_at`, which this view never reads.
    pub updated_at: i64,
}

/// Every track of `area_id` with a report card, ordered by track id.
pub(super) async fn rows(
    tx: &mut Transaction<'_, Sqlite>,
    area_id: &str,
) -> Result<Vec<Row>, sqlx::Error> {
    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(concat!(
        "SELECT t.id, c.id, t.title, c.updated_at FROM tracks t ",
        "JOIN cards c ON c.track_id = t.id AND c.kind = 'track-report' ",
        "WHERE t.area_id = ?1 ORDER BY t.id ASC"
    ))
    .bind(area_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(track_id, card_id, title, updated_at)| Row {
            track_id,
            card_id,
            title,
            updated_at,
        })
        .collect())
}

/// The tags of every report in `area_id`, by track id, each list in insertion order.
pub(super) async fn tags(
    tx: &mut Transaction<'_, Sqlite>,
    area_id: &str,
) -> Result<HashMap<String, Vec<String>>, sqlx::Error> {
    let rows: Vec<(String, String)> = sqlx::query_as(concat!(
        "SELECT rt.track_id, rt.tag FROM report_tags rt JOIN tracks t ON t.id = rt.track_id ",
        "WHERE t.area_id = ?1 ORDER BY rt.track_id ASC, rt.ordinal ASC, rt.tag ASC"
    ))
    .bind(area_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut tags: HashMap<String, Vec<String>> = HashMap::new();
    for (track_id, tag) in rows {
        tags.entry(track_id).or_default().push(tag);
    }
    Ok(tags)
}

/// The report card's payload JSON text, re-checked against its track and the area.
pub(super) async fn payload(
    tx: &mut Transaction<'_, Sqlite>,
    area_id: &str,
    row: &Row,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(concat!(
        "SELECT c.payload FROM cards c JOIN tracks t ON t.id = c.track_id ",
        "WHERE c.id = ?1 AND c.track_id = ?2 AND c.kind = 'track-report' AND t.area_id = ?3"
    ))
    .bind(&row.card_id)
    .bind(&row.track_id)
    .bind(area_id)
    .fetch_optional(&mut **tx)
    .await
}
