//! The SQL behind `area/reports/`. Every statement is keyed by the caller's area and is one
//! autocommit statement, so each is a point-in-time read without a transaction (a parked deferred
//! read tx could close a shared-cache lock cycle; see `deferred_write_tx_invariant`).

use sqlx::SqlitePool;

/// One report of the area: the track it belongs to, its title, tags and the report card's own update time.
pub(super) struct Row {
    pub track_id: String,
    pub card_id: String,
    pub title: String,
    /// `cards.updated_at` of the track-report card (ms). Tag changes and body writes move it;
    /// other track activity moves only `tracks.updated_at`, which this view never reads.
    pub updated_at: i64,
    /// In insertion order; read in the same statement as `updated_at`, so the two agree.
    pub tags: Vec<String>,
}

/// Every track of `area_id` with a report card and its tags, ordered by track id, in one statement.
pub(super) async fn rows(pool: &SqlitePool, area_id: &str) -> Result<Vec<Row>, sqlx::Error> {
    let rows: Vec<(String, String, String, i64, String)> = sqlx::query_as(concat!(
        "SELECT t.id, c.id, t.title, c.updated_at, ",
        "(SELECT json_group_array(rt.tag ORDER BY rt.ordinal ASC, rt.tag ASC) ",
        "FROM report_tags rt WHERE rt.track_id = t.id) ",
        "FROM tracks t JOIN cards c ON c.track_id = t.id AND c.kind = 'track-report' ",
        "WHERE t.area_id = ?1 ORDER BY t.id ASC"
    ))
    .bind(area_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|(track_id, card_id, title, updated_at, tags)| {
            Ok(Row {
                tags: serde_json::from_str(&tags).map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
                track_id,
                card_id,
                title,
                updated_at,
            })
        })
        .collect()
}

/// The report card's payload JSON text, re-checked against its track and the area; `None` when
/// the report left the area (deleted or moved) after `rows` resolved it.
pub(super) async fn payload(
    pool: &SqlitePool,
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
    .fetch_optional(pool)
    .await
}
