//! `report_series` rows: one per `(track, block, request_hash)`. Every reader selects by the full primary key; 'any row for this block' would let a pinned row for an old parameter set shadow the current one forever.
//! The summary read leaves the `data` column on disk: `calm.report.read` is on the planner's CAS path.

use serde_json::Value;
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::error::CalmError;

/// A stored row. `data` is `None` on a summary read even for an `ok` row.
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesRow {
    pub status: String,
    pub reason: Option<String>,
    pub as_of: String,
    pub resolved_at: i64,
    pub pinned: bool,
    pub summary: Option<Value>,
    pub data: Option<Value>,
}

#[derive(sqlx::FromRow)]
struct RawRow {
    status: String,
    reason: Option<String>,
    as_of: String,
    resolved_at: i64,
    pinned: bool,
    summary: Option<String>,
    data: Option<String>,
}

fn decode_json(column: &str, text: Option<String>) -> Result<Option<Value>, CalmError> {
    text.map(|text| {
        serde_json::from_str(&text)
            .map_err(|e| CalmError::Internal(format!("report_series: decode {column}: {e}")))
    })
    .transpose()
}

impl RawRow {
    fn decode(self) -> Result<SeriesRow, CalmError> {
        Ok(SeriesRow {
            status: self.status,
            reason: self.reason,
            as_of: self.as_of,
            resolved_at: self.resolved_at,
            pinned: self.pinned,
            summary: decode_json("summary", self.summary)?,
            data: decode_json("data", self.data)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Summary,
    Full,
}

const SUMMARY_COLUMNS: &str = "status, reason, as_of, resolved_at, pinned, summary, NULL AS data";
const FULL_COLUMNS: &str = "status, reason, as_of, resolved_at, pinned, summary, data";
const BY_KEY: &str =
    "FROM report_series WHERE track_id = ?1 AND block_id = ?2 AND request_hash = ?3";

fn select_sql(detail: Detail) -> String {
    let columns = match detail {
        Detail::Summary => SUMMARY_COLUMNS,
        Detail::Full => FULL_COLUMNS,
    };
    format!("SELECT {columns} {BY_KEY}")
}

/// Select the row for exactly this `(track, block, hash)`: one autocommit statement, no transaction.
pub async fn select_row(
    pool: &SqlitePool,
    track_id: &str,
    block_id: &str,
    request_hash: &str,
    detail: Detail,
) -> Result<Option<SeriesRow>, CalmError> {
    let row = sqlx::query_as::<_, RawRow>(&select_sql(detail))
        .bind(track_id)
        .bind(block_id)
        .bind(request_hash)
        .fetch_optional(pool)
        .await?;
    row.map(RawRow::decode).transpose()
}

/// What one resolution writes.
#[derive(Debug, Clone, PartialEq)]
pub struct NewRow {
    pub status: String,
    pub reason: Option<String>,
    pub as_of: String,
    pub resolved_at: i64,
    pub pinned: bool,
    pub summary: Option<Value>,
    pub data: Option<Value>,
}

/// `INSERT … ON CONFLICT DO UPDATE … WHERE pinned = 0`: a pinned row is immutable at the DB layer. Returns rows affected (0 when pinned).
pub async fn upsert_row_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track_id: &str,
    block_id: &str,
    request_hash: &str,
    row: &NewRow,
) -> Result<u64, CalmError> {
    let summary = row
        .summary
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| CalmError::Internal(format!("report_series: encode summary: {e}")))?;
    let data = row
        .data
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| CalmError::Internal(format!("report_series: encode data: {e}")))?;
    let sql = concat!(
        "INSERT INTO report_series ",
        "(track_id, block_id, request_hash, status, reason, as_of, ",
        " resolved_at, pinned, summary, data) ",
        "VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) ",
        "ON CONFLICT(track_id, block_id, request_hash) DO UPDATE SET ",
        "status = excluded.status, reason = excluded.reason, ",
        "as_of = excluded.as_of, resolved_at = excluded.resolved_at, ",
        "pinned = excluded.pinned, summary = excluded.summary, ",
        "data = excluded.data ",
        "WHERE report_series.pinned = 0"
    );
    let result = sqlx::query(sql)
        .bind(track_id)
        .bind(block_id)
        .bind(request_hash)
        .bind(&row.status)
        .bind(&row.reason)
        .bind(&row.as_of)
        .bind(row.resolved_at)
        .bind(row.pinned)
        .bind(summary)
        .bind(data)
        .execute(&mut **tx)
        .await?;
    Ok(result.rows_affected())
}

/// Fork: copy every row of `source_track_id` to `target_track_id` inside the fork's own transaction; block ids are preserved.
pub async fn copy_rows_tx(
    tx: &mut Transaction<'_, Sqlite>,
    source_track_id: &str,
    target_track_id: &str,
) -> Result<u64, CalmError> {
    let sql = concat!(
        "INSERT INTO report_series ",
        "(track_id, block_id, request_hash, status, reason, as_of, ",
        " resolved_at, pinned, summary, data) ",
        "SELECT ?1, block_id, request_hash, status, reason, as_of, ",
        " resolved_at, pinned, summary, data ",
        "FROM report_series WHERE track_id = ?2"
    );
    let result = sqlx::query(sql)
        .bind(target_track_id)
        .bind(source_track_id)
        .execute(&mut **tx)
        .await?;
    Ok(result.rows_affected())
}
