//! `area/reports/` (#1838 S2): the Planner's read-only view of the track reports of its own area,
//! served by `neige ls`, `neige find` and `neige cat`. This module lists, filters, resolves and reads
//! for all three. The area is always the caller's own; a path resolves only against that area's
//! listing, so no name, ID suffix, duplicate title, rename or traversal reaches another area. A read
//! returns a report's body, tags and the report card's `updated_at` — never another track's card
//! payload, runs or workspace. The name codec lives in [`name`].

pub mod glob;
pub mod name;
mod store;

#[cfg(test)]
mod tests;

use chrono::{Local, SecondsFormat, TimeZone};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::track_fs_view::{TrackFsContent, TrackFsError, report_markdown};

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
            "calm.track: path not available in this view: {path} (under `area/` there is only \
             `area/reports/<name>.md`)"
        ))
    })
}

/// One listed report; also the `--json` shape of `ls`/`find` on `area/reports/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportEntry {
    /// `area/reports/<name>.md`, readable with `neige cat`.
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

async fn named(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    area_id: &str,
) -> Result<Vec<Named>, TrackFsError> {
    let rows = store::rows(tx, area_id).await.map_err(internal)?;
    let pairs: Vec<(&str, &str)> = rows
        .iter()
        .map(|row| (row.title.as_str(), row.track_id.as_str()))
        .collect();
    let files = name::file_names(&pairs);
    Ok(rows
        .into_iter()
        .zip(files)
        .map(|(row, file)| Named { row, file })
        .collect())
}

/// The reports of `area_id` that pass `filter`, newest report update first (then by name).
/// Refused when more than [`MAX_REPORTS_PER_LISTING`] match.
pub async fn list(
    pool: &SqlitePool,
    area_id: &str,
    filter: &Filter,
) -> Result<Vec<ReportEntry>, TrackFsError> {
    let mut tx = pool.begin().await.map_err(internal)?;
    let reports = named(&mut tx, area_id).await?;
    let mut tags = store::tags(&mut tx, area_id).await.map_err(internal)?;
    tx.commit().await.map_err(internal)?;

    let mut matched: Vec<(Named, Vec<String>)> = reports
        .into_iter()
        .map(|report| {
            let report_tags = tags.remove(&report.row.track_id).unwrap_or_default();
            (report, report_tags)
        })
        .filter(|(report, report_tags)| {
            filter
                .name
                .as_deref()
                .is_none_or(|pattern| glob::matches(pattern, &report.file))
                && filter
                    .tag
                    .as_deref()
                    .is_none_or(|tag| report_tags.iter().any(|t| t == tag))
        })
        .collect();
    if matched.len() > MAX_REPORTS_PER_LISTING {
        let count = matched.len();
        return Err(TrackFsError::PathNotAvailable(if filter.is_search() {
            format!(
                "{count} reports match, more than the {MAX_REPORTS_PER_LISTING} one listing \
                 returns; narrow the search with a tighter -name GLOB or a -tag TAG"
            )
        } else {
            format!(
                "{REPORTS_DIR}/ holds {count} reports, more than the {MAX_REPORTS_PER_LISTING} one \
                 listing returns; narrow it with `neige find {REPORTS_DIR}/ -name GLOB` or `-tag TAG`"
            )
        }));
    }
    matched.sort_by(|(a, _), (b, _)| {
        b.row
            .updated_at
            .cmp(&a.row.updated_at)
            .then_with(|| a.file.cmp(&b.file))
    });
    matched
        .into_iter()
        .map(|(report, tags)| {
            Ok(ReportEntry {
                path: format!("{REPORTS_DIR}/{}", report.file),
                updated_at: rfc3339_local(report.row.updated_at)?,
                title: report.row.title,
                track_id: report.row.track_id,
                tags,
            })
        })
        .collect()
}

/// The latest body of the report `file` names in `area_id`, resolved and read in one transaction.
pub async fn read(
    pool: &SqlitePool,
    area_id: &str,
    file: &str,
) -> Result<TrackFsContent, TrackFsError> {
    let path = format!("{REPORTS_DIR}/{file}");
    let parsed = name::parse(file).map_err(TrackFsError::PathNotAvailable)?;
    let mut tx = pool.begin().await.map_err(internal)?;
    let reports = named(&mut tx, area_id).await?;
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
        0 => {
            return Err(TrackFsError::PathNotAvailable(format!(
                "no report at `{path}` in this area; `neige ls {REPORTS_DIR}/` lists the current names"
            )));
        }
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
    let payload = store::payload(&mut tx, area_id, &report.row)
        .await
        .map_err(internal)?
        .ok_or_else(|| TrackFsError::Internal(format!("{path}: report vanished mid-read")))?;
    tx.commit().await.map_err(internal)?;
    let payload = serde_json::from_str(&payload).map_err(|e| {
        TrackFsError::Internal(format!(
            "track_report: malformed payload on card {}: {e}",
            report.row.card_id
        ))
    })?;
    report_markdown(&report.row.card_id, payload)
}

fn rfc3339_local(ms: i64) -> Result<String, TrackFsError> {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|at| at.to_rfc3339_opts(SecondsFormat::Millis, false))
        .ok_or_else(|| TrackFsError::Internal(format!("report updated_at {ms} is out of range")))
}

fn internal(error: impl std::fmt::Display) -> TrackFsError {
    TrackFsError::Internal(format!("area reports: {error}"))
}
