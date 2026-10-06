//! `area/reports/` (#1838 S2): the Planner's read-only view of the track reports of its own area,
//! served by `neige track ls`, `neige report find` and `neige track cat`. This module lists, filters, resolves and reads
//! for all three. The area is always the caller's own; a path resolves only against that area's
//! listing, so no name, ID suffix, duplicate title, rename or traversal reaches another area. A read
//! returns a report's body or its blocks (#1874), tags and the report card's `updated_at` — never
//! another track's card payload, runs or workspace. The name codec lives in [`name`]. [`outlines`]
//! serves the same listing, with blocks, to the chat `@` mention search (#1881).

pub mod glob;
pub mod name;
mod store;

#[cfg(test)]
mod tests;

use calm_types::report_blocks::tasks::normalize_legacy_terminal_task_blocks;
use chrono::{Local, SecondsFormat, TimeZone};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::track_fs_view::{TrackFsContent, TrackFsError, report_markdown};
use crate::track_report::{ReportBlock, TrackReportPayload};
use crate::track_report_read::{legacy_row_blocks, report_doc_snapshot};

/// The directory every report path starts with.
pub const REPORTS_DIR: &str = "area/reports";
/// Most reports one `ls` or `find` returns; more is refused, never truncated.
pub const MAX_REPORTS_PER_LISTING: usize = 500;

/// A normalized view path under `area/`.
#[derive(Debug, PartialEq, Eq)]
pub enum AreaPath<'a> {
    /// `area`: holds `reports/`.
    Root,
    /// `area/reports`.
    Reports,
    /// `area/reports/<file>`; `file` holds no `/`.
    Report(&'a str),
}

/// `None` when `path` (already `normalize_path`ed) is not under `area/`, so the track view serves it;
/// `Some(Err)` for a path under `area/` this view does not have, traversal included.
pub fn classify(path: &str) -> Option<Result<AreaPath<'_>, String>> {
    if path == "area" {
        return Some(Ok(AreaPath::Root));
    }
    let rest = path.strip_prefix("area/")?;
    Some(if rest == "reports" {
        Ok(AreaPath::Reports)
    } else if let Some(file) = rest.strip_prefix("reports/")
        && !file.contains('/')
        && !file.is_empty()
    {
        Ok(AreaPath::Report(file))
    } else {
        Err(format!(
            "neige.track: path not available in this view: {path} (under `area/` there is only \
             `area/reports/<name>.md`)"
        ))
    })
}

/// One listed report: a row of `{reports: […]}`, the `--json` shape of `ls`/`find` on `area/reports/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportEntry {
    /// `area/reports/<name>.md`, readable with `neige track cat`.
    pub path: String,
    pub title: String,
    pub track_id: String,
    /// In insertion order.
    pub tags: Vec<String>,
    /// The report card's update time, RFC 3339 in the server's local offset.
    pub updated_at: String,
}

/// `find` predicates; both set means AND. The default matches every report (plain `ls`).
#[derive(Debug, Default)]
pub struct Filter {
    /// Glob over the listed file name; see [`glob`].
    pub name: Option<String>,
    /// One tag, matched exactly.
    pub tag: Option<String>,
}

impl Filter {
    fn is_search(&self) -> bool {
        self.name.is_some() || self.tag.is_some()
    }
}

struct Named {
    row: store::Row,
    file: String,
}

/// The listed file name of each of `rows`, which must be every report of the area.
fn file_names(rows: &[&store::Row]) -> Vec<String> {
    let pairs: Vec<(&str, &str)> = rows
        .iter()
        .map(|row| (row.title.as_str(), row.track_id.as_str()))
        .collect();
    name::file_names(&pairs)
}

async fn named(pool: &SqlitePool, area_id: &str) -> Result<Vec<Named>, TrackFsError> {
    let rows = store::rows(pool, area_id).await.map_err(internal)?;
    let files = file_names(&rows.iter().collect::<Vec<_>>());
    Ok(rows
        .into_iter()
        .zip(files)
        .map(|(row, file)| Named { row, file })
        .collect())
}

/// One report of the area with its block projection: the `@` mention candidates (#1881).
#[derive(Debug)]
pub struct ReportOutline {
    /// `area/reports/<name>.md`, exactly as `ls` lists it.
    pub path: String,
    pub title: String,
    pub track_id: String,
    /// In insertion order.
    pub tags: Vec<String>,
    /// The report card's update time (ms).
    pub updated_at: i64,
    /// In document order, with the ids [`read_blocks`] accepts; see [`outlines`].
    pub blocks: Vec<ReportBlock>,
}

/// Every report of `area_id` with its path, tags and blocks, ordered by track id, from one
/// statement. Blocks carry the ids [`read_blocks`] accepts, without loading any report CRDT (#1859):
/// a row with no CRDT derives them from its body exactly as `report_doc_snapshot` does; a CRDT row
/// returns its stored projection, normalized as that function's cached branch does. A CRDT row
/// without a stored projection (the writer always stores one) lists with no blocks.
pub async fn outlines(
    pool: &SqlitePool,
    area_id: &str,
) -> Result<Vec<ReportOutline>, TrackFsError> {
    let rows = store::rows_with_projection(pool, area_id)
        .await
        .map_err(internal)?;
    let files = file_names(&rows.iter().map(|(row, _)| row).collect::<Vec<_>>());
    rows.into_iter()
        .zip(files)
        .map(|((row, projection), file)| {
            let payload: TrackReportPayload =
                serde_json::from_str(&projection.payload).map_err(|e| {
                    TrackFsError::Internal(format!(
                        "track_report: malformed payload on card {}: {e}",
                        row.card_id
                    ))
                })?;
            let blocks = match (projection.has_crdt, payload.blocks) {
                (false, _) => legacy_row_blocks(&payload.body),
                (true, Some(blocks)) => normalize_legacy_terminal_task_blocks(&blocks),
                (true, None) => Vec::new(),
            };
            Ok(ReportOutline {
                path: format!("{REPORTS_DIR}/{file}"),
                title: row.title,
                track_id: row.track_id,
                tags: row.tags,
                updated_at: row.updated_at,
                blocks,
            })
        })
        .collect()
}

/// The reports of `area_id` that pass `filter`, newest report update first (then by name).
/// Refused when more than [`MAX_REPORTS_PER_LISTING`] match.
pub async fn list(
    pool: &SqlitePool,
    area_id: &str,
    filter: &Filter,
) -> Result<Vec<ReportEntry>, TrackFsError> {
    let mut matched: Vec<Named> = named(pool, area_id)
        .await?
        .into_iter()
        .filter(|report| {
            filter
                .name
                .as_deref()
                .is_none_or(|pattern| glob::matches(pattern, &report.file))
                && filter
                    .tag
                    .as_deref()
                    .is_none_or(|tag| report.row.tags.iter().any(|t| t == tag))
        })
        .collect();
    if matched.len() > MAX_REPORTS_PER_LISTING {
        let count = matched.len();
        return Err(TrackFsError::PathNotAvailable(if filter.is_search() {
            format!(
                "{count} reports match, more than the {MAX_REPORTS_PER_LISTING} one listing \
                 returns; narrow the search with a tighter --name GLOB or a --tag TAG"
            )
        } else {
            format!(
                "{REPORTS_DIR}/ holds {count} reports, more than the {MAX_REPORTS_PER_LISTING} one \
                 listing returns; narrow it with `neige report find {REPORTS_DIR}/ --name GLOB` or `--tag TAG`"
            )
        }));
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
                path: format!("{REPORTS_DIR}/{}", report.file),
                updated_at: rfc3339_local(report.row.updated_at)?,
                title: report.row.title,
                track_id: report.row.track_id,
                tags: report.row.tags,
            })
        })
        .collect()
}

/// The latest body of the report `file` names in `area_id`: resolved against the area's listing,
/// then the body read in one statement that re-checks the track and the area. A rename after the
/// resolution still reads the resolved report's latest body; a report that left the area is not found.
pub async fn read(
    pool: &SqlitePool,
    area_id: &str,
    file: &str,
) -> Result<TrackFsContent, TrackFsError> {
    let report = resolve_and_read(pool, area_id, file).await?;
    report_markdown(&report.card_id, report.payload)
}

/// The block projection of the report `file` names in `area_id` — the blocks `neige_area_ls`
/// indexes and `neige_report_read` selects — resolved and re-checked exactly as [`read`].
pub async fn read_blocks(
    pool: &SqlitePool,
    area_id: &str,
    file: &str,
) -> Result<Vec<ReportBlock>, TrackFsError> {
    let report = resolve_and_read(pool, area_id, file).await?;
    report_doc_snapshot(
        &report.card_id,
        report.updated_at,
        report.payload,
        report.body_crdt.as_deref(),
    )
    .map(|snapshot| snapshot.blocks)
    .map_err(|e| TrackFsError::Internal(e.to_string()))
}

/// One resolved report card, read in one statement.
struct ReadReport {
    card_id: String,
    payload: serde_json::Value,
    body_crdt: Option<Vec<u8>>,
    updated_at: i64,
}

/// Resolves `file` against the area's listing, then reads that report's row in one statement that
/// re-checks the track and the area.
async fn resolve_and_read(
    pool: &SqlitePool,
    area_id: &str,
    file: &str,
) -> Result<ReadReport, TrackFsError> {
    let path = format!("{REPORTS_DIR}/{file}");
    let parsed = name::parse(file).map_err(TrackFsError::PathNotAvailable)?;
    let not_found = || {
        TrackFsError::PathNotAvailable(format!(
            "no report at `{path}` in this area; `neige track ls {REPORTS_DIR}/` lists the current names"
        ))
    };
    let reports = named(pool, area_id).await?;
    let mut candidates: Vec<Named> = reports
        .into_iter()
        .filter(|report| {
            report.row.title == parsed.title
                && parsed
                    .suffix
                    .as_deref()
                    .is_none_or(|suffix| report.row.track_id.starts_with(suffix))
        })
        .collect();
    let report = match candidates.len() {
        0 => return Err(not_found()),
        1 => candidates.remove(0),
        _ => {
            let paths: Vec<String> = candidates
                .iter()
                .map(|report| format!("{REPORTS_DIR}/{}", report.file))
                .collect();
            return Err(TrackFsError::PathNotAvailable(format!(
                "`{path}` names {} reports in this area; read one of: {}",
                paths.len(),
                paths.join(", ")
            )));
        }
    };
    let stored = store::report(pool, area_id, &report.row)
        .await
        .map_err(internal)?
        .ok_or_else(not_found)?;
    let card_id = report.row.card_id;
    let payload = serde_json::from_str(&stored.payload).map_err(|e| {
        TrackFsError::Internal(format!(
            "track_report: malformed payload on card {card_id}: {e}"
        ))
    })?;
    Ok(ReadReport {
        card_id,
        payload,
        body_crdt: stored.body_crdt,
        updated_at: stored.updated_at,
    })
}

fn rfc3339_local(ms: i64) -> Result<String, TrackFsError> {
    rfc3339_local_ms(ms)
        .ok_or_else(|| TrackFsError::Internal(format!("report updated_at {ms} is out of range")))
}

/// A unix-ms time as RFC 3339 with the server's offset, the one spelling of agent-facing times
/// here and in `neige mail ls|cat`; `None` when it is out of range.
pub(crate) fn rfc3339_local_ms(ms: i64) -> Option<String> {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|at| at.to_rfc3339_opts(SecondsFormat::Millis, false))
}

fn internal(error: impl std::fmt::Display) -> TrackFsError {
    TrackFsError::Internal(format!("area reports: {error}"))
}
