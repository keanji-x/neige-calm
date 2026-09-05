//! `POST`/`GET /api/cards/{id}/planner/attachments[/{attachment_id}]`.
//!
//! # Why there is a dedicated read-back endpoint
//!
//! `GET /api/fs/readfile-raw` would work with zero new code — it accepts any
//! absolute path and has no workspace boundary. That is exactly the problem:
//! the browser would have to hold and send back the attachment's absolute host
//! path, which turns "the harness already ships host paths to the client" from
//! an existing fact into something a product feature depends on, and it grows a
//! boundary-free read primitive a permanent consumer. Here the server derives
//! the directory from `(card -> track -> workspace)` and joins a validated id
//! onto it, so there is no traversal surface to defend in the first place.
//!
//! # The two ends are guarded differently, on purpose
//!
//! Reading is admitted for any actor, exactly like `GET /harness/items`: an
//! agent's working directory *is* this workspace, so it can already open these
//! bytes with its own tools, and a guard it can walk around is a guard that
//! only misleads the next reader. Writing is human-only, because that is the
//! one direction that puts new content into a directory an agent reads.

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

use super::{attachment_root, attachment_url, resolve, store};

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

/// Everything both handlers need: the card, its `.neige/attachments` root, and
/// the git work tree that root lives in.
struct AttachmentContext {
    card: Card,
    root: std::path::PathBuf,
    repo_root: std::path::PathBuf,
}

/// Card admission is the same predicate `GET /api/cards/{id}/harness/items`
/// uses. An attachment is conversation content, so the door onto it must not be
/// wider than the door onto the conversation.
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
    let stored = store::store_upload(&context.root, &context.repo_root, &card_id, body).await?;
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
    let (path, format) = resolve(&context.root, &card_id, &attachment_id)?;
    let file = tokio::fs::File::open(&path).await.map_err(|error| {
        CalmError::BadRequest(format!(
            "attachment `{attachment_id}` is unreadable: {error}"
        ))
    })?;
    let size = file
        .metadata()
        .await
        .map_err(|error| {
            CalmError::Internal(format!("attachment `{attachment_id}` metadata: {error}"))
        })?
        .len();
    // Content type comes from the id's extension, which came from the sniffed
    // magic number — never from the file's current bytes and never from a
    // header. `read_file_raw_response_from_handle` supplies `nosniff`, a
    // sandbox CSP and `no-store`; none of that is restated here.
    crate::routes::fs::read_file_raw_response_from_handle(file, size, &path, format.mime()).await
}
