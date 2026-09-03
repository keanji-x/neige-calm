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

pub(crate) fn require_rest_user_actor(actor: &Actor) -> Result<()> {
    if actor.as_str() == "user" {
        return Ok(());
    }
    Err(CalmError::Forbidden(format!(
        "track-report edit: only `X-Calm-Actor: user` is allowed via REST; got `{}`. MCP write \
         paths use `calm.report.*` tools.",
        actor.as_str()
    )))
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
