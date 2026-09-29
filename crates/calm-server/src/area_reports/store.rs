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

/// The report rows of one area in one statement, ordered by track id: track id, card id, title, the
/// card's `updated_at` and its tags as a JSON array, then the columns `$extra` appends.
macro_rules! area_rows_sql {
    ($extra:literal) => {
        concat!(
            "SELECT t.id, c.id, t.title, c.updated_at, ",
            "(SELECT json_group_array(rt.tag ORDER BY rt.ordinal ASC, rt.tag ASC) ",
            "FROM report_tags rt WHERE rt.track_id = t.id)",
            $extra,
            " FROM tracks t JOIN cards c ON c.track_id = t.id AND c.kind = 'track-report' ",
            "WHERE t.area_id = ?1 ORDER BY t.id ASC"
        )
    };
}

fn row(
    track_id: String,
    card_id: String,
    title: String,
    updated_at: i64,
    tags: &str,
) -> Result<Row, sqlx::Error> {
    Ok(Row {
        tags: serde_json::from_str(tags).map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        track_id,
        card_id,
        title,
        updated_at,
    })
}

/// Every track of `area_id` with a report card and its tags, ordered by track id, in one statement.
pub(super) async fn rows(pool: &SqlitePool, area_id: &str) -> Result<Vec<Row>, sqlx::Error> {
    let rows: Vec<(String, String, String, i64, String)> = sqlx::query_as(area_rows_sql!(""))
        .bind(area_id)
        .fetch_all(pool)
        .await?;
    rows.into_iter()
        .map(|(track_id, card_id, title, updated_at, tags)| {
            row(track_id, card_id, title, updated_at, &tags)
        })
        .collect()
}

/// A report card's stored payload JSON and whether it has a CRDT: what
/// `track_report::report_blocks_snapshot_from_row` projects blocks from without loading Automerge.
pub(super) struct Projection {
    pub payload: String,
    pub has_crdt: bool,
}

/// [`rows`] plus each report's [`Projection`], in the same one statement.
pub(super) async fn rows_with_projection(
    pool: &SqlitePool,
    area_id: &str,
) -> Result<Vec<(Row, Projection)>, sqlx::Error> {
    let rows: Vec<(String, String, String, i64, String, String, bool)> =
        sqlx::query_as(area_rows_sql!(", json(c.payload), c.body_crdt IS NOT NULL"))
            .bind(area_id)
            .fetch_all(pool)
            .await?;
    rows.into_iter()
        .map(
            |(track_id, card_id, title, updated_at, tags, payload, has_crdt)| {
                Ok((
                    row(track_id, card_id, title, updated_at, &tags)?,
                    Projection { payload, has_crdt },
                ))
            },
        )
        .collect()
}

/// One report card row as a read needs it: the payload JSON text, the report CRDT and the card's
/// update time, from one statement.
pub(super) struct Stored {
    pub payload: String,
    pub body_crdt: Option<Vec<u8>>,
    pub updated_at: i64,
}

/// The report card's row, re-checked against its track and the area; `None` when the report left
/// the area (deleted or moved) after `rows` resolved it.
pub(super) async fn report(
    pool: &SqlitePool,
    area_id: &str,
    row: &Row,
) -> Result<Option<Stored>, sqlx::Error> {
    let stored: Option<(String, Option<Vec<u8>>, i64)> = sqlx::query_as(concat!(
        "SELECT c.payload, c.body_crdt, c.updated_at FROM cards c JOIN tracks t ON t.id = c.track_id ",
        "WHERE c.id = ?1 AND c.track_id = ?2 AND c.kind = 'track-report' AND t.area_id = ?3"
    ))
    .bind(&row.card_id)
    .bind(&row.track_id)
    .bind(area_id)
    .fetch_optional(pool)
    .await?;
    Ok(stored.map(|(payload, body_crdt, updated_at)| Stored {
        payload,
        body_crdt,
        updated_at,
    }))
}
