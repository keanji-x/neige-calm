//! `GET /api/agent-providers` — whether each Planner provider can run right now (#1817).

use crate::actor::Actor;
use crate::auth::{OWNER_ROLE, Principal};
use crate::extract::{Json, JsonBody, Query};
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::{
    Router,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::agent_providers::{Freshness, ProviderAvailability};
use crate::error::{CalmError, Result};
use crate::state::{AppState, CodexShellState, RouteState};

#[derive(Debug, Deserialize, IntoParams)]
pub struct AgentProvidersQuery {
    /// `true` runs every check again instead of answering from the last one (at most 30 s old).
    #[serde(default)]
    pub refresh: bool,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/agent-providers", get(list_agent_providers))
        .route(
            "/api/agent-providers/codex/retry",
            post(retry_codex_authentication),
        )
}

/// Whether each Planner provider can run right now. `ready` passed every check; `unavailable`
/// names the first failed check and its fix in `reason`; `not_configured` is a provider this
/// server was not started with (Claude without `--claude-planner-config`). Codex: the shared
/// app-server runs and `account/read` says it is logged in. Claude: the pinned binary answers
/// `--version` with the pinned version, `auth status --json` says `loggedIn`, and the CLI lists
/// its models (`initialize`, #1822). Answers are cached per provider for 30 s; the 30 s re-check
/// keeps the Claude model list, which `refresh=true` or a check after a failed one fetches again.
/// A failed check is an answer, never an error.
#[utoipa::path(
    get,
    path = "/api/agent-providers",
    tag = "agent_providers",
    params(AgentProvidersQuery),
    responses(
        (status = 200, description = "One entry per Planner provider: Codex, then Claude", body = [ProviderAvailability]),
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
        .all(
            freshness,
            &s.claude_planner,
            &s.acp_planner,
            &codex.shared_codex_appserver,
        )
        .await;
    Ok(Json(
        checked
            .into_iter()
            .map(ProviderAvailability::from)
            .collect(),
    ))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthenticationRetryRequest {
    /// The exact revision of the confirmed failure the owner read.
    pub expected_revision: String,
}
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationRetryStatus {
    RetryRequested,
}
#[derive(Serialize, ToSchema)]
pub struct ConversationRecoveryNotice {
    pub card_id: String,
    pub track_id: String,
    pub title: String,
    pub text: String,
}
#[derive(Serialize, ToSchema)]
pub struct AuthenticationRetryResponse {
    /// Owner authorization was saved; this is not a credential-verification result.
    pub status: AuthenticationRetryStatus,
    pub requested_revision: String,
    pub recovery_notices: Vec<ConversationRecoveryNotice>,
}

/// After signing in for the server, explicitly authorize queued messages to retry. No login,
/// credential refresh/copy or daemon restart is performed. A stale revision never rearms.
#[utoipa::path(
    post,path="/api/agent-providers/codex/retry",tag="agent_providers",
    request_body=AuthenticationRetryRequest,
    responses(
        (status=200,description="Retry authorization committed; read provider and conversation status for its outcome",body=AuthenticationRetryResponse),
        (status=400,description="Invalid request",body=crate::error::ErrorBody),
        (status=401,description="Owner login required",body=crate::error::ErrorBody),
        (status=403,description="Only the owner acting as a user may authorize this retry",body=crate::error::ErrorBody),
        (status=409,description="The observation changed or there is no confirmed failure to retry",body=crate::error::ErrorBody),
        (status=503,description="Retry status cannot be safely saved or recovery checked",body=crate::error::ErrorBody),
    )
)]
pub(crate) async fn retry_codex_authentication(
    State(s): State<RouteState>,
    State(w): State<crate::state::WorkerState>,
    State(cs): State<CodexShellState>,
    principal: Principal,
    actor: Actor,
    JsonBody(request): JsonBody<AuthenticationRetryRequest>,
) -> Result<Response> {
    if principal.role != OWNER_ROLE || actor.as_str() != "user" {
        return Err(CalmError::Forbidden(
            "Only the owner can retry the server's queued messages.".into(),
        ));
    }
    let requested_revision = cs
        .shared_codex_appserver
        .request_authentication_retry(&request.expected_revision)?;
    let recovery_notices =
        super::planner_recovery::retry_provider_conversations(&s, &w, &cs).await?;
    let mut response = Json(AuthenticationRetryResponse {
        status: AuthenticationRetryStatus::RetryRequested,
        requested_revision,
        recovery_notices,
    })
    .into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}
