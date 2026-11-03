//! Read-only daily Track resolution and report change browsing.
use crate::auth::Principal;
use crate::daily_planner::{TIME_ZONE, track_for_date};
use crate::error::{CalmError, ErrorBody, Result};
use crate::extract::{Json, Query};
use crate::state::{AppState, RouteState};
use crate::workspace_reports::{
    self, ReportChangesPage, ReportChangesQuery, ReportEditsPage, ReportEditsQuery,
};
use axum::{Router, extract::State, routing::get};
pub use calm_types::daily_planner::DailyTrackResolved;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/today/daily", get(resolve_daily_track))
        .route("/api/today/supervisor", get(supervisor_snapshot))
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

fn pool(state: &RouteState) -> Result<&crate::db::sqlite::SqliteDb> {
    state
        .mcp_context
        .sqlite_db
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
        workspace_reports::edits(pool(&state)?.read_pool(), &query, TIME_ZONE).await?,
    ))
}

/// A current observation, with null values for unavailable sources.
#[derive(Serialize, ToSchema)]
pub(crate) struct SupervisorSnapshot {
    observed_at_ms: i64,
    kernel_version: String,
    build_sha: Option<String>,
    database_probe_ms: Option<f64>,
    scheduler_boot_sweep_completed: bool,
    retired_workspaces: Option<RetiredWorkspaceSummary>,
}

#[derive(Serialize, ToSchema)]
struct RetiredWorkspaceSummary {
    count: i64,
    oldest_retired_at_ms: Option<i64>,
}

#[utoipa::path(get,path="/api/today/supervisor",tag="tracks",
    responses(
        (status=200,body=SupervisorSnapshot,description="Current deployment-owner observations"),
        (status=401,body=ErrorBody,description="Authentication required"),
        (status=403,body=ErrorBody,description="Deployment owner required"),
    ))]
pub(crate) async fn supervisor_snapshot(
    principal: Principal,
    State(state): State<RouteState>,
) -> Result<Json<SupervisorSnapshot>> {
    if !principal.is_deployment_owner() {
        return Err(CalmError::Forbidden("Deployment owner required".into()));
    }
    let version = super::version::current_version_info(
        (*state.db_instance_id).clone(),
        (*state.database_id).clone(),
    );
    let mut snapshot = SupervisorSnapshot {
        observed_at_ms: Utc::now().timestamp_millis(),
        kernel_version: version.kernel_version,
        build_sha: version.build_sha,
        database_probe_ms: None,
        scheduler_boot_sweep_completed: state.dispatcher.scheduler().boot_sweep_completed(),
        retired_workspaces: None,
    };
    if let Some(db) = state.mcp_context.sqlite_db.as_ref() {
        let start = Instant::now();
        let probe = tokio::time::timeout(
            Duration::from_secs(1),
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM database_identity")
                .fetch_one(db.read_pool()),
        )
        .await;
        if matches!(probe, Ok(Ok(1))) {
            snapshot.database_probe_ms = Some(start.elapsed().as_secs_f64() * 1000.0);
            snapshot.retired_workspaces = tokio::time::timeout(Duration::from_secs(1), async {
                let mut conn = db.read_pool().acquire().await?;
                calm_truth::db::sqlite::workspace_retirement::retirement_summary(&mut conn).await
            })
            .await
            .ok()
            .and_then(std::result::Result::ok)
            .map(|(count, oldest_retired_at_ms)| RetiredWorkspaceSummary {
                count,
                oldest_retired_at_ms,
            });
        }
    }
    Ok(Json(snapshot))
}
