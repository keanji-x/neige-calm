//! `/api/read-receipts` (#2596): the one user's read state, kept by the kernel instead of each
//! browser, so a refresh or a second device reads the same receipts. Writes answer the stored
//! receipt and emit no event; another device sees them on its next read (focus, visibility).

use axum::{
    Router,
    extract::State,
    routing::{get, post},
};
use calm_types::read_receipts::{MarkReadRequest, ReadReceipt, ReadReceiptKind, ReadReceipts};

use crate::actor::Actor;
use crate::error::{CalmError, ErrorBody, Result};
use crate::extract::{Json, JsonBody, Path};
use crate::state::{AppState, RouteState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/read-receipts", get(list_read_receipts))
        .route(
            "/api/read-receipts/{kind}/{target_id}/read",
            post(mark_read),
        )
        .route(
            "/api/read-receipts/{kind}/{target_id}/unread",
            post(mark_unread),
        )
}

fn kind_of(kind: &str) -> Result<ReadReceiptKind> {
    ReadReceiptKind::parse(kind).ok_or_else(|| {
        CalmError::BadRequest(format!(
            "read receipt: `{kind}` is not a receipt kind (track or conversation)"
        ))
    })
}

fn require_user(actor: &Actor) -> Result<()> {
    super::track_report_blocks::require_rest_user_actor_for(
        actor,
        "read receipt",
        "Only the user reads.",
    )
}

/// The cutover baseline (activity at or before it is read) and every stored receipt.
#[utoipa::path(
    get,
    path = "/api/read-receipts",
    tag = "read_receipts",
    responses(
        (status = 200, description = "The baseline and every receipt", body = ReadReceipts),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_read_receipts(State(s): State<RouteState>) -> Result<Json<ReadReceipts>> {
    Ok(Json(s.repo.read_receipts_load().await?))
}

/// Mark a target read through `readThroughMs`: the stored instant is the larger of the two, and a
/// manual unread is cleared. Idempotent.
#[utoipa::path(
    post,
    path = "/api/read-receipts/{kind}/{target_id}/read",
    tag = "read_receipts",
    params(
        ("kind" = String, Path, description = "`track` or `conversation`"),
        ("target_id" = String, Path, description = "The track or conversation id"),
    ),
    request_body = MarkReadRequest,
    responses(
        (status = 200, description = "The stored receipt", body = ReadReceipt),
        (status = 400, description = "Unknown kind, or a negative instant", body = ErrorBody),
        (status = 403, description = "The actor is not the authenticated user", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn mark_read(
    State(s): State<RouteState>,
    actor: Actor,
    Path((kind, target_id)): Path<(String, String)>,
    JsonBody(body): JsonBody<MarkReadRequest>,
) -> Result<Json<ReadReceipt>> {
    require_user(&actor)?;
    let kind = kind_of(&kind)?;
    if body.read_through_ms < 0 {
        return Err(CalmError::BadRequest(format!(
            "read receipt: readThroughMs {} is negative",
            body.read_through_ms
        )));
    }
    let receipt = s
        .repo
        .read_receipt_mark_read(kind, &target_id, body.read_through_ms)
        .await?;
    Ok(Json(receipt))
}

/// Mark a target unread by hand; the next mark read clears it. Idempotent.
#[utoipa::path(
    post,
    path = "/api/read-receipts/{kind}/{target_id}/unread",
    tag = "read_receipts",
    params(
        ("kind" = String, Path, description = "`track` or `conversation`"),
        ("target_id" = String, Path, description = "The track or conversation id"),
    ),
    responses(
        (status = 200, description = "The stored receipt", body = ReadReceipt),
        (status = 400, description = "Unknown kind", body = ErrorBody),
        (status = 403, description = "The actor is not the authenticated user", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn mark_unread(
    State(s): State<RouteState>,
    actor: Actor,
    Path((kind, target_id)): Path<(String, String)>,
) -> Result<Json<ReadReceipt>> {
    require_user(&actor)?;
    let kind = kind_of(&kind)?;
    let receipt = s.repo.read_receipt_mark_unread(kind, &target_id).await?;
    Ok(Json(receipt))
}
