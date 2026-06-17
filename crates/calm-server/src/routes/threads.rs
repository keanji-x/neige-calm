//! Thread resolution helpers for shared codex daemon hooks.

use crate::error::{CalmError, ErrorBody, Result};
use crate::model::CardRole;
use crate::session_projection_lookup::resolve_card_for_thread as resolve_card_for_thread_runtime;
use crate::session_projection_repo::AgentProvider;
use crate::state::{AppState, RouteState};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/threads/{thread_id}/card",
        get(resolve_card_for_thread),
    )
}

/// Bridge endpoint: resolve a codex thread_id back to its owning card_id.
///
/// Used by neige-codex-bridge to attribute hooks fired by the shared codex
/// app-server to the correct card.
#[utoipa::path(
    get,
    path = "/api/threads/{thread_id}/card",
    tag = "threads",
    params(
        ("thread_id" = String, Path, description = "Provider thread/session id"),
        ("provider" = Option<String>, Query, description = "Agent provider: codex or claude; defaults to codex"),
    ),
    responses(
        (status = 200, description = "Owning card for this provider thread/session", body = ThreadCardResolution),
        (status = 400, description = "Invalid provider", body = ErrorBody),
        (status = 404, description = "No card is mapped to this thread", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub async fn resolve_card_for_thread(
    State(s): State<RouteState>,
    Path(thread_id): Path<String>,
    Query(q): Query<ResolveThreadQuery>,
) -> Result<Json<ThreadCardResolution>> {
    let provider = parse_provider(q.provider.as_deref())?;
    let card_id = resolve_card_for_thread_runtime(s.repo.as_ref(), provider, &thread_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("thread {thread_id}")))?;
    let card = s
        .repo
        .card_get(&card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    let role = s
        .repo
        .card_role_get(&card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    Ok(Json(ThreadCardResolution {
        thread_id,
        card_id,
        role,
        wave_id: Some(card.wave_id.to_string()),
    }))
}

#[derive(Debug, Deserialize)]
pub struct ResolveThreadQuery {
    pub provider: Option<String>,
}

fn parse_provider(provider: Option<&str>) -> Result<AgentProvider> {
    match provider {
        None | Some("codex") => Ok(AgentProvider::Codex),
        Some("claude") => Ok(AgentProvider::Claude),
        Some(other) => Err(CalmError::BadRequest(format!(
            "invalid provider {other:?}; expected codex or claude"
        ))),
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ThreadCardResolution {
    pub thread_id: String,
    pub card_id: String,
    pub role: CardRole,
    pub wave_id: Option<String>,
}
