//! The Report resource owner: every HTTP audience uses these same reads and fences.
use crate::report_series::hydrate::hydrate_chart_series;
use crate::report_sources::{
    Detail, SourceRow, TrackSourceDetail, TrackSourceList, TrackSourceSummary, captured_at_text,
    store,
};
use crate::routes::{
    track_report_series::{ReportSeriesQuery, ReportSeriesRevConflict},
    tracks::TrackReportReadResponse,
};
use crate::track_report::{report_blocks_snapshot, resolve_report_for_track};
use crate::track_report_read::{
    flat_blocks_text, listed_blocks, load_report_doc_snapshot, load_report_read_snapshot,
};
use crate::{
    error::{CalmError, Result},
    extract::Json,
    read_access::Audience,
    state::RouteState,
};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use calm_types::report_blocks::KIND_CHART_SERIES;

pub(crate) async fn get_track_report(
    s: &RouteState,
    audience: Audience<'_>,
    id: String,
) -> Result<Response> {
    let fence = audience.check(s.repo.as_ref(), &id).await?;
    let (_, report_card, _) = resolve_report_for_track(s.repo.as_ref(), &id).await?;
    let response = match audience {
        Audience::Owner(_) => {
            let snapshot =
                load_report_read_snapshot(s.repo.as_ref(), report_card.id.as_str()).await?;
            TrackReportReadResponse {
                schema_version: snapshot.schema_version,
                doc_rev: snapshot.doc_rev,
                summary: snapshot.summary,
                body: snapshot.body,
                blocks: snapshot.blocks,
                task_diagnostics: snapshot.task_diagnostics,
            }
        }
        Audience::ShareLink { .. } => {
            let doc = load_report_doc_snapshot(s.repo.as_ref(), report_card.id.as_str()).await?;
            let blocks: Vec<_> = listed_blocks(&doc.blocks).cloned().collect();
            TrackReportReadResponse {
                schema_version: crate::track_report::TrackReportPayload::SCHEMA_VERSION,
                doc_rev: doc.doc_rev,
                summary: doc.summary,
                body: flat_blocks_text(&blocks),
                blocks,
                task_diagnostics: vec![],
            }
        }
    };
    audience.publish(s.repo.as_ref(), &id, &fence).await?;
    Ok(Json(response).into_response())
}

pub(crate) async fn get_report_series(
    state: &RouteState,
    audience: Audience<'_>,
    id: String,
    block_id: String,
    query: ReportSeriesQuery,
) -> Result<Response> {
    let fence = audience.check(state.repo.as_ref(), &id).await?;
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
        audience.publish(state.repo.as_ref(), &id, &fence).await?;
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
        crate::report_series::rest_resolved_at,
    )
    .await;
    audience.publish(state.repo.as_ref(), &id, &fence).await?;
    Ok((StatusCode::OK, Json(resolved)).into_response())
}

async fn source_pool(state: &RouteState) -> Result<sqlx::SqlitePool> {
    state
        .mcp_context
        .sqlite_db
        .as_ref()
        .map(|db| db.read_pool().clone())
        .ok_or_else(|| {
            CalmError::Internal("report_sources: route requires a sqlite-backed repo".into())
        })
}

pub(crate) async fn list_track_sources(
    state: &RouteState,
    audience: Audience<'_>,
    id: String,
) -> Result<Response> {
    let fence = audience.check(state.repo.as_ref(), &id).await?;
    let pool = source_pool(state).await?;
    let rows = store::list(&pool, &id).await?;
    let sources = rows.iter().map(summary_of).collect();
    audience.publish(state.repo.as_ref(), &id, &fence).await?;
    Ok((StatusCode::OK, Json(TrackSourceList { sources })).into_response())
}
pub(crate) async fn get_track_source(
    state: &RouteState,
    audience: Audience<'_>,
    id: String,
    source_id: String,
) -> Result<Response> {
    let fence = audience.check(state.repo.as_ref(), &id).await?;
    let pool = source_pool(state).await?;
    let row = store::get(&pool, &id, &source_id, Detail::Full)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {id} has no source {source_id}")))?;
    let detail =
        TrackSourceDetail::from_summary(summary_of(&row), row.body.clone().unwrap_or_default());
    audience.publish(state.repo.as_ref(), &id, &fence).await?;
    Ok((StatusCode::OK, Json(detail)).into_response())
}

fn summary_of(row: &SourceRow) -> TrackSourceSummary {
    TrackSourceSummary {
        source_id: row.source_id.clone(),
        provenance: row.provenance,
        origin: row.origin.clone(),
        title: row.title.clone(),
        published_at: row.published_at.clone(),
        content_id: row.origin.content_id().map(str::to_string),
        url: row.origin.url().map(str::to_string),
        body_bytes: row.body_bytes,
        body_sha256: row.body_sha256.clone(),
        captured_at: captured_at_text(row.captured_at),
        quotes: row.quotes.clone(),
    }
}
