//! `/api/track-recipes` — user-defined starting points for a new track (#1292).
//!
//! # What a recipe is
//!
//! A saved report: a title (which doubles as the instantiated report's
//! summary) and a body whose `neige-block` fences **are** its tasks. It is
//! deliberately not a track. #1300 removed "template = a hidden wave" because
//! that shape cost seven "this wave is special" exceptions across unrelated
//! subsystems plus a kernel report write that impersonated the user; storing
//! recipes as tracks again would buy back every one of those.
//!
//! The three built-in templates stay Rust constants and are **not** rows
//! here. Both feed the same instantiation seam
//! (`routes::tracks::prepare_initial_report_payload`), so "built-in" and
//! "mine" differ only in where the payload came from. Built-ins are
//! therefore read-only by construction rather than by a guard: there is no
//! row to write.
//!
//! # Why the write side is a whole-document PUT, not block ops
//!
//! A track's report needs block-level CAS because a user, a spec author and
//! an agent write it concurrently and none of them knows about the others —
//! losing an update there costs attribution and audit.
//!
//! A recipe's only writer is its owner, possibly from two windows. That is
//! still concurrency, but the correct handling differs: showing the second
//! writer a conflict is enough, and the cost of losing is redoing one edit.
//! So the lock is a single `revision` CAS and the answer to a stale write is
//! 409 — not a merge engine that nothing here needs.
//!
//! # No events
//!
//! Recipe writes emit no `Event`. Minting a variant would pull in frontend
//! zod schemas, invalidation policies and golden counts, and would buy only
//! "the other window refreshes by itself" — while the `revision` CAS already
//! ensures the other window cannot silently clobber. Deferred deliberately;
//! see #1292 design §3.1b.

use crate::actor::Actor;
use crate::error::{CalmError, ErrorBody, Result};
use crate::routes::track_report_blocks::require_rest_user_actor;
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
use serde::Deserialize;
use serde_json::Value;
use utoipa::ToSchema;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/track-recipes", get(list_recipes).post(create_recipe))
        .route(
            "/api/track-recipes/{id}",
            get(get_recipe).put(update_recipe).delete(delete_recipe),
        )
}

/// Bring a body into the canonical shape a recipe is allowed to hold.
///
/// Two transforms, and both are about **not carrying one track's authority
/// into every wave made from this recipe**:
///
/// 1. **Tombstones are dropped.** A tombstone blocks re-declaring its key
///    (`report_blocks::tasks`), so a recipe carrying one would poison every
///    instantiated wave — that key could never be used again in any of them.
///    This is the one place recipes deliberately diverge from fork, which
///    *keeps* tombstones because they are that track's audit history. A
///    recipe has no history to preserve; it describes work not yet done.
/// 2. **Privilege fields are normalized** via the same
///    [`normalize_task_privilege_fields`] the fork path calls, so
///    `declared_by`/`ready`/`released_by_user` cannot smuggle in authorship
///    or a human approval granted somewhere else.
///
/// Prose slices pass through byte-identical. Non-task fences pass through
/// too: this function has an opinion about task authority, not about
/// vocabulary.
///
/// Runs at the write boundary rather than at instantiation so the stored row
/// is already canonical — which is what makes "what the picker shows" and
/// "what create produces" the same bytes, structurally. Reading the same row
/// twice cannot disagree with itself; that was #1230's failure shape.
fn normalize_recipe_body(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    for slice in split_body(body) {
        let Some(fence) = parse_fence(&slice.raw) else {
            out.push_str(&slice.raw);
            continue;
        };
        if fence.kind != KIND_TASK {
            out.push_str(&slice.raw);
            continue;
        }
        if fence
            .payload
            .get("tombstone")
            .is_some_and(|value| !value.is_null())
        {
            continue;
        }
        let mut payload = match fence.payload {
            Value::Object(map) => map,
            // `parse_fence` only returns object payloads; keep the slice
            // rather than inventing a shape if that ever changes.
            _ => {
                out.push_str(&slice.raw);
                continue;
            }
        };
        normalize_task_privilege_fields(&mut payload);
        out.push_str(&render_fence(KIND_TASK, &Value::Object(payload)));
    }
    out
}

/// Validate a candidate recipe body the same way wave creation validates the
/// payload it is about to instantiate.
///
/// `BadRequest`, not `Internal`: unlike `prepare_template_report`, whose
/// every byte comes from a Rust constant, this body came from the caller.
fn validate_recipe_body(body: &str) -> Result<()> {
    crate::track_report_guard::validate_body_fences(body)
        .map_err(|error| CalmError::BadRequest(format!("wave recipe body: {error}")))
}

fn validate_title(title: &str) -> Result<()> {
    if title.trim().is_empty() {
        return Err(CalmError::BadRequest(
            "wave recipe title must not be empty".into(),
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
    /// The `revision` the caller read. A mismatch is 409 — never a silent
    /// overwrite.
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
        .ok_or_else(|| CalmError::NotFound(format!("wave recipe {id}")))
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
    require_rest_user_actor(&actor)?;
    validate_title(&body.title)?;
    let normalized = normalize_recipe_body(&body.body);
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
    require_rest_user_actor(&actor)?;
    validate_title(&body.title)?;
    let normalized = normalize_recipe_body(&body.body);
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
    require_rest_user_actor(&actor)?;
    s.repo.track_recipe_delete(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}
