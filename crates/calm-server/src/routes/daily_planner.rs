//! Read-only daily Track resolution and report change browsing.
use crate::daily_planner::{TIME_ZONE, track_for_date};
use crate::error::{CalmError, ErrorBody, Result};
use crate::extract::Query;
use crate::state::{AppState, RouteState};
use crate::workspace_reports::{
    self, ReportChangesPage, ReportChangesQuery, ReportEditsPage, ReportEditsQuery,
};
use axum::{Json, Router, extract::State, routing::get};
pub use calm_types::daily_planner::DailyTrackResolved;
use chrono::Utc;
use serde::Deserialize;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/today/daily", get(resolve_daily_track))
        .route("/api/today/report-changes", get(report_changes))
        .route("/api/today/report-edits", get(report_edits))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DailyQuery {
    date: Option<String>,
}

#[utoipa::path(get,path="/api/today/daily",tag="tracks",
    params(("date"=Option<String>,Query,description="Asia/Shanghai date; omitted means today")),
    responses((status=200,body=Option<DailyTrackResolved>,description="Daily Track, or null when the date has no Track"),(status=400,body=ErrorBody,description="Invalid date"),(status=500,body=ErrorBody,description="Internal error")))]
pub(crate) async fn resolve_daily_track(
    State(state): State<RouteState>,
    Query(query): Query<DailyQuery>,
) -> Result<Json<Option<DailyTrackResolved>>> {
    let date = match query.date {
        Some(date) => workspace_reports::parse_date(&date)?,
        None => Utc::now().with_timezone(&TIME_ZONE).date_naive(),
    };
    Ok(Json(track_for_date(&state, date).await?.map(|track_id| {
        DailyTrackResolved {
            date: date.to_string(),
            time_zone: TIME_ZONE.name().into(),
            track_id,
        }
    })))
}

fn pool(state: &RouteState) -> Result<&sqlx::SqlitePool> {
    state
        .mcp_context
        .sqlite_pool
        .as_ref()
        .ok_or_else(|| CalmError::Internal("report changes require sqlite".into()))
}

#[utoipa::path(get,path="/api/today/report-changes",tag="tracks",
    params(("date"=String,Query,description="YYYY-MM-DD in Asia/Shanghai"),("cursor"=Option<String>,Query,description="The previous page's next_cursor"),("through_event_id"=Option<i64>,Query,description="Snapshot cursor from the first page")),
    responses((status=200,body=ReportChangesPage,description="Report changes, including net reverts"),(status=400,body=ErrorBody,description="Invalid date or cursor"),(status=500,body=ErrorBody,description="Internal error")))]
pub(crate) async fn report_changes(
    State(state): State<RouteState>,
    Query(query): Query<ReportChangesQuery>,
) -> Result<Json<ReportChangesPage>> {
    Ok(Json(
        workspace_reports::changes(pool(&state)?, &query, TIME_ZONE).await?,
    ))
}

#[utoipa::path(get,path="/api/today/report-edits",tag="tracks",
    params(("date"=String,Query,description="YYYY-MM-DD"),("track_id"=String,Query,description="Track id"),("cursor"=Option<String>,Query,description="The previous page's next_cursor"),("through_event_id"=i64,Query,description="Snapshot cursor from report changes")),
    responses((status=200,body=ReportEditsPage,description="Individual report edits in event order"),(status=400,body=ErrorBody,description="Invalid date or cursor"),(status=500,body=ErrorBody,description="Internal error")))]
pub(crate) async fn report_edits(
    State(state): State<RouteState>,
    Query(query): Query<ReportEditsQuery>,
) -> Result<Json<ReportEditsPage>> {
    Ok(Json(
        workspace_reports::edits(pool(&state)?, &query, TIME_ZONE).await?,
    ))
}
