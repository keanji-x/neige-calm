//! Drain-side admission: re-read the block as it is NOW, derive the request and window from that payload, and refuse the job when the block is gone, re-kinded or re-pointed, or the row for the current hash is fresh or pinned.
//! Two autocommit statements, no transaction: a deferred read tx is one party of the deadlock ring against an IMMEDIATE writer, and atomicity between the two reads is not needed.

use sqlx::SqlitePool;

use calm_types::report_blocks::KIND_CHART_SERIES;

use super::request::{SeriesRequest, Window};
use super::store::{Detail, select_row};
use super::summary::row_is_fresh;

/// `Ok(Err(reason))` is a drop.
pub(super) async fn admit(
    pool: &SqlitePool,
    track_id: &str,
    block_id: &str,
    lane_plugin: &str,
    now: i64,
) -> Result<Result<(SeriesRequest, Window), String>, crate::error::CalmError> {
    let (_, blocks) = crate::track_report::report_blocks_snapshot(pool, track_id).await?;
    let Some(block) = blocks.iter().find(|block| block.id == block_id) else {
        return Ok(Err("block no longer exists".to_string()));
    };
    if block.kind != KIND_CHART_SERIES {
        return Ok(Err("block is no longer a chart.series".to_string()));
    }
    let request = match SeriesRequest::from_payload(&block.payload) {
        Ok(request) => request,
        Err(error) => {
            tracing::warn!(
                track_id,
                block_id,
                error,
                "report_series: payload does not derive"
            );
            return Ok(Err(format!("payload does not derive: {error}")));
        }
    };
    if request.plugin_id != lane_plugin {
        return Ok(Err("block moved to another plugin".to_string()));
    }
    let Some(window) = request.window(now) else {
        tracing::warn!(track_id, block_id, "report_series: cutoff window undefined");
        return Ok(Err("cutoff window undefined".to_string()));
    };
    let row = select_row(
        pool,
        track_id,
        block_id,
        &request.request_hash,
        Detail::Summary,
    )
    .await?;
    if let Some(row) = row
        && row_is_fresh(
            &row.status,
            row.summary.as_ref(),
            row.pinned,
            row.resolved_at,
            now,
        )
    {
        return Ok(Err("row is fresh or pinned".to_string()));
    }
    Ok(Ok((request, window)))
}
