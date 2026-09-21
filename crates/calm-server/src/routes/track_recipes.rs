//! `/api/track-recipes` — user-defined starting points for a new track: a saved report
//! whose `neige-block` fences are its tasks. Writes are a whole-document PUT under a
//! single `revision` CAS and emit no `Event`.

use crate::actor::Actor;
use crate::error::{CalmError, ErrorBody, Result};
use crate::routes::track_report_blocks::require_rest_user_actor_for;
use crate::state::{AppState, RouteState};
use crate::task_privilege::normalize_task_privilege_fields;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use calm_types::model::{NewTrackRecipe, TrackRecipe};
use calm_types::report_blocks::{KIND_TASK, parse_fence, render_fence, split_body};
use calm_types::report_contract::{check_document, normalize_header};
use serde::Deserialize;
use serde_json::Value;
use std::borrow::Cow;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/track-recipes", get(list_recipes).post(create_recipe))
        .route(
            "/api/track-recipes/{id}",
            get(get_recipe).put(update_recipe).delete(delete_recipe),
        )
}

/// Bring a body into the canonical shape a recipe is allowed to hold: every parseable
/// fence is re-rendered so instantiation's re-render is the identity. Task fences
/// additionally drop tombstones (a tombstone would poison the key in every instantiated
/// track; a blank line is restored so the neighbours do not re-parse as one paragraph),
/// normalize privilege fields, and drop `refs` (block ids are minted per track at
/// instantiation, so any entry names a block in some other track).
fn normalize_recipe_body(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    // Set when a tombstone was dropped; consumed by whatever is emitted next, which keeps
    // normalization idempotent.
    let mut pending_break = false;
    for slice in split_body(body) {
        let rendered = match parse_fence(&slice.raw) {
            Some(fence) if fence.kind == KIND_TASK => {
                if fence
                    .payload
                    .get("tombstone")
                    .is_some_and(|value| !value.is_null())
                {
                    pending_break = true;
                    continue;
                }
                match fence.payload {
                    Value::Object(mut payload) => {
                        // Not folded into `normalize_task_privilege_fields`: fork rewrites `refs`, it does not drop them.
                        payload.remove("refs");
                        normalize_task_privilege_fields(&mut payload);
                        render_fence(KIND_TASK, &Value::Object(payload))
                    }
                    // `parse_fence` only returns object payloads; keep the slice rather than inventing a shape.
                    _ => slice.raw,
                }
            }
            // Every other parseable fence is re-rendered and nothing else.
            Some(fence) => render_fence(&fence.kind, &fence.payload),
            // Not a well-formed fence — the lenient read calls it prose.
            None => slice.raw,
        };
        if pending_break {
            restore_paragraph_break(&mut out);
            pending_break = false;
        }
        out.push_str(&rendered);
    }
    out
}

/// End `out` on a blank line so whatever is appended next starts a new Markdown block.
/// No-op when empty or already blank-terminated, so repeated calls do not accumulate.
pub(super) fn restore_paragraph_break(out: &mut String) {
    if out.is_empty() || out.ends_with("\n\n") {
        return;
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
}

/// Validate a candidate recipe body the same way track creation validates the payload it
/// is about to instantiate. `BadRequest`, not `Internal`: this body came from the caller.
/// A body starting with `+++` is refused first: that prefix is template front matter.
fn validate_recipe_body(body: &str) -> Result<()> {
    if body.starts_with("+++") {
        return Err(CalmError::BadRequest(
            "recipe bodies must not start with `+++`; that prefix is reserved for template \
             files' front matter (#1635 D1)"
                .into(),
        ));
    }
    crate::track_report_guard::validate_body_fences(body)
        .map_err(|error| CalmError::BadRequest(format!("track recipe body: {error}")))?;
    check_document(body)
        .map(|_| ())
        .map_err(|error| CalmError::BadRequest(format!("report contract header: {error}")))
}

/// The recipe ingress: line 1 rewritten to the canonical header (or the body handed back
/// untouched when there is none). Runs after [`normalize_recipe_body`] and before
/// [`validate_recipe_body`], so the stored row is what the funnel will accept.
fn normalize_recipe_header(body: &str) -> Result<String> {
    normalize_header(body)
        .map(Cow::into_owned)
        .map_err(|error| CalmError::BadRequest(format!("report contract header: {error}")))
}

/// The same actor decision the block endpoints make; only the redirect sentence differs
/// (no MCP tool writes recipes at all).
fn require_recipe_user_actor(actor: &Actor) -> Result<()> {
    require_rest_user_actor_for(
        actor,
        "track recipe write",
        "Recipes are the human's own saved starting points and have no agent-facing write path.",
    )
}

fn validate_title(title: &str) -> Result<()> {
    if title.trim().is_empty() {
        return Err(CalmError::BadRequest(
            "track recipe title must not be empty".into(),
        ));
    }
    Ok(())
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateRecipeBody {
    pub title: String,
    pub body: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateRecipeBody {
    pub title: String,
    pub body: String,
    /// The `revision` the caller read. A mismatch is 409 — never a silent overwrite.
    pub if_revision: i64,
}

#[utoipa::path(
    get, path = "/api/track-recipes", tag = "track-recipes",
    responses(
        (status = 200, description = "Every user-defined recipe", body = Vec<TrackRecipe>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_recipes(State(s): State<RouteState>) -> Result<Json<Vec<TrackRecipe>>> {
    Ok(Json(s.repo.track_recipe_list().await?))
}

#[utoipa::path(
    get, path = "/api/track-recipes/{id}", tag = "track-recipes",
    responses(
        (status = 200, description = "One recipe", body = TrackRecipe),
        (status = 404, description = "No such recipe", body = ErrorBody),
    ),
)]
pub(crate) async fn get_recipe(
    State(s): State<RouteState>,
    Path(id): Path<String>,
) -> Result<Json<TrackRecipe>> {
    s.repo
        .track_recipe_get(&id)
        .await?
        .map(Json)
        .ok_or_else(|| CalmError::NotFound(format!("track recipe {id}")))
}

#[utoipa::path(
    post, path = "/api/track-recipes", tag = "track-recipes",
    request_body = CreateRecipeBody,
    responses(
        (status = 201, description = "Recipe created", body = TrackRecipe),
        (status = 400, description = "Malformed body or empty title", body = ErrorBody),
        (status = 403, description = "Only `X-Calm-Actor: user` may write recipes", body = ErrorBody),
    ),
)]
pub(crate) async fn create_recipe(
    State(s): State<RouteState>,
    actor: Actor,
    Json(body): Json<CreateRecipeBody>,
) -> Result<(StatusCode, Json<TrackRecipe>)> {
    require_recipe_user_actor(&actor)?;
    validate_title(&body.title)?;
    let normalized = normalize_recipe_header(&normalize_recipe_body(&body.body))?;
    validate_recipe_body(&normalized)?;
    let created = s
        .repo
        .track_recipe_create(NewTrackRecipe {
            title: body.title,
            body: normalized,
        })
        .await?;
    Ok((StatusCode::CREATED, Json(created)))
}

#[utoipa::path(
    put, path = "/api/track-recipes/{id}", tag = "track-recipes",
    request_body = UpdateRecipeBody,
    responses(
        (status = 200, description = "Recipe replaced", body = TrackRecipe),
        (status = 400, description = "Malformed body or empty title", body = ErrorBody),
        (status = 403, description = "Only `X-Calm-Actor: user` may write recipes", body = ErrorBody),
        (status = 404, description = "No such recipe", body = ErrorBody),
        (status = 409, description = "`if_revision` is stale", body = ErrorBody),
    ),
)]
pub(crate) async fn update_recipe(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
    Json(body): Json<UpdateRecipeBody>,
) -> Result<Json<TrackRecipe>> {
    require_recipe_user_actor(&actor)?;
    validate_title(&body.title)?;
    let normalized = normalize_recipe_header(&normalize_recipe_body(&body.body))?;
    validate_recipe_body(&normalized)?;
    Ok(Json(
        s.repo
            .track_recipe_update(
                &id,
                NewTrackRecipe {
                    title: body.title,
                    body: normalized,
                },
                body.if_revision,
            )
            .await?,
    ))
}

#[utoipa::path(
    delete, path = "/api/track-recipes/{id}", tag = "track-recipes",
    responses(
        (status = 204, description = "Recipe deleted"),
        (status = 403, description = "Only `X-Calm-Actor: user` may write recipes", body = ErrorBody),
        (status = 404, description = "No such recipe", body = ErrorBody),
    ),
)]
pub(crate) async fn delete_recipe(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    require_recipe_user_actor(&actor)?;
    s.repo.track_recipe_delete(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}
