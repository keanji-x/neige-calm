use super::{
    PLUGIN_ID,
    model::*,
    store::{self, Access},
};
use crate::{
    actor::Actor,
    error::{CalmError, Result},
    event::EventScope,
    ids::ActorId,
    state::{AppState, RouteState},
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/calendar/tasks", get(list).post(create))
        .route("/api/calendar/tasks/{id}", post(update))
}
async fn access(s: &RouteState, actor: &Actor) -> Result<Access> {
    crate::routes::track_report_blocks::require_rest_user_actor_for(
        actor,
        "calendar tasks",
        "AI calls use calm.calendar.* tools.",
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
), responses((status=200, body=Vec<Listed>)))]
pub async fn list(
    State(s): State<RouteState>,
    actor: Actor,
    Query(window): Query<Window>,
) -> Result<Json<Vec<Listed>>> {
    let access = access(&s, &actor).await?;
    Ok(Json(store::list(&s.mcp_context, &access, window).await?))
}
#[utoipa::path(post, path="/api/calendar/tasks", tag="calendar", request_body=Create, responses((status=200, body=Entry)))]
pub async fn create(
    State(s): State<RouteState>,
    actor: Actor,
    Json(request): Json<Create>,
) -> Result<Json<Entry>> {
    let access = access(&s, &actor).await?;
    Ok(Json(store::create(&s.mcp_context, access, request).await?))
}
#[utoipa::path(post, path="/api/calendar/tasks/{id}", tag="calendar", params(("id"=String, Path)), request_body=Update, responses((status=200, body=Entry)))]
pub async fn update(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
    Json(request): Json<Update>,
) -> Result<Json<Entry>> {
    let access = access(&s, &actor).await?;
    Ok(Json(
        store::update(&s.mcp_context, access, id, request).await?,
    ))
}
