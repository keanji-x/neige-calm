//! `GET /api/tracks/{id}/report/series/{block_id}`: the browser's read of one
//! `chart.series` block's resolved data, returned verbatim from `report_series::hydrate`.
//! A read never calls a plugin and never writes; a missing or stale row is an in-memory
//! `enqueue`. Every read is one autocommit statement on the pool — no transaction.

use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::QueryRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};

use crate::auth::Principal;
use crate::error::{CalmError, ErrorBody, Result};
use crate::report_series::Detail;
use crate::report_series::hydrate::hydrate_chart_series;
use crate::state::{AppState, RouteState};
use crate::track_report::report_blocks_snapshot;
use calm_types::report_blocks::KIND_CHART_SERIES;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/tracks/{id}/report/series/{block_id}",
        get(get_report_series),
    )
}

/// `?detail=`: `full` (the default) attaches every series' `points`; `summary` answers
/// with the stored summary only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReportSeriesDetail {
    #[default]
    Full,
    Summary,
}

impl From<ReportSeriesDetail> for Detail {
    fn from(detail: ReportSeriesDetail) -> Self {
        match detail {
            ReportSeriesDetail::Full => Detail::Full,
            ReportSeriesDetail::Summary => Detail::Summary,
        }
    }
}

/// `parameter_in` is spelled out because the handler takes the extractor as
/// `Result<Query<_>, QueryRejection>`, which utoipa's axum inference does not see through.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ReportSeriesQuery {
    /// The block `rev` the caller rendered. Required: a request that names
    /// no rev cannot be told apart from one made against a stale document.
    pub rev: u64,
    /// Defaults to `full`.
    #[serde(default)]
    #[param(required = false)]
    pub detail: ReportSeriesDetail,
}

/// The 409 body: the block moved on since the caller read the report.
#[derive(Debug, Serialize, ToSchema)]
pub struct ReportSeriesRevConflict {
    pub current_rev: u64,
}

/// One series inside `ReportSeriesResolved.series` — the stored summary
/// (`summary::SeriesSummary`, verbatim) plus `points` on a `full` read.
#[derive(Debug, Serialize, ToSchema)]
pub struct ReportSeriesEntry {
    pub asset: String,
    /// `ok`, or the plugin's verdict for this asset (`unknown_asset`,
    /// `unavailable`), in which case only `reason` accompanies it.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// The latest daily bar the source had published when the row was
    /// resolved (`YYYY-MM-DD`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub complete_through: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<usize>,
    /// `[YYYY-MM-DD, value]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first: Option<Value>,
    /// `[YYYY-MM-DD, value]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last: Option<Value>,
    /// `(last - first) / first * 100`, two decimals; `null` when `first`
    /// is zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_pct: Option<Option<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub high: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `full` only: `[[ts_ms, value], …]`, or `[[ts_ms, open, high, low,
    /// close, volume], …]` for `view: candles`. `ts_ms` is UTC midnight.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub points: Option<Vec<Vec<f64>>>,
}

/// The flattened `resolved` object — the same shape `calm.report.read`
/// attaches to a `chart.series` block, with the block's presentation
/// fields (`view` / `field` / `period` / `range`) alongside.
#[derive(Debug, Serialize, ToSchema)]
pub struct ReportSeriesResolved {
    /// `pending` (no row yet — the read just queued one), `unavailable`
    /// (the last resolution failed; see `reason`) or `ok`.
    pub status: String,
    /// `unavailable`: why. `pending`: present when the read could not even
    /// queue a job (plugin not installed / not running / tool not exposed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `ok` / `unavailable`: when the row was written, RFC 3339 UTC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
    /// `ok`: the cutoff the data runs through (`YYYY-MM-DD`) — the payload's
    /// `as_of` for a frozen block, yesterday UTC at resolution for a live one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
    /// `ok`: a frozen block whose source has published past `as_of`; the row
    /// is immutable from then on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    pub view: String,
    pub field: String,
    pub period: String,
    pub range: String,
    /// `ok`: one entry per requested asset, in request order. Absent on the
    /// other two statuses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series: Option<Vec<ReportSeriesEntry>>,
}

#[utoipa::path(
    get,
    path = "/api/tracks/{id}/report/series/{block_id}",
    tag = "tracks",
    params(
        ("id" = String, Path, description = "Track id"),
        ("block_id" = String, Path, description = "The `chart.series` block"),
        ReportSeriesQuery,
    ),
    responses(
        (status = 200, description = "The block's resolved data, as stored", body = ReportSeriesResolved),
        (status = 400, description = "`rev` missing or malformed", body = ErrorBody),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 404, description = "Track, block, or a block that is not `chart.series`", body = ErrorBody),
        (status = 409, description = "`rev` is not the block's current rev", body = ReportSeriesRevConflict),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_report_series(
    State(state): State<RouteState>,
    _principal: Principal,
    Path((id, block_id)): Path<(String, String)>,
    query: std::result::Result<Query<ReportSeriesQuery>, QueryRejection>,
) -> Result<Response> {
    let Query(query) = query.map_err(|rejection| CalmError::BadRequest(rejection.body_text()))?;
    state
        .repo
        .track_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {id}")))?;
    // A server without a sqlite pool cannot boot, so this is an invariant, not a mode.
    let pool = state.mcp_context.series_resolver.pool().ok_or_else(|| {
        CalmError::Internal("report_series: route requires a sqlite-backed repo".into())
    })?;
    let (_, blocks) = report_blocks_snapshot(pool, &id).await?;
    let block = blocks
        .iter()
        .find(|block| block.id == block_id && block.kind == KIND_CHART_SERIES)
        .ok_or_else(|| {
            CalmError::NotFound(format!(
                "track {id} has no {KIND_CHART_SERIES} block {block_id}"
            ))
        })?;
    let current_rev = u64::from(block.rev);
    if query.rev != current_rev {
        return Ok((
            StatusCode::CONFLICT,
            Json(ReportSeriesRevConflict { current_rev }),
        )
            .into_response());
    }
    let resolved = hydrate_chart_series(
        &state.mcp_context,
        id.as_str(),
        block,
        query.detail.into(),
        None,
    )
    .await;
    Ok((StatusCode::OK, Json(resolved)).into_response())
}
