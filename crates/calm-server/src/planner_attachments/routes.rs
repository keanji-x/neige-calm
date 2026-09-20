//! `POST`/`GET /api/cards/{id}/planner/attachments[/{attachment_id}]`.
//! Reading is admitted for any actor (an agent can already open these bytes with its own tools); writing refuses a request that declares a non-`user` actor, and a request with no `X-Calm-Actor` header is `user`.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use calm_types::planner_attachment::{AttachmentId, UploadAttachmentResponse};

use crate::actor::Actor;
use crate::error::{CalmError, ErrorBody, Result};
use crate::ids::CardId;
use crate::model::Card;
use crate::routes::cards::card_runs_headless_harness;
use crate::routes::track_report_blocks::require_rest_user_actor_for;
use crate::state::{AppState, RouteState};

use super::{attachment_root, attachment_url, open_attachment, store};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/cards/{id}/planner/attachments",
            post(upload_planner_attachment),
        )
        .route(
            "/api/cards/{id}/planner/attachments/{attachment_id}",
            get(read_planner_attachment),
        )
}

struct AttachmentContext {
    card: Card,
    root: std::path::PathBuf,
    repo_root: std::path::PathBuf,
}

/// Card admission is the same predicate `GET /api/cards/{id}/harness/items` uses: the door onto an attachment must not be wider than the door onto the conversation.
async fn attachment_context(s: &RouteState, id: &str) -> Result<AttachmentContext> {
    let card = s
        .repo
        .card_get(id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    let role = s
        .write
        .verify_role(&card.id)
        .ok_or_else(|| CalmError::NotFound(format!("card {id}")))?;
    if !card_runs_headless_harness(&card, role) {
        return Err(CalmError::Forbidden(format!(
            "card {id} is not a planner codex card",
        )));
    }
    let track = s
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {} for card {id}", card.track_id)))?;
    let root = attachment_root(&track.workspace, &s.workspace_root)?;
    Ok(AttachmentContext {
        card,
        root,
        repo_root: std::path::PathBuf::from(&track.workspace.path),
    })
}

#[utoipa::path(
    post,
    path = "/api/cards/{id}/planner/attachments",
    tag = "cards",
    params(("id" = String, Path, description = "Planner card id")),
    request_body(
        content = Vec<u8>,
        description = "Raw image bytes. The declared Content-Type is not the judgement — the file's magic number is.",
        content_type = "application/octet-stream",
    ),
    responses(
        (status = 201, description = "Attachment stored", body = UploadAttachmentResponse),
        (status = 400, description = "Not one of PNG/JPEG/GIF/WebP, the track has an attached workspace, or the card's attachment budget is exhausted", body = ErrorBody),
        (status = 403, description = "Card is not a planner codex card, or the actor is not `user`", body = ErrorBody),
        (status = 404, description = "Card or track not found", body = ErrorBody),
        (status = 413, description = "File exceeds the per-file size limit", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn upload_planner_attachment(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
    body: Body,
) -> Result<(StatusCode, Json<UploadAttachmentResponse>)> {
    require_rest_user_actor_for(
        &actor,
        "planner attachment upload",
        "An agent already reads this workspace directly and has no upload channel.",
    )?;
    let context = attachment_context(&s, &id).await?;
    let card_id = context.card.id.clone();
    let stored = store::store_upload(
        &context.root,
        &context.repo_root,
        &card_id,
        &s.planner_attachment_locks,
        super::UPLOAD_DEADLINE,
        body,
    )
    .await?;
    let url = attachment_url(&card_id, &stored.id);
    tracing::info!(
        target: "planner_attachments::store",
        card_id = %card_id,
        attachment_id = %stored.id,
        size = stored.size,
        "planner attachment stored"
    );
    Ok((
        StatusCode::CREATED,
        Json(UploadAttachmentResponse {
            content_type: stored.id.format().mime().to_string(),
            attachment_id: stored.id,
            size: stored.size,
            url,
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/api/cards/{id}/planner/attachments/{attachment_id}",
    tag = "cards",
    params(
        ("id" = String, Path, description = "Planner card id"),
        ("attachment_id" = String, Path, description = "`<uuid>.<ext>` id returned by the upload"),
    ),
    responses(
        (status = 200, description = "Raw image bytes", body = Vec<u8>, content_type = "application/octet-stream"),
        (status = 400, description = "Malformed id, no such attachment on this card, or the track has an attached workspace", body = ErrorBody),
        (status = 403, description = "Card is not a planner codex card", body = ErrorBody),
        (status = 404, description = "Card or track not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn read_planner_attachment(
    State(s): State<RouteState>,
    Path((id, attachment_id)): Path<(String, String)>,
) -> Result<Response> {
    let attachment_id = AttachmentId::parse(&attachment_id)
        .map_err(|error| CalmError::BadRequest(error.to_string()))?;
    let context = attachment_context(&s, &id).await?;
    let card_id: CardId = context.card.id.clone();
    let opened = open_attachment(&context.root, &card_id, &attachment_id).await?;
    // Content type comes from the id's extension (the sniffed magic number), never from the file's current bytes or a header.
    // The `path` argument is only ever rendered into an error message that reaches the client, so it is the attachment id rather than the host path.
    let for_errors = std::path::PathBuf::from(attachment_id.as_str());
    crate::routes::fs::read_file_raw_response_from_handle(
        opened.file,
        opened.size,
        &for_errors,
        opened.format.mime(),
    )
    .await
}
