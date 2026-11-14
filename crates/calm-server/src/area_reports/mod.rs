//! Report search owns tag/glob filtering, update ordering and its 500-match limit.
//! Navigation and canonical names belong to report_paths.

pub mod glob;
mod store;

#[cfg(test)]
mod tests;

use calm_types::report_blocks::tasks::normalize_legacy_task_blocks;
use serde::Serialize;
use sqlx::SqlitePool;

use crate::track_fs_view::TrackFsError;
use crate::track_report::{ReportBlock, TrackReportPayload};
use crate::track_report_read::legacy_row_blocks;

/// Most matches one `find` returns; more is refused, never truncated.
pub const MAX_REPORTS_PER_LISTING: usize = 500;

/// One match in the search result `{reports: […]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportEntry {
    /// Canonical `/AreaName/TrackName/report.md`, readable with `neige cat`.
    pub path: String,
    pub title: String,
    pub track_id: String,
    /// In insertion order.
    pub tags: Vec<String>,
    /// The report card's update time, RFC 3339 in the server's local offset.
    pub updated_at: String,
}

/// `find` predicates; both set means AND. The default matches every report.
#[derive(Debug, Default)]
pub struct Filter {
    /// Glob over the canonical Track segment; see [`glob`].
    pub name: Option<String>,
    /// One tag, matched exactly.
    pub tag: Option<String>,
}

struct Named {
    row: store::Row,
    file: String,
}

/// One report of the area with its block projection: the `@` mention candidates (#1881).
#[derive(Debug)]
pub struct ReportOutline {
    /// Canonical `/AreaName/TrackName/report.md`, readable with `neige cat`.
    pub path: String,
    pub title: String,
    pub track_id: String,
    /// In insertion order.
    pub tags: Vec<String>,
    /// The report card's update time (ms).
    pub updated_at: i64,
    /// In document order, with the ids `neige cat --blocks` accepts; see [`outlines`].
    pub blocks: Vec<ReportBlock>,
}

/// Every discoverable report of `area_id` with its path, tags and blocks, ordered by track id.
/// Names use complete identity sets; report payloads and tags come from one
/// statement. Blocks carry the ids `neige cat --blocks` accepts, without loading any report CRDT (#1859):
/// a row with no CRDT derives them from its body exactly as `report_doc_snapshot` does; a CRDT row
/// returns its stored projection, normalized as that function's cached branch does. A CRDT row
/// without a stored projection (the writer always stores one) lists with no blocks.
pub async fn outlines(
    ctx: &crate::mcp_server::registry::AppContext,
    pool: &SqlitePool,
    area_id: &str,
) -> Result<Vec<ReportOutline>, TrackFsError> {
    // Name the complete user Area and sibling Track sets before selecting report rows.
    // System Areas have no discoverable name path, so offer no references (including tags).
    let Some((_, area_name)) = crate::report_paths::areas(ctx)
        .await
        .map_err(path_error)?
        .into_iter()
        .find(|(a, _)| a.id.as_str() == area_id)
    else {
        return Ok(Vec::new());
    };
    let paths: std::collections::HashMap<_, _> = crate::report_paths::tracks(ctx, area_id)
        .await
        .map_err(path_error)?
        .into_iter()
        .filter(|(track, _)| crate::track_access::user_visible_track(track))
        .map(|(track, name)| {
            (
                track.id.to_string(),
                format!("/{area_name}/{name}/report.md"),
            )
        })
        .collect();
    let rows = store::rows_with_projection(pool, area_id)
        .await
        .map_err(internal)?;
    rows.into_iter()
        .filter_map(|(row, projection)| {
            paths.get(&row.track_id).map(|path| (row, projection, path))
        })
        .map(|(row, projection, path)| {
            let payload: TrackReportPayload =
                serde_json::from_str(&projection.payload).map_err(|e| {
                    TrackFsError::Internal(format!(
                        "track_report: malformed payload on card {}: {e}",
                        row.card_id
                    ))
                })?;
            let blocks = match (projection.has_crdt, payload.blocks) {
                (false, _) => legacy_row_blocks(&payload.body),
                (true, Some(blocks)) => normalize_legacy_task_blocks(&blocks),
                (true, None) => Vec::new(),
            };
            Ok(ReportOutline {
                path: path.clone(),
                title: row.title,
                track_id: row.track_id,
                tags: row.tags,
                updated_at: row.updated_at,
                blocks,
            })
        })
        .collect()
}

/// Find in the current user Area; name all Tracks before report/tag filtering.
pub async fn find(
    ctx: &crate::mcp_server::registry::AppContext,
    pool: &SqlitePool,
    area_id: &str,
    filter: &Filter,
) -> Result<Vec<ReportEntry>, TrackFsError> {
    let root = crate::report_paths::area_path(ctx, area_id)
        .await
        .map_err(path_error)?;
    let tracks = crate::report_paths::tracks(ctx, area_id)
        .await
        .map_err(path_error)?;
    let names: std::collections::HashMap<_, _> = tracks
        .into_iter()
        .filter(|(t, _)| crate::track_access::user_visible_track(t))
        .map(|(t, name)| (t.id.to_string(), name))
        .collect();
    let reports = store::rows(pool, area_id)
        .await
        .map_err(internal)?
        .into_iter()
        .filter_map(|row| {
            names.get(&row.track_id).map(|file| Named {
                file: file.clone(),
                row,
            })
        })
        .collect();
    let mut result = list_named(reports, filter)?;
    for r in &mut result {
        r.path = format!("{root}{}/report.md", r.path);
    }
    Ok(result)
}

fn list_named(reports: Vec<Named>, filter: &Filter) -> Result<Vec<ReportEntry>, TrackFsError> {
    let mut matched: Vec<Named> = reports
        .into_iter()
        .filter(|report| {
            filter
                .name
                .as_deref()
                .is_none_or(|p| glob::matches(p, &report.file))
                && filter
                    .tag
                    .as_deref()
                    .is_none_or(|tag| report.row.tags.iter().any(|t| t == tag))
        })
        .collect();
    if matched.len() > MAX_REPORTS_PER_LISTING {
        let count = matched.len();
        return Err(TrackFsError::PathNotAvailable(format!(
            "{count} reports match, more than the {MAX_REPORTS_PER_LISTING} one search \
             returns; narrow the search with a name glob or tag"
        )));
    }

    matched.sort_by(|a, b| {
        b.row
            .updated_at
            .cmp(&a.row.updated_at)
            .then_with(|| a.file.cmp(&b.file))
    });
    matched
        .into_iter()
        .map(|report| {
            Ok(ReportEntry {
                path: report.file,
                updated_at: rfc3339_local(report.row.updated_at)?,
                title: report.row.title,
                track_id: report.row.track_id,
                tags: report.row.tags,
            })
        })
        .collect()
}

fn path_error(error: crate::mcp_server::framing::RpcError) -> TrackFsError {
    use crate::mcp_server::framing::RpcError;
    match error.code {
        RpcError::INVALID_PARAMS | RpcError::NOT_FOUND => {
            TrackFsError::PathNotAvailable(error.message)
        }
        RpcError::FORBIDDEN => TrackFsError::Forbidden(error.message),
        _ => TrackFsError::Internal(error.message),
    }
}

fn rfc3339_local(ms: i64) -> Result<String, TrackFsError> {
    crate::time_format::rfc3339_local_ms(ms)
        .ok_or_else(|| TrackFsError::Internal(format!("report updated_at {ms} is out of range")))
}

fn internal(error: impl std::fmt::Display) -> TrackFsError {
    TrackFsError::Internal(format!("area reports: {error}"))
}
