//! Human block-level track-report writes.

use crate::actor::Actor;
use crate::auth::Principal;
use crate::error::{CalmError, ErrorBody, Result};
use crate::state::{AppState, RouteState};
use crate::track_report::{self, ReportDocOp, ReportEditTarget};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{patch, post},
};
use calm_types::report_blocks;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/tracks/{id}/report/blocks", post(create_block))
        .route(
            "/api/tracks/{id}/report/blocks/{block_id}",
            patch(update_block).delete(delete_block),
        )
        .route(
            "/api/tracks/{id}/report/blocks/{block_id}/move",
            post(move_block),
        )
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateReportBlockBody {
    pub kind: String,
    pub markdown: Option<String>,
    pub payload: Option<Value>,
    pub if_doc_rev: u64,
    pub position: Option<usize>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateReportBlockBody {
    pub kind: String,
    pub markdown: Option<String>,
    pub payload: Option<Value>,
    pub if_block_rev: u32,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteReportBlockBody {
    pub if_block_rev: u32,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveReportBlockBody {
    pub to_index: usize,
    pub if_doc_rev: u64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportBlockWriteResponse {
    pub id: Option<String>,
    pub rev: Option<u32>,
    pub doc_rev: u64,
    pub updated_at: i64,
}

/// The judgement: REST writes are the human's channel.
///
/// One implementation, many sentences. `subject` names the write the caller
/// attempted and `redirect` tells the refused caller where its own channel is —
/// those differ per endpoint, the rule does not. Restating the rule per
/// endpoint is how the copies drift apart, so callers pass wording and never a
/// second `actor.as_str() == "user"`. A 403 that names the wrong subsystem is a
/// false statement in the audit log, which is the other reason the wording is
/// a parameter and not a second copy of this function.
///
/// # The criterion is `as_str()`, deliberately, and NOT `to_actor_id()`
///
/// `Actor::to_actor_id` maps `ai:codex` to `ActorId::AiCodex` and then, by a
/// defensive default, folds every OTHER `ai:<id>` the middleware admits —
/// `ai:claude` included — down to `ActorId::User`. A guard written on the id
/// would therefore admit exactly the agents it was written to exclude, and
/// would look correct doing it. Pinned by
/// `every_ai_actor_is_refused_including_the_ones_that_map_to_user`, which
/// walks `ai:claude` explicitly for that reason.
///
/// # Argument order is not type-checked
///
/// Two of the three parameters are `&str`, so a call that swaps `subject` and
/// `redirect` COMPILES and produces a 403 naming the wrong subsystem — the
/// false audit statement above, arriving silently. #1515 and #1505 PR2 each
/// grew this helper independently with the two orders reversed, and the merge
/// of the two is where that was nearly shipped. Read a new call site back
/// against these names rather than trusting the build.
pub(crate) fn require_rest_user_actor_for(
    actor: &Actor,
    subject: &str,
    redirect: &str,
) -> Result<()> {
    if actor.as_str() == "user" {
        return Ok(());
    }
    Err(CalmError::Forbidden(format!(
        "{subject}: only `X-Calm-Actor: user` is allowed; got `{}`. {redirect}",
        actor.as_str()
    )))
}

/// Track-report REST writes. See [`require_rest_user_actor_for`].
pub(crate) fn require_rest_user_actor(actor: &Actor) -> Result<()> {
    require_rest_user_actor_for(
        actor,
        "track-report edit",
        "MCP write paths use `calm.report.*` tools.",
    )
}

fn block_content(kind: &str, markdown: Option<String>, payload: Option<Value>) -> Result<String> {
    if kind == report_blocks::KIND_PROSE {
        if payload.is_some() {
            return Err(CalmError::BadRequest(
                "kind=prose accepts `markdown`, not `payload`".into(),
            ));
        }
        let markdown = markdown
            .ok_or_else(|| CalmError::BadRequest("kind=prose requires `markdown`".into()))?;
        report_blocks::check_prose_markdown(&markdown).map_err(CalmError::BadRequest)?;
        return Ok(markdown);
    }
    if markdown.is_some() {
        return Err(CalmError::BadRequest(format!(
            "`markdown` is only valid for kind=prose; pass {kind} data in `payload`"
        )));
    }
    let payload =
        payload.ok_or_else(|| CalmError::BadRequest(format!("kind={kind} requires `payload`")))?;
    report_blocks::render_data_block(kind, &payload).map_err(CalmError::BadRequest)
}

async fn commit(
    state: &RouteState,
    actor: &Actor,
    track_id: &str,
    op: ReportDocOp,
) -> Result<ReportBlockWriteResponse> {
    require_rest_user_actor(actor)?;
    let target = ReportEditTarget::resolve(state.repo.as_ref(), track_id).await?;
    // #1318 §1 — the writer is private to its module; this REST leg
    // reaches it only through the entry point below.
    // `ActorId::User` / `EditAuthor::User` used to be arguments here with a
    // comment saying they always would be; now they are not expressible at
    // this call site at all.
    let (card, block) = track_report::write::rest_user_block_op(
        state.repo.as_ref(),
        &state.events,
        &state.write,
        target,
        op,
    )
    .await?;
    let payload: calm_types::track_report::TrackReportPayload =
        serde_json::from_value(card.payload.clone()).map_err(|error| {
            CalmError::Internal(format!("track-report block response payload: {error}"))
        })?;
    Ok(ReportBlockWriteResponse {
        id: block.as_ref().map(|block| block.id.clone()),
        rev: block.map(|block| block.rev),
        doc_rev: payload.doc_rev,
        updated_at: card.updated_at,
    })
}

#[utoipa::path(post, path = "/api/tracks/{id}/report/blocks", tag = "tracks",
    params(("id" = String, Path)), request_body = CreateReportBlockBody,
    responses((status = 200, body = ReportBlockWriteResponse), (status = 400, body = ErrorBody),
        (status = 401, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 500, body = ErrorBody)))]
pub async fn create_block(
    State(state): State<RouteState>,
    _principal: Principal,
    actor: Actor,
    Path(id): Path<String>,
    Json(body): Json<CreateReportBlockBody>,
) -> Result<Json<ReportBlockWriteResponse>> {
    let content = block_content(&body.kind, body.markdown, body.payload)?;
    commit(
        &state,
        &actor,
        &id,
        ReportDocOp::UpsertBlock {
            id: None,
            kind: body.kind,
            content,
            if_rev: None,
            if_doc_rev: Some(body.if_doc_rev),
            position: body.position,
        },
    )
    .await
    .map(Json)
}

#[utoipa::path(patch, path = "/api/tracks/{id}/report/blocks/{block_id}", tag = "tracks",
    params(("id" = String, Path), ("block_id" = String, Path)),
    request_body = UpdateReportBlockBody,
    responses((status = 200, body = ReportBlockWriteResponse), (status = 400, body = ErrorBody),
        (status = 401, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 500, body = ErrorBody)))]
pub async fn update_block(
    State(state): State<RouteState>,
    _principal: Principal,
    actor: Actor,
    Path((id, block_id)): Path<(String, String)>,
    Json(body): Json<UpdateReportBlockBody>,
) -> Result<Json<ReportBlockWriteResponse>> {
    let content = block_content(&body.kind, body.markdown, body.payload)?;
    commit(
        &state,
        &actor,
        &id,
        ReportDocOp::UpsertBlock {
            id: Some(block_id),
            kind: body.kind,
            content,
            if_rev: Some(body.if_block_rev),
            if_doc_rev: None,
            position: None,
        },
    )
    .await
    .map(Json)
}

#[utoipa::path(delete, path = "/api/tracks/{id}/report/blocks/{block_id}", tag = "tracks",
    params(("id" = String, Path), ("block_id" = String, Path)),
    request_body = DeleteReportBlockBody,
    responses((status = 200, body = ReportBlockWriteResponse), (status = 400, body = ErrorBody),
        (status = 401, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 500, body = ErrorBody)))]
pub async fn delete_block(
    State(state): State<RouteState>,
    _principal: Principal,
    actor: Actor,
    Path((id, block_id)): Path<(String, String)>,
    Json(body): Json<DeleteReportBlockBody>,
) -> Result<Json<ReportBlockWriteResponse>> {
    commit(
        &state,
        &actor,
        &id,
        ReportDocOp::DeleteBlock {
            id: block_id,
            if_rev: body.if_block_rev,
        },
    )
    .await
    .map(Json)
}

#[utoipa::path(post, path = "/api/tracks/{id}/report/blocks/{block_id}/move", tag = "tracks",
    params(("id" = String, Path), ("block_id" = String, Path)),
    request_body = MoveReportBlockBody,
    responses((status = 200, body = ReportBlockWriteResponse), (status = 400, body = ErrorBody),
        (status = 401, body = ErrorBody), (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
        (status = 500, body = ErrorBody)))]
pub async fn move_block(
    State(state): State<RouteState>,
    _principal: Principal,
    actor: Actor,
    Path((id, block_id)): Path<(String, String)>,
    Json(body): Json<MoveReportBlockBody>,
) -> Result<Json<ReportBlockWriteResponse>> {
    commit(
        &state,
        &actor,
        &id,
        ReportDocOp::MoveBlock {
            id: block_id,
            to_index: body.to_index,
            if_doc_rev: body.if_doc_rev,
        },
    )
    .await
    .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1505 PR2 — two write ports, one criterion, two sentences.
    ///
    /// The 403 body lands in the audit log, so a planner-input refusal that
    /// tells the reader to use `calm.report.*` tools is a false statement about
    /// what the caller should have done. This is the reason the subject is a
    /// parameter and not a second copy of the check.
    #[test]
    fn each_write_port_names_itself_in_its_403() {
        let agent = Actor("ai:codex".into());

        let report = require_rest_user_actor(&agent)
            .expect_err("an agent is refused")
            .to_string();
        assert!(report.contains("track-report edit"), "{report}");
        assert!(report.contains("calm.report.*"), "{report}");
        assert!(!report.contains("planner input"), "{report}");

        let planner = require_rest_user_actor_for(
            &agent,
            "planner input edit",
            "A queued message is the person's own un-sent intent.",
        )
        .expect_err("an agent is refused")
        .to_string();
        assert!(planner.contains("planner input edit"), "{planner}");
        assert!(!planner.contains("track-report"), "{planner}");
        assert!(!planner.contains("calm.report.*"), "{planner}");
    }

    /// The criterion itself: `ai:claude` is admitted by the middleware and
    /// collapses to `ActorId::User` under `Actor::to_actor_id`, so a guard
    /// written on the id would admit it. This one does not.
    #[test]
    fn every_ai_actor_is_refused_including_the_ones_that_map_to_user() {
        for header in ["ai:codex", "ai:claude", "ai:planner", "ai:anything"] {
            assert!(
                require_rest_user_actor(&Actor(header.into())).is_err(),
                "{header} must not reach a human-only REST write port"
            );
        }
        assert!(require_rest_user_actor(&Actor("user".into())).is_ok());
    }
}
