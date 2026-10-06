use super::{
    PLUGIN_ID,
    model::*,
    store::{self, Access},
};
use crate::extract::{JsonBody, Path, Query};
use crate::{
    actor::Actor,
    error::{CalmError, ErrorBody, Result},
    event::EventScope,
    ids::ActorId,
    state::{AppState, RouteState},
};
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};

/// The answers every calendar route shares, from [`access`].
const FORBIDDEN: &str =
    "`forbidden`: the caller is not `X-Calm-Actor: user`; AI calls use the calendar tools";
const UNAVAILABLE: &str = "`service_unavailable`: the Calendar plugin is not running";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/calendar/tasks", get(list).post(create))
        .route("/api/calendar/tasks/{id}", post(update))
}
async fn access(s: &RouteState, actor: &Actor) -> Result<Access> {
    crate::routes::track_report_blocks::require_rest_user_actor_for(
        actor,
        "calendar tasks",
        "AI calls use neige_calendar_* tools.",
    )?;
    let host = s
        .mcp_context
        .plugin_host
        .get()
        .ok_or_else(|| CalmError::ServiceUnavailable("plugin host unavailable".into()))?;
    if !host.running_plugin_ids().await.contains(PLUGIN_ID) {
        return Err(CalmError::ServiceUnavailable(
            "Calendar is temporarily unavailable".into(),
        ));
    }
    Ok(Access {
        track: None,
        actor: ActorId::User,
        scope: EventScope::System,
        creator: "user".into(),
    })
}
#[utoipa::path(get, path="/api/calendar/tasks", tag="calendar", params(
    ("from"=String, Query), ("until"=String, Query), ("timezone"=String, Query)
), responses(
    (status=200, body=Vec<Listed>),
    (status=400, body=ErrorBody, description="`bad_request`: the window is not valid dates and an IANA timezone spanning 1 to 366 days"),
    (status=403, body=ErrorBody, description=FORBIDDEN),
    (status=500, body=ErrorBody),
    (status=503, body=ErrorBody, description=UNAVAILABLE),
))]
pub async fn list(
    State(s): State<RouteState>,
    actor: Actor,
    Query(window): Query<Window>,
) -> Result<Json<Vec<Listed>>> {
    let access = access(&s, &actor).await?;
    Ok(Json(store::list(&s.mcp_context, &access, window).await?))
}
#[utoipa::path(post, path="/api/calendar/tasks", tag="calendar", request_body=Create, responses(
    (status=200, body=Entry, description="Created, or the entry an earlier create under the same `idempotency_key` made"),
    (status=400, body=ErrorBody, description="`bad_request`: a time or timezone is not valid, or `idempotency_key` is not 1 to 200 bytes"),
    (status=403, body=ErrorBody, description=FORBIDDEN),
    (status=409, body=ErrorBody, description="`conflict`: the `idempotency_key` was already used for a different task"),
    (status=500, body=ErrorBody),
    (status=503, body=ErrorBody, description=UNAVAILABLE),
))]
pub async fn create(
    State(s): State<RouteState>,
    actor: Actor,
    JsonBody(request): JsonBody<Create>,
) -> Result<Json<Entry>> {
    let access = access(&s, &actor).await?;
    Ok(Json(store::create(&s.mcp_context, access, request).await?))
}
#[utoipa::path(post, path="/api/calendar/tasks/{id}", tag="calendar", params(("id"=String, Path)), request_body=Update, responses(
    (status=200, body=Entry),
    (status=400, body=ErrorBody, description="`bad_request`: a time or timezone is not valid"),
    (status=403, body=ErrorBody, description=FORBIDDEN),
    (status=404, body=ErrorBody, description="`not_found`: no calendar task with this id"),
    (status=409, body=ErrorBody, description="`conflict`: the task changed since `expected_version`; reload before editing"),
    (status=500, body=ErrorBody),
    (status=503, body=ErrorBody, description=UNAVAILABLE),
))]
pub async fn update(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
    JsonBody(request): JsonBody<Update>,
) -> Result<Json<Entry>> {
    let access = access(&s, &actor).await?;
    let expected_version = request.expected_version;
    Ok(Json(
        store::update(
            &s.mcp_context,
            access,
            id,
            expected_version,
            store::Change::Replace(request),
        )
        .await?,
    ))
}
