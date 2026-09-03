//! `/api/track-recipes` — user-defined starting points for a new track (#1292).
//!
//! # What a recipe is
//!
//! A saved report: a title (which doubles as the instantiated report's
//! summary) and a body whose `neige-block` fences **are** its tasks. It is
//! deliberately not a track. #1300 removed "template = a hidden track" because
//! that shape cost seven "this track is special" exceptions across unrelated
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
//! A track's report needs block-level CAS because a user, a planner agent and
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
/// Every parseable `neige-block` fence is re-rendered through
/// [`render_fence`], so the stored body holds each one in exactly the form
/// [`render_fence`] produces. That is not cosmetic. Instantiation
/// (`routes::tracks::prepare_initial_report_payload` → `ReportDoc` →
/// `reassign_ids`) re-renders every fence it can parse, so any fence stored
/// in some other spelling — a compact `app` payload, say — would come out of
/// `create` with different bytes than the picker showed. Canonicalising here
/// makes that re-render the identity, which turns "the recipe and the track
/// made from it are byte-for-byte the same document" from a claim into a
/// construction.
///
/// Two further transforms apply to task fences only, and both are about
/// **not carrying one track's authority into every track made from this
/// recipe**:
///
/// 1. **Tombstones are dropped**, leaving a blank-line boundary behind. A
///    tombstone blocks re-declaring its key (`report_blocks::tasks`), so a
///    recipe carrying one would poison every instantiated track — that key
///    could never be used again in any of them. This is the one place
///    recipes deliberately diverge from fork, which *keeps* tombstones
///    because they are that track's audit history. A recipe has no history
///    to preserve; it describes work not yet done.
///
///    The blank line is not cosmetic. Splicing the dropped fence's two prose
///    neighbours together makes them one Markdown paragraph context, and the
///    join can *create* syntax neither side wrote: `foo\n` followed by
///    `---\n` is a Setext H2 titled "foo", where the original had a
///    paragraph and a thematic break. Deleting one task must not re-parse
///    the prose around it, so [`restore_paragraph_break`] reinstates the
///    paragraph boundary the fence used to provide.
/// 2. **Privilege fields are normalized** via the same
///    [`normalize_task_privilege_fields`] the fork path calls, so
///    `declared_by`/`ready`/`released_by_user` cannot smuggle in authorship
///    or a human approval granted somewhere else.
///
/// Prose slices pass through byte-identical, and so does anything
/// [`parse_fence`] declines — the lenient read treats those as prose too, and
/// rewriting text nobody could parse is not this function's job.
///
/// Runs at the write boundary rather than at instantiation so the stored row
/// is already canonical — which is what makes "what the picker shows" and
/// "what create produces" the same bytes, structurally. Reading the same row
/// twice cannot disagree with itself; that was #1230's failure shape.
fn normalize_recipe_body(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    // Set when a tombstone was dropped; consumed by whatever is emitted
    // next. Deferring it this way is what keeps normalization idempotent:
    // a body with nothing after the tombstone gains no trailing blank
    // line, and a normalized body has no tombstones left to drop, so
    // re-normalizing it is byte-for-byte the identity.
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
                        normalize_task_privilege_fields(&mut payload);
                        render_fence(KIND_TASK, &Value::Object(payload))
                    }
                    // `parse_fence` only returns object payloads; keep the
                    // slice rather than inventing a shape if that changes.
                    _ => slice.raw,
                }
            }
            // Every other parseable fence is re-rendered and nothing else:
            // no opinion about its payload, only about its bytes.
            Some(fence) => render_fence(&fence.kind, &fence.payload),
            // Not a well-formed fence — the lenient read calls it prose, and
            // prose passes through untouched.
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

/// End `out` on a blank line so that whatever is appended next starts a new
/// Markdown block, never a continuation of the last one.
///
/// No-op when `out` is empty (nothing to separate from) or already ends on a
/// blank line (nothing to add) — which is what makes repeated calls, and
/// therefore repeated normalization, non-accumulating.
fn restore_paragraph_break(out: &mut String) {
    if out.is_empty() || out.ends_with("\n\n") {
        return;
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
}

/// Validate a candidate recipe body the same way track creation validates the
/// payload it is about to instantiate.
///
/// `BadRequest`, not `Internal`: unlike `prepare_template_report`, whose
/// every byte comes from a Rust constant, this body came from the caller.
fn validate_recipe_body(body: &str) -> Result<()> {
    crate::track_report_guard::validate_body_fences(body)
        .map_err(|error| CalmError::BadRequest(format!("track recipe body: {error}")))
}

/// The same actor decision the block endpoints make, said in this endpoint's
/// own words.
///
/// The rule is identical — REST writes are the human's channel — so the
/// *judgement* stays in [`require_rest_user_actor`] and is never restated
/// here; restating it is how the two drift apart. Only the sentence differs:
/// the block endpoints' text points the refused caller at `calm.report.*`,
/// which is the right redirect for a track report and the wrong one for a
/// recipe (no MCP tool writes recipes at all — an agent that wants this
/// starting point asks its human for it).
fn require_recipe_user_actor(actor: &Actor) -> Result<()> {
    match require_rest_user_actor(actor) {
        Ok(()) => Ok(()),
        Err(CalmError::Forbidden(_)) => Err(CalmError::Forbidden(format!(
            "track recipe write: only `X-Calm-Actor: user` is allowed; got `{}`. Recipes are the \
             human's own saved starting points and have no agent-facing write path.",
            actor.as_str()
        ))),
        Err(other) => Err(other),
    }
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
    require_recipe_user_actor(&actor)?;
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
    require_recipe_user_actor(&actor)?;
    s.repo.track_recipe_delete(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}
