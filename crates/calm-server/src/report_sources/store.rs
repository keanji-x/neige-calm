//! `report_sources` rows (#1669 §2.4): one per capture, keyed
//! `(track_id, source_id)`.
//!
//! Writers take a transaction (the capture tool runs insert + quota check
//! in one `write_in_tx`; the fork copies inside the create transaction).
//! Readers run one autocommit statement on the pool — no deferred read
//! transaction (#930). The default list read leaves `body` on disk.

use std::collections::HashMap;

use sqlx::{Sqlite, SqlitePool, Transaction};

use super::{Origin, Provenance, Quote};
use crate::error::CalmError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// Everything but `body`.
    Summary,
    Full,
}

/// A stored row. `body` is `None` on a summary read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    pub track_id: String,
    pub source_id: String,
    pub provenance: Provenance,
    pub origin: Origin,
    pub title: String,
    pub published_at: Option<String>,
    pub body: Option<String>,
    pub body_bytes: usize,
    pub body_sha256: String,
    /// Unix milliseconds.
    pub captured_at: i64,
    pub quotes: Vec<Quote>,
}

#[derive(sqlx::FromRow)]
struct RawRow {
    track_id: String,
    source_id: String,
    provenance: String,
    origin: String,
    title: String,
    published_at: Option<String>,
    body: Option<String>,
    body_bytes: i64,
    body_sha256: String,
    captured_at: i64,
    quotes: String,
}

impl RawRow {
    fn decode(self) -> Result<SourceRow, CalmError> {
        let provenance = Provenance::parse(&self.provenance).ok_or_else(|| {
            CalmError::Internal(format!(
                "report_sources: unknown provenance {:?}",
                self.provenance
            ))
        })?;
        let origin: Origin = serde_json::from_str(&self.origin)
            .map_err(|e| CalmError::Internal(format!("report_sources: decode origin: {e}")))?;
        let quotes: Vec<Quote> = serde_json::from_str(&self.quotes)
            .map_err(|e| CalmError::Internal(format!("report_sources: decode quotes: {e}")))?;
        let body_bytes = usize::try_from(self.body_bytes).unwrap_or_default();
        Ok(SourceRow {
            track_id: self.track_id,
            source_id: self.source_id,
            provenance,
            origin,
            title: self.title,
            published_at: self.published_at,
            body: self.body,
            body_bytes,
            body_sha256: self.body_sha256,
            captured_at: self.captured_at,
            quotes,
        })
    }
}

const SUMMARY_COLUMNS: &str = concat!(
    "track_id, source_id, provenance, origin, title, published_at, ",
    "NULL AS body, length(CAST(body AS BLOB)) AS body_bytes, body_sha256, ",
    "captured_at, quotes"
);
const FULL_COLUMNS: &str = concat!(
    "track_id, source_id, provenance, origin, title, published_at, ",
    "body, length(CAST(body AS BLOB)) AS body_bytes, body_sha256, ",
    "captured_at, quotes"
);

fn columns(detail: Detail) -> &'static str {
    match detail {
        Detail::Summary => SUMMARY_COLUMNS,
        Detail::Full => FULL_COLUMNS,
    }
}

/// What one capture writes. `body_sha256` and `captured_at` are computed
/// by the caller from `body` and the clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSource {
    pub source_id: String,
    pub provenance: Provenance,
    pub origin: Origin,
    pub title: String,
    pub published_at: Option<String>,
    pub body: String,
    pub body_sha256: String,
    pub captured_at: i64,
    pub quotes: Vec<Quote>,
}

fn encode_json<T: serde::Serialize>(what: &str, value: &T) -> Result<String, CalmError> {
    serde_json::to_string(value)
        .map_err(|e| CalmError::Internal(format!("report_sources: encode {what}: {e}")))
}

pub async fn insert_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    row: &NewSource,
) -> Result<(), CalmError> {
    let origin = encode_json("origin", &row.origin)?;
    let quotes = encode_json("quotes", &row.quotes)?;
    let sql = concat!(
        "INSERT INTO report_sources ",
        "(track_id, source_id, provenance, origin, title, published_at, ",
        " body, body_sha256, captured_at, quotes) ",
        "VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
    );
    sqlx::query(sql)
        .bind(track_id)
        .bind(&row.source_id)
        .bind(row.provenance.as_str())
        .bind(origin)
        .bind(&row.title)
        .bind(&row.published_at)
        .bind(&row.body)
        .bind(&row.body_sha256)
        .bind(row.captured_at)
        .bind(quotes)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Replace the `quotes` column. The caller has computed the new list from
/// the stored one with [`super::append_quotes`], so this only ever grows.
pub async fn set_quotes_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    source_id: &str,
    quotes: &[Quote],
) -> Result<u64, CalmError> {
    let quotes = encode_json("quotes", &quotes)?;
    let result =
        sqlx::query("UPDATE report_sources SET quotes = ?3 WHERE track_id = ?1 AND source_id = ?2")
            .bind(track_id)
            .bind(source_id)
            .bind(quotes)
            .execute(&mut **tx)
            .await?;
    Ok(result.rows_affected())
}

pub async fn exists_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    source_id: &str,
) -> Result<bool, CalmError> {
    let found: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM report_sources WHERE track_id = ?1 AND source_id = ?2")
            .bind(track_id)
            .bind(source_id)
            .fetch_optional(&mut **tx)
            .await?;
    Ok(found.is_some())
}

pub async fn get_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    source_id: &str,
    detail: Detail,
) -> Result<Option<SourceRow>, CalmError> {
    let sql = format!(
        "SELECT {} FROM report_sources WHERE track_id = ?1 AND source_id = ?2",
        columns(detail)
    );
    let row = sqlx::query_as::<_, RawRow>(&sql)
        .bind(track_id)
        .bind(source_id)
        .fetch_optional(&mut **tx)
        .await?;
    row.map(RawRow::decode).transpose()
}

pub async fn get(
    pool: &SqlitePool,
    track_id: &str,
    source_id: &str,
    detail: Detail,
) -> Result<Option<SourceRow>, CalmError> {
    let sql = format!(
        "SELECT {} FROM report_sources WHERE track_id = ?1 AND source_id = ?2",
        columns(detail)
    );
    let row = sqlx::query_as::<_, RawRow>(&sql)
        .bind(track_id)
        .bind(source_id)
        .fetch_optional(pool)
        .await?;
    row.map(RawRow::decode).transpose()
}

/// Every source of the track, oldest capture first, without bodies.
pub async fn list(pool: &SqlitePool, track_id: &str) -> Result<Vec<SourceRow>, CalmError> {
    let sql = format!(
        "SELECT {SUMMARY_COLUMNS} FROM report_sources WHERE track_id = ?1 \
         ORDER BY captured_at ASC, source_id ASC"
    );
    let rows = sqlx::query_as::<_, RawRow>(&sql)
        .bind(track_id)
        .fetch_all(pool)
        .await?;
    rows.into_iter().map(RawRow::decode).collect()
}

/// `(count, body bytes)` for the quota check.
pub async fn count_and_bytes_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
) -> Result<(i64, i64), CalmError> {
    let row: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(length(CAST(body AS BLOB))), 0) \
         FROM report_sources WHERE track_id = ?1",
    )
    .bind(track_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(row)
}

/// `source_id → quote ids` for every source of the track, in one
/// statement — what the receipt warnings resolve links against.
pub async fn anchor_index(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<HashMap<String, Vec<String>>, CalmError> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT source_id, quotes FROM report_sources WHERE track_id = ?1")
            .bind(track_id)
            .fetch_all(pool)
            .await?;
    let mut index = HashMap::with_capacity(rows.len());
    for (source_id, quotes) in rows {
        let quotes: Vec<Quote> = serde_json::from_str(&quotes)
            .map_err(|e| CalmError::Internal(format!("report_sources: decode quotes: {e}")))?;
        index.insert(source_id, quotes.into_iter().map(|q| q.id).collect());
    }
    Ok(index)
}

/// Fork (§2.4): copy every row of `source_track_id` to `target_track_id`
/// inside the fork's own transaction, ids and anchors verbatim.
pub async fn copy_rows_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source_track_id: &str,
    target_track_id: &str,
) -> Result<u64, CalmError> {
    let sql = concat!(
        "INSERT INTO report_sources ",
        "(track_id, source_id, provenance, origin, title, published_at, ",
        " body, body_sha256, captured_at, quotes) ",
        "SELECT ?1, source_id, provenance, origin, title, published_at, ",
        " body, body_sha256, captured_at, quotes ",
        "FROM report_sources WHERE track_id = ?2"
    );
    let result = sqlx::query(sql)
        .bind(target_track_id)
        .bind(source_track_id)
        .execute(&mut **tx)
        .await?;
    Ok(result.rows_affected())
}
