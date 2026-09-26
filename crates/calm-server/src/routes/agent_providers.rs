//! `GET /api/agent-providers` — whether each Planner provider can run right now (#1817).

use axum::extract::{Query, State};
use axum::{Json, Router, routing::get};
use serde::Deserialize;
use utoipa::IntoParams;

use crate::agent_providers::{Freshness, ProviderAvailability};
use crate::error::{ErrorBody, Result};
use crate::state::{AppState, CodexShellState, RouteState};

#[derive(Debug, Deserialize, IntoParams)]
pub struct AgentProvidersQuery {
    /// `true` runs every check again instead of answering from the last one (at most 30 s old).
    #[serde(default)]
    pub refresh: bool,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/agent-providers", get(list_agent_providers))
}

#[utoipa::path(
    get,
    path = "/api/agent-providers",
    tag = "agent_providers",
    params(AgentProvidersQuery),
    responses(
        (status = 200, description = "One entry per Planner provider, Codex then Claude. `ready` passed every check; `unavailable` names the first failed check and its fix in `reason`; `not_configured` is a provider this server was not started with (Claude without `--claude-planner-config`). Codex: the shared app-server runs and `account/read` says it is logged in. Claude: the pinned binary answers `--version` with the pinned version and `auth status --json` says `loggedIn`. Answers are cached per provider for 30 s; a failed check is an answer, never an error.", body = [ProviderAvailability]),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_agent_providers(
    State(s): State<RouteState>,
    State(codex): State<CodexShellState>,
    Query(q): Query<AgentProvidersQuery>,
) -> Result<Json<Vec<ProviderAvailability>>> {
    let freshness = if q.refresh {
        Freshness::Recheck
    } else {
        Freshness::Cached
    };
    let checked = s
        .provider_availability
        .all(freshness, &s.claude_planner, &codex.shared_codex_appserver)
        .await;
    Ok(Json(
        checked
            .into_iter()
            .map(ProviderAvailability::from)
            .collect(),
    ))
}
