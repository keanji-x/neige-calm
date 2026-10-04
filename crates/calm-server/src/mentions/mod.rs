//! `GET /api/areas/{area_id}/mentions` (#1881): the chat `@` candidates of one area — its report
//! tags, its reports and their blocks, exactly the `area/reports/` view the Planner reads — each with
//! the text a chosen candidate inserts:
//!
//! * tag: ``@`tag:<tag>` `` — the Planner runs `neige report find area/reports/ --tag <tag>`;
//! * report: ``@`area/reports/<name>.md` `` — `neige track cat <path>`;
//! * block: ``@`area/reports/<name>.md#<block_id>` `` — `neige track cat <path> --blocks <block_id>`.
//!
//! Every request reads the area afresh (no cache or index), so a new tag or a rename shows on the
//! next keystroke; matching and ranking run in memory, see [`rank`].

mod rank;

#[cfg(test)]
mod tests;

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use calm_types::mentions::MentionCandidates;
use serde::Deserialize;
use utoipa::IntoParams;

use crate::area_reports;
use crate::error::{CalmError, ErrorBody, Result};
use crate::state::{AppState, RouteState};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/areas/{area_id}/mentions", get(list_mentions))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct MentionsQuery {
    /// The text typed after `@`; empty asks for recommendations.
    pub q: String,
    /// The track the message is written in: its blocks rank first. A track outside the area lifts nothing.
    pub track: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/areas/{area_id}/mentions",
    tag = "areas",
    params(("area_id" = String, Path, description = "Area id"), MentionsQuery),
    responses(
        (status = 200, description = "The area's tags, reports and report blocks matching `q`, best first, at most 8 per group", body = MentionCandidates),
        (status = 400, description = "Missing `q` (plain-text query rejection)"),
        (status = 404, description = "Area not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_mentions(
    State(s): State<RouteState>,
    Path(area_id): Path<String>,
    Query(query): Query<MentionsQuery>,
) -> Result<Json<MentionCandidates>> {
    s.repo
        .area_get(&area_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("area {area_id}")))?;
    let pool = s.mcp_context.sqlite_pool.as_ref().ok_or_else(|| {
        CalmError::Internal("mentions: route requires a sqlite-backed repo".into())
    })?;
    let reports = area_reports::outlines(pool, &area_id).await?;
    Ok(Json(rank::candidates(
        &reports,
        &query.q,
        query.track.as_deref(),
    )))
}

/// `@` followed by `text` as one Markdown code span: report names keep a title's spaces, so a bare
/// path could not be delimited in free text. A fence longer than any backtick run in `text`, padded
/// with a space when `text` starts or ends with a backtick, keeps the span exact (CommonMark).
fn mention_insert(text: &str) -> String {
    let longest_run = text
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest_run + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("@{fence}{pad}{text}{pad}{fence}")
}
