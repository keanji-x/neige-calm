//! The one read-side step both readers share (#1628 D4 / D5): derive the
//! request from the block's current payload, select the row by its full
//! primary key, judge its freshness, enqueue when it is missing or stale,
//! and flatten what was found into the `resolved` object.
//!
//! `calm.report.read` calls this once per `chart.series` block and
//! `GET /api/tracks/{id}/report/series/{block_id}` calls it for the one block
//! it was asked about. D4 promises the route returns *the same bytes* the
//! read returns for the same row; that promise is kept by there being one
//! function, not by two copies agreeing.
//!
//! Nothing here calls a plugin and nothing writes the database: a miss is an
//! in-memory `enqueue`, and the answer is whatever row (or absence of one)
//! was on disk.

use std::sync::Arc;

use serde_json::Value;

use crate::mcp_server::registry::AppContext;
use crate::mcp_server::tool_visibility::TrackPluginScope;
use calm_types::track_report::ReportBlock;

use super::{Detail, Enqueue, Resolved, SeriesRequest, row_is_fresh, store};

/// `resolved` for one `chart.series` block.
///
/// `scope` is the track's plugin scope when the caller already resolved it
/// (a read hydrating several blocks resolves it once, F4.18); `None` lets
/// the resolver look it up itself, and only when a job actually has to be
/// queued — a fresh row costs no scope lookup at all.
pub(crate) async fn hydrate_chart_series(
    ctx: &Arc<AppContext>,
    track_id: &str,
    block: &ReportBlock,
    detail: Detail,
    scope: Option<&TrackPluginScope>,
) -> Value {
    let request = match SeriesRequest::from_payload(&block.payload) {
        Ok(request) => request,
        Err(error) => {
            return Resolved::Pending {
                reason: Some(format!("payload does not derive a request: {error}")),
            }
            .to_json();
        }
    };
    let resolver = &ctx.series_resolver;
    let row = match resolver.pool() {
        Some(pool) => {
            match store::select_row(pool, track_id, &block.id, &request.request_hash, detail).await
            {
                Ok(row) => row,
                Err(error) => {
                    tracing::warn!(
                        track_id,
                        block_id = block.id,
                        error = %error,
                        "report_series: row read failed; reporting pending"
                    );
                    None
                }
            }
        }
        None => None,
    };
    let now = resolver.now_ms();
    let fresh = row.as_ref().is_some_and(|row| {
        row_is_fresh(
            &row.status,
            row.summary.as_ref(),
            row.pinned,
            row.resolved_at,
            now,
        )
    });
    let mut resolved = Resolved::from_row(row);
    if !fresh {
        let outcome = match scope {
            Some(scope) => {
                resolver
                    .enqueue_scoped(ctx, track_id, &block.id, &request, scope)
                    .await
            }
            None => resolver.enqueue(ctx, track_id, &block.id, &request).await,
        };
        if let (Resolved::Pending { reason }, Enqueue::Miss(miss)) = (&mut resolved, outcome) {
            *reason = Some(miss);
        }
    }
    let mut out = resolved.to_json();
    out["view"] = Value::String(request.view.clone());
    out["field"] = Value::String(request.field.clone());
    out["period"] = Value::String(request.period.clone());
    out["range"] = Value::String(request.range.clone());
    out
}
