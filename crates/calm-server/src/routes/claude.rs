//! `/internal/claude/hook` — receive Claude Code hook events from the
//! bridge subprocess and re-emit them on the WS event bus.
//!
//! This is the Claude sibling of `routes::codex`: the shared ingest helper
//! resolves `card_id`, stamps the provider-specific AI actor, derives the
//! `hook.claude.<snake_case_name>` kind, and persists an opaque hook payload
//! as `Event::ClaudeHook`.

use crate::actor::Actor;
use crate::error::Result;
use crate::routes::codex::{HookProvider, IngestQuery, ingest_provider_hook};
use crate::state::{AppState, RouteState};
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::post,
};
use serde_json::{Value, json};

pub fn router() -> Router<AppState> {
    Router::new().route("/internal/claude/hook", post(ingest_hook))
}

pub(crate) async fn ingest_hook(
    State(s): State<RouteState>,
    _actor: Actor,
    Query(q): Query<IngestQuery>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>> {
    ingest_provider_hook(&s, q.card_id, payload, HookProvider::Claude).await?;
    Ok(Json(json!({ "continue": true })))
}
