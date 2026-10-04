//! Read-only workspace report projection shared by HTTP and granted Planner tools.
pub use calm_types::daily_planner::{
    ReportChange, ReportChangesPage, ReportEdit, ReportEditEntry, ReportEditsPage,
};

use crate::error::{CalmError, Result};
use chrono::{NaiveDate, TimeZone};
use chrono_tz::Tz;
use serde::Deserialize;
use sqlx::{FromRow, SqlitePool};
use utoipa::ToSchema;

pub const PAGE_SIZE: usize = 20;
pub const PATCH_LINES: usize = 500;

#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportChangesQuery {
    pub date: String,
    pub after: Option<String>,
    pub through_event_id: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportEditsQuery {
    pub date: String,
    pub track_id: String,
    pub after: Option<i64>,
    pub through_event_id: i64,
}

pub fn parse_date(date: &str) -> Result<NaiveDate> {
    let parsed = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|_| CalmError::BadRequest("date must be YYYY-MM-DD".into()))?;
    if parsed.to_string() != date {
        return Err(CalmError::BadRequest("date must be YYYY-MM-DD".into()));
    }
    Ok(parsed)
}

pub fn day_window(date: NaiveDate, zone: Tz) -> Result<(i64, i64)> {
    let midnight = |day: NaiveDate| {
        zone.from_local_datetime(&day.and_hms_opt(0, 0, 0).expect("midnight"))
            .single()
            .map(|at| at.timestamp_millis())
            .ok_or_else(|| {
                CalmError::BadRequest("date has no unique midnight in this time zone".into())
            })
    };
    let next = date
        .succ_opt()
        .ok_or_else(|| CalmError::BadRequest("date has no following day".into()))?;
    Ok((midnight(date)?, midnight(next)?))
}

#[derive(FromRow)]
struct ChangeRow {
    track_id: String,
    track_title: String,
    area_id: String,
    area_name: String,
    edit_count: i64,
    first_event_id: i64,
    last_event_id: i64,
    first_payload: String,
    last_payload: String,
}

/// Snapshot-bound pagination includes closed Tracks and edits that net to a revert.
pub async fn changes(
    pool: &SqlitePool,
    query: &ReportChangesQuery,
    zone: Tz,
) -> Result<ReportChangesPage> {
    let (start, end) = day_window(parse_date(&query.date)?, zone)?;
    if query.after.is_some() && query.through_event_id.is_none() {
        return Err(CalmError::BadRequest(
            "continuation requires through_event_id".into(),
        ));
    }
    let mut tx = pool.begin().await?;
    let through = match query.through_event_id {
        Some(id) if id >= 0 => id,
        Some(_) => {
            return Err(CalmError::BadRequest(
                "through_event_id cannot be negative".into(),
            ));
        }
        None => {
            sqlx::query_scalar::<_, i64>("SELECT COALESCE(MAX(id),0) FROM events")
                .fetch_one(&mut *tx)
                .await?
        }
    };
    let mut rows: Vec<ChangeRow> = sqlx::query_as(r#"
        WITH edits AS (
            SELECT e.scope_track AS track_id,MIN(e.id) AS first_event_id,MAX(e.id) AS last_event_id,COUNT(*) AS edit_count
            FROM events e JOIN tracks t ON t.id=e.scope_track JOIN areas a ON a.id=t.area_id
            WHERE e.kind='track.report_edited' AND e.at>=?1 AND e.at<?2 AND e.id<=?3 AND a.kind='user'
            GROUP BY e.scope_track
        )
        SELECT t.id AS track_id,t.title AS track_title,a.id AS area_id,a.name AS area_name,
               edits.edit_count,edits.first_event_id,edits.last_event_id,
               first.payload AS first_payload,last.payload AS last_payload
        FROM edits JOIN tracks t ON t.id=edits.track_id JOIN areas a ON a.id=t.area_id
        JOIN events first ON first.id=edits.first_event_id JOIN events last ON last.id=edits.last_event_id
        WHERE (?4 IS NULL OR t.id>?4) ORDER BY t.id LIMIT ?5
    "#).bind(start).bind(end).bind(through).bind(&query.after)
        .bind((PAGE_SIZE+1) as i64).fetch_all(&mut *tx).await?;
    tx.commit().await?;
    let next_cursor = (rows.len() > PAGE_SIZE).then(|| rows[PAGE_SIZE - 1].track_id.clone());
    rows.truncate(PAGE_SIZE);
    let changes = rows
        .into_iter()
        .map(|row| {
            let first: ReportEdit = serde_json::from_str(&row.first_payload)?;
            let last: ReportEdit = serde_json::from_str(&row.last_payload)?;
            if first.track_id != row.track_id || last.track_id != row.track_id {
                return Err(CalmError::Internal(
                    "report edit scope disagrees with its Track".into(),
                ));
            }
            let (patch, patch_truncated) = crate::track_vcs::unified_patch(
                "report.md",
                &first.body_before,
                &last.body_after,
                PATCH_LINES,
            );
            Ok(ReportChange {
                track_id: row.track_id,
                track_title: row.track_title,
                area_id: row.area_id,
                area_name: row.area_name,
                edit_count: row.edit_count,
                first_event_id: row.first_event_id,
                last_event_id: row.last_event_id,
                summary_before: first.summary_before,
                summary_after: last.summary_after,
                patch,
                patch_truncated,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ReportChangesPage {
        date: query.date.clone(),
        time_zone: zone.name().into(),
        through_event_id: through,
        changes,
        next_cursor,
    })
}

pub async fn edits(
    pool: &SqlitePool,
    query: &ReportEditsQuery,
    zone: Tz,
) -> Result<ReportEditsPage> {
    let (start, end) = day_window(parse_date(&query.date)?, zone)?;
    let after = query.after.unwrap_or(0);
    if after < 0 || query.through_event_id < 0 {
        return Err(CalmError::BadRequest(
            "event cursors cannot be negative".into(),
        ));
    }
    let mut rows:Vec<(i64,i64,String)> = sqlx::query_as(r#"
        SELECT e.id,e.at,e.payload FROM events e JOIN tracks t ON t.id=e.scope_track JOIN areas a ON a.id=t.area_id
        WHERE e.kind='track.report_edited' AND e.scope_track=?1 AND a.kind='user'
        AND e.at>=?2 AND e.at<?3 AND e.id>?4 AND e.id<=?5 ORDER BY e.id LIMIT ?6
    "#).bind(&query.track_id).bind(start).bind(end).bind(after).bind(query.through_event_id)
        .bind((PAGE_SIZE+1) as i64).fetch_all(pool).await?;
    let next_cursor = (rows.len() > PAGE_SIZE).then(|| rows[PAGE_SIZE - 1].0);
    rows.truncate(PAGE_SIZE);
    let edits = rows
        .into_iter()
        .map(|(event_id, at, payload)| {
            let edit: ReportEdit = serde_json::from_str(&payload)?;
            if edit.track_id != query.track_id {
                return Err(CalmError::Internal(
                    "report edit scope disagrees with its Track".into(),
                ));
            }
            Ok(ReportEditEntry { event_id, at, edit })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ReportEditsPage { edits, next_cursor })
}

#[cfg(test)]
mod tests;
