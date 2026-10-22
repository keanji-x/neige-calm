//! `GET /api/models` — the model catalog the planner's model picker reads, plus the
//! default this installation follows. Daemon failures answer 200 with an explicit `source`.
//! A Claude Planner's catalog is the Claude CLI's own model list, fetched and cached by the
//! Claude availability check (#1822).

use crate::extract::{Json, Query};
use crate::harness::profile::HarnessProfileExt;
use crate::planner_model::catalog::ModelsResponse;

use axum::extract::State;
use axum::{Router, routing::get};
use serde::Deserialize;
use utoipa::IntoParams;

use crate::error::{CalmError, ErrorBody, Result};
use crate::session_projection_repo::AgentProvider;
use crate::state::{AppState, CodexShellState, RouteState};

/// Budget for the whole request, not each codex read. Passed down into
/// `request_until` rather than wrapped in `tokio::time::timeout`: cancelling from
/// outside leaks the pending-map entry.
pub use crate::planner_model::catalog::CODEX_READ_TIMEOUT;

#[derive(Debug, Deserialize, IntoParams)]
pub struct ModelsQuery {
    /// Resolve the default against this card's workspace; without one the read has no
    /// `cwd` and `default_source` is `unknown`.
    pub card_id: Option<String>,
    /// The Planner provider a card is about to be created with. `claude` (like a `card_id`
    /// naming a Claude Planner card) answers the Claude CLI's cached model list with
    /// `source: "live"` without asking Codex, or `source: "unavailable"` with an empty catalog
    /// while Claude is not ready.
    pub provider: Option<AgentProvider>,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/models", get(list_models))
}

#[utoipa::path(
    get,
    path = "/api/models",
    tag = "models",
    params(ModelsQuery),
    responses(
        (status = 200, description = "Model catalog and the default this installation follows. Codex: `source: \"live\"`, or `source: \"unavailable\"` with an empty catalog if codex cannot be reached (never an error or a hardcoded list). A Claude Planner: `source: \"live\"` with the Claude CLI's cached model list (its `default` entry as `default`, `default_source: \"claude_cli\"`), or `unavailable` with an empty catalog while Claude is not ready", body = ModelsResponse),
        (status = 404, description = "`card_id` names a card that does not exist", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_models(
    State(s): State<RouteState>,
    State(codex): State<CodexShellState>,
    Query(q): Query<ModelsQuery>,
) -> Result<Json<ModelsResponse>> {
    // Resolved before codex is asked so a bad `card_id` is rejected regardless of daemon
    // state. A blank `card_id=` means "no card supplied".
    let card = match q
        .card_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(card_id) => Some(resolve_card_workspace(&s, card_id).await?),
        None => None,
    };
    if let (Some(card), Some(requested)) = (card.as_ref(), q.provider.as_ref())
        && &card.provider != requested
    {
        return Err(CalmError::BadRequest(
            "The catalog provider does not match this conversation".into(),
        ));
    }
    // A card's immutable binding owns its catalog; a query cannot override it.
    let provider = card
        .as_ref()
        .map(|card| card.provider.clone())
        .or(q.provider.clone())
        .unwrap_or(AgentProvider::Codex);
    Ok(Json(
        crate::planner_model::acquisition::CatalogAccess::new(
            &s.provider_availability,
            &s.claude_planner,
            &s.acp_planner,
            &codex.shared_codex_appserver,
        )
        .read(
            &provider,
            q.card_id.as_deref(),
            card.as_ref().map(|c| c.workspace.as_str()),
        )
        .await?,
    ))
}

/// A card's workspace and whether it is a Claude Planner card.
struct ResolvedCard {
    workspace: String,
    provider: AgentProvider,
}

/// The workspace path a card's codex thread runs in — the same value
/// `planner-harness-start` puts on its payload. Any card resolves, not only a planner card.
async fn resolve_card_workspace(s: &RouteState, card_id: &str) -> Result<ResolvedCard> {
    let card = s
        .repo
        .card_get(card_id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?;
    let role = s.write.verify_role(&card.id);
    let provider = match role {
        Some(role)
            if crate::harness::profile::HarnessProfile::from_shape(
                &card.kind,
                role,
                &card.payload,
            )
            .is_some()
                || role == crate::model::CardRole::Assistant =>
        {
            crate::harness::profile::checked_card_binding(s.repo.as_ref(), &card, role)
                .await?
                .provider
        }
        _ => AgentProvider::Codex,
    };
    let track = s
        .repo
        .track_get(card.track_id.as_str())
        .await?
        .ok_or_else(|| {
            CalmError::NotFound(format!("track {} for card {card_id}", card.track_id))
        })?;
    Ok(ResolvedCard {
        workspace: track.workspace.agent_cwd().to_string(),
        provider,
    })
}
