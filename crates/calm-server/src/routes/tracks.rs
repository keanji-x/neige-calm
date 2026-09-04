//! `/api/tracks`, `/api/areas/:id/tracks` — Track CRUD. **Owned by Track B.**
//!
//! Writes go through `Repo::write_with_event` (via the
//! `write_with_event_typed` ergonomic wrapper). See `routes/areas.rs` for
//! the migration pattern; this file follows the same shape.
//!
//! ## PR6 (#136) — atomic planner-card binding
//!
//! `create_track` now mints a track **and** a `CardRole::Planner` codex card
//! in a single transaction via [`crate::db::write_with_events_typed`].
//! Two events leave the tx: [`Event::TrackUpdated`] (scope = Track) and
//! [`Event::CardAdded`] (scope = Card).
//!
//! ## Planner harness start
//!
//! Track creation now mints the kernel-owned planner card and report card, then
//! submits the `planner-harness-start` operation. Start failures are non-fatal:
//! the committed track remains and the planner card can recover through the
//! harness runtime.
//!
//! ## Track-delete teardown (issue #197)
//!
//! `delete_track` first performs a best-effort descendant preflight and
//! snapshots its teardown-owned resources. It then reaps terminals/harnesses
//! outside SQLite and finishes the row delete in a short transaction whose
//! descendant guard is authoritative. The `terminals.card_id` FK is
//! `ON DELETE RESTRICT` (migration 0011),
//! so a missed cleanup surfaces as a transaction-level error rather
//! than a silent daemon-process leak.

use crate::AREA_CHAT_PURPOSE;
use crate::actor::Actor;
use crate::auth::Principal;
use crate::db::sqlite::{
    MAX_TREE_TASK_BUDGET, TrackRecipeOrigin, TrackWorkspacePlan, area_folder_create_tx,
    area_folders_list_all_tx, card_create_with_id_tx, overlay_delete_by_entity_tx,
    overlay_delete_card_overlays_by_track_tx, overlay_upsert_tx, terminal_delete_tx,
    track_create_tx, track_delete_tx, track_recipe_get_tx, track_update_tx,
};
use crate::db::write_with_actor_events_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::ids::{ActorId, CardId, TrackId};
use crate::model::{
    AreaKind, Card, CardRole, FolderConflict, FolderConflictKind, NewCard, NewOverlay, NewTrack,
    RequestTheme, Track, TrackDetail, TrackPatch, TrackWorkspace, TrackWorkspaceKind,
    TrackWorkspacePatch, new_id,
};
use crate::operation::planner_harness_start_adapter::PlannerHarnessStartOperationPayload;
use crate::operation::workspace_lease::{
    release_workspace_leases_for_track_tx, sweep_workspace_worktrees_for_tracks_repo,
    track_has_active_forge_action,
};
use crate::operation::{OperationKey, OperationOutcome};
use crate::plugin_host::manifest::Manifest;
use crate::report_backlinks;
use crate::routes::area_folders::{find_owner, is_descendant_of, normalize_path};
use crate::routes::cards::interrupt_shared_card_active_turn;
use crate::routes::codex_cards::default_cwd;
use crate::routes::conversations_shared::validate_first_message;
use crate::routes::terminal_cards::stable_payload_hash;
use crate::session_projection_lookup::project_runtime_into_cards_payload;
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};
use crate::templates::{Template, template_by_key};
use crate::terminal_sweeper::reap_terminal_artifacts_with_renderer;
use crate::track_fs_view::{TrackFsContent, TrackFsEntry, TrackFsView};
use crate::track_lifecycle::{track_get_tx, validate_transition};
use crate::track_report::{
    self, ReportBlock, TrackReportPayload, report_blocks_snapshot_tx, resolve_report_for_track,
    tasks_rebuild_tree_tx, tasks_rebuild_tx,
};
use crate::track_report_doc::ReportDoc;
use crate::track_report_read::load_report_read_snapshot;
use crate::validation::CODEX_PAYLOAD_SCHEMA_VERSION;
use crate::workspace_recycle;
use crate::workspace_repoint::{PristineVerdict, workspace_pristine};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[cfg(feature = "fixtures")]
use std::collections::HashMap;
#[cfg(feature = "fixtures")]
use std::sync::{Mutex as StdMutex, OnceLock};
#[cfg(feature = "fixtures")]
use tokio::sync::Notify;

mod fork_guard;

use fork_guard::guard_forked_blocks;

#[derive(Clone)]
struct TrackDeletePlan {
    track_id: TrackId,
    area_id: crate::ids::AreaId,
    cards: Vec<Card>,
    terminals: Vec<crate::model::Terminal>,
    active_runtime_ids: Vec<String>,
}

#[cfg(feature = "fixtures")]
#[derive(Clone)]
pub struct TrackDeleteTeardownHook {
    pub entered: std::sync::Arc<Notify>,
    pub release: std::sync::Arc<Notify>,
}

#[cfg(feature = "fixtures")]
fn track_delete_teardown_hooks() -> &'static StdMutex<HashMap<String, TrackDeleteTeardownHook>> {
    static HOOKS: OnceLock<StdMutex<HashMap<String, TrackDeleteTeardownHook>>> = OnceLock::new();
    HOOKS.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn install_track_delete_teardown_hook_for_test(track_id: &str, hook: TrackDeleteTeardownHook) {
    track_delete_teardown_hooks()
        .lock()
        .expect("track delete hook mutex")
        .insert(track_id.to_string(), hook);
}

async fn wait_at_track_delete_teardown_hook(track_id: &str) {
    #[cfg(feature = "fixtures")]
    {
        let hook = track_delete_teardown_hooks()
            .lock()
            .expect("track delete hook mutex")
            .remove(track_id);
        if let Some(hook) = hook {
            hook.entered.notify_one();
            hook.release.notified().await;
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = track_id;
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTrackRequest {
    #[schema(value_type = String)]
    pub area_id: crate::ids::AreaId,
    /// Issue #1211 — on this user-driven create path the title is no longer
    /// the track's intent, so the client may omit it entirely. Omitting it
    /// stores the **empty string** — there is no server-side default; the
    /// `Untitled track` a user sees in a list is the frontend's display
    /// fallback (`fe/core/domain/track.ts` `UNTITLED_TRACK_LABEL`). The planner
    /// agent then names the track via `calm.track.rename`, which only succeeds
    /// while the stored title is still blank. The type
    /// stays `String`: the empty string has always been a legal title and the
    /// server applies no non-empty validation.
    #[serde(default)]
    #[schema(required = false)]
    pub title: String,
    pub sort: Option<f64>,
    /// Issue #1131 — omitted / null → persist `default_cwd()` (`$HOME`, else
    /// process cwd) on the track row and skip `area_folders`. Present values
    /// (including the empty string) keep the pre-#1131 absolute-path + claim
    /// rules. The SQLite column stays NOT NULL; only the request field is
    /// optional.
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub template_id: Option<String>,
    /// A user-defined recipe (`track_recipes` row, #1292) to start from.
    ///
    /// Deliberately **not** folded into `template_id`. That field's value
    /// lands on `tracks.template_id`, which the track start path later
    /// resolves against running plugins' manifests to recover a bound
    /// template descriptor. A recipe id has no manifest to resolve against,
    /// so putting one there would make every recipe-created track log a
    /// resolution failure while starting — an error record for an entirely
    /// normal situation.
    ///
    /// Supplying both is a 400: two starting points is not a preference to
    /// resolve, it is a request that does not name one thing.
    #[serde(default)]
    pub recipe_id: Option<String>,
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub template_input: Option<serde_json::Value>,
    #[serde(default)]
    pub attach_folder: bool,
    pub theme: RequestTheme,
    /// One-time creation instruction: copy this track's report snapshot into
    /// the new report inside the track-create transaction.
    #[serde(default)]
    pub fork_report_from: Option<String>,
    /// Issue #1299 S1 — the sentence the user typed on the synthesiser page,
    /// delivered to the planner agent **with** this create instead of having to
    /// be retyped after landing on the track.
    ///
    /// It becomes an `Observation::UserMessage` seeded into the harness
    /// snapshot inside the `planner-harness-start` transaction — not a
    /// `TrackGoal`, which is a different semantic slot (see
    /// `PlannerHarnessStartOperationPayload::first_message`). Validated exactly
    /// like `POST /api/cards/{id}/planner/input` (non-blank after trim, at most
    /// 32768 **characters**) and validated before anything is minted.
    ///
    /// Omitting it leaves this endpoint's behaviour byte-for-byte unchanged,
    /// down to the operation payload, whose `first_message` key is
    /// `skip_serializing_if`-omitted.
    ///
    /// Supplying it also changes what a harness-start failure means. Without
    /// it, a create whose `planner-harness-start` operation fails still returns
    /// 201 — "the track exists, its planner agent is inert" is a documented,
    /// recoverable state. With it, that same failure is a 500, because the
    /// sentence the user typed was only ever going to be written by that
    /// operation, so a 201 would claim a delivery the create did not make.
    ///
    /// The 500 does **not** undo the create: the track and its cards are
    /// already committed and nothing compensates for them. Nor does it say the
    /// message was not delivered — that depends on how far the start got, and
    /// this endpoint cannot tell. A start that failed before the harness was
    /// installed handed nothing to any agent; a start that failed *after* it
    /// (the `Stuck` outcome) has already seeded the observation and fired the
    /// turn, and nothing recalls it. So the 500 reports an unknown delivery and
    /// asks the client to look at the track before resending, because resending
    /// a message that did arrive delivers it twice (see the 500 description on
    /// `create_track`). Teaching the endpoint to answer what actually happened
    /// is #1384.
    ///
    /// This slice delivers the message; it does not make the create
    /// **retryable**. A client that retries a create carrying a
    /// `first_message` gets a second track, exactly as a client retrying any
    /// other create always has.
    #[serde(default)]
    pub first_message: Option<String>,
}

impl CreateTrackRequest {
    /// `(body, fork_report_from, recipe_id, cwd_omitted)`. `cwd_omitted` is
    /// true when the client sent no `cwd` / `null`; that is a different branch
    /// from an explicit empty string, which still 400s.
    fn into_parts(self) -> (NewTrack, Option<String>, Option<String>, bool) {
        let cwd_omitted = self.cwd.is_none();
        (
            NewTrack {
                area_id: self.area_id,
                title: self.title,
                sort: self.sort,
                cwd: self.cwd.unwrap_or_else(default_cwd),
                template_id: self.template_id,
                plugin_scope: None,
                template_input: self.template_input,
                attach_folder: if cwd_omitted {
                    false
                } else {
                    self.attach_folder
                },
                theme: self.theme,
            },
            self.fork_report_from,
            self.recipe_id,
            cwd_omitted,
        )
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/tracks", get(list_tracks_window).post(create_track))
        .route(
            "/api/tracks/{id}",
            get(get_track_detail)
                .patch(update_track)
                .delete(delete_track),
        )
        // Issue #247 PR3 — user-facing track-report edit endpoint. Session-
        // authenticated; only `ActorId::User` is accepted (worker / planner /
        // plugin actors are rejected 403 even when carrying a valid
        // session cookie). The MCP `calm.report.{write,edit}` path is
        // unchanged; both paths funnel through the `track_report::write`
        // module — different entry points, one private writer — so the
        // dual-event invariant + CRDT write stays one boundary.
        .route(
            "/api/tracks/{id}/report",
            get(get_track_report).post(update_track_report),
        )
        .route("/api/tracks/{id}/backlinks", get(get_track_backlinks))
        .route("/api/tracks/{id}/files/ls", get(list_track_files))
        .route("/api/tracks/{id}/files/cat", get(cat_track_file))
        .route("/api/areas/{area_id}/tracks", get(list_tracks_by_area))
}

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct TrackFsLsQuery {
    /// Logical path to list. Omitted or `/` lists the track root.
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct TrackFsCatQuery {
    /// Logical path to read. Required.
    pub path: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/tracks/{id}/files/ls",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id"), TrackFsLsQuery),
    responses(
        (status = 200, description = "Track file view directory entries", body = Vec<TrackFsEntry>),
        (status = 400, description = "Logical path not available", body = ErrorBody),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 403, description = "Referenced card is outside the track", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
// NOTE: no `Principal` extractor here.
//
// `update_track_report` (POST) keeps `_principal: Principal` as an implicit
// session-middleware assertion — the route fires on user action, never
// during a11y/replay traffic. These GET routes fire on every track page
// mount (the report sidebar lists root on first render); the replay
// binary intentionally does NOT attach `require_session` so its a11y
// suite can drive REST without a session, and a `Principal` extractor
// here would surface as a 401 → SessionProvider redirect → login page
// during a11y replay runs. The TODO below keeps the multi-user
// ownership hook visible without breaking the no-auth surface contract.
//
// TODO(#573 multi-user): ownership check
pub(crate) async fn list_track_files(
    State(s): State<RouteState>,
    Path(id): Path<String>,
    Query(q): Query<TrackFsLsQuery>,
) -> Result<Json<Vec<TrackFsEntry>>> {
    let track = s
        .repo
        .track_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {id}")))?;
    // TODO(#573 multi-user): ownership check
    let view = TrackFsView::new(s.repo.as_ref(), &s.write);
    let entries = view.ls(&track, q.path.as_deref()).await?;
    Ok(Json(entries))
}

#[utoipa::path(
    get,
    path = "/api/tracks/{id}/files/cat",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id"), TrackFsCatQuery),
    responses(
        (status = 200, description = "Track file view content", body = TrackFsContent),
        (status = 400, description = "Missing path or logical path not available", body = ErrorBody),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 403, description = "Referenced card is outside the track", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
// See note on `list_track_files` for why `Principal` is intentionally NOT
// extracted here. The `TODO(#573 multi-user)` lives next to `list_track_files`.
pub(crate) async fn cat_track_file(
    State(s): State<RouteState>,
    Path(id): Path<String>,
    Query(q): Query<TrackFsCatQuery>,
) -> Result<Json<TrackFsContent>> {
    let path = q
        .path
        .as_deref()
        .ok_or_else(|| CalmError::BadRequest("calm.track.cat: missing `path` (string)".into()))?;
    let track = s
        .repo
        .track_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {id}")))?;
    // TODO(#573 multi-user): ownership check
    let view = TrackFsView::new(s.repo.as_ref(), &s.write);
    let content = view.cat(&track, path).await?;
    Ok(Json(content))
}

#[utoipa::path(
    get,
    path = "/api/areas/{area_id}/tracks",
    tag = "tracks",
    params(("area_id" = String, Path, description = "Area id")),
    responses(
        (status = 200, description = "Tracks under area", body = Vec<Track>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_tracks_by_area(
    State(s): State<RouteState>,
    Path(area_id): Path<String>,
) -> Result<Json<Vec<Track>>> {
    let mut tracks = s.repo.tracks_by_area(&area_id).await?;
    tracks.retain(user_visible_track);
    Ok(Json(tracks))
}

/// Public track lists hide retired Area-conversation containers. Keep this at
/// the route boundary: repository readers such as area deletion and backlink
/// resolution require the complete set.
///
/// #1318 S2 retired the template-overlay half of this filter along with the
/// mechanism that produced it: there is no longer any way to mark a track as a
/// template, so there is nothing left to hide on that account. That half was
/// the only reason the filter needed the repository, and with it gone the
/// `async fn retain_user_visible_tracks(&dyn RepoRead, ..) -> Result<()>`
/// wrapper was a synchronous `Vec::retain` wearing an async fallible
/// signature: it ignored its only parameter and had no failure path, while
/// both call sites still wrote `.await?`. Callers now retain directly
/// (第二轮评审 MINOR-1).
///
/// The `match` is spelled out rather than written `!= Some(AREA_CHAT_PURPOSE)`
/// purely for readability — both forms already keep NULL-purpose tracks
/// visible, because Rust comparison against `Option` is total. The three-valued
/// logic trap this must not be confused with lives in SQL, where
/// `purpose <> 'area-chat'` drops NULL rows; the two hand-written predicates
/// that must spell out `purpose IS NULL OR ...` are in `session_repo_impl.rs`.
fn user_visible_track(track: &Track) -> bool {
    match track.purpose.as_deref() {
        None => true,
        Some(purpose) => purpose != AREA_CHAT_PURPOSE,
    }
}

/// Build the initial report a template instantiates to.
///
/// #1300 — this replaces `ensure_templates` / `lookup_template_track` /
/// `seed_template_track` / `restamp_template_report_if_placeholder`. Those
/// lazily minted three hidden system-area tracks and `POST /api/tracks` then
/// forked one of them, which made a template a kind of track. It is a read-only
/// recipe: instantiating it is structural initialization of a new track, and it
/// reads nothing.
///
/// ## Why this does not go through the report-edit boundary
///
/// The seeding path did, and it had to name an author to do so. It named
/// `EditAuthor::User` for a write no user made — the last production path on
/// which the kernel wrote a report as the user, and the reason #1300 exists.
///
/// Naming the kernel honestly instead was not available: `guard_task_declarations`
/// gives `EditAuthor::Kernel` no permission to author task-declaration blocks
/// at all (`track_report_edit_guard.rs`), and every template report is a page of
/// them. That guard is not an obstacle to route around — refusing to let
/// non-humans declare tasks is its entire purpose.
///
/// So this is not a report *edit* with a better-chosen author; it is the same
/// structural initialization the fork path performs, on the same in-transaction
/// writer, with no author to name because no one is editing anything. That is
/// also why the constants can now declare `planner` directly
/// (`templates::report_from_tasks`) instead of writing `user` and having the
/// fork rewrite it one step later.
///
/// ## The single validation, and why there is not a second one
///
/// [`crate::track_report_guard::validate_body_fences`] is not only a fence-shape
/// check: it runs `validate_payload` over every parseable fence in the body.
/// So one call covers both failure modes a bad recipe constant has — a fence
/// that does not parse (which `split_body` would otherwise demote to prose,
/// silently dropping the task) and a fence that parses but violates its
/// schema.
///
/// An earlier draft added a per-block check beside it, mirroring the two
/// call sites inside `prepare_fork_report`. On this path that is a **vacuous
/// guard**: the blocks come from this same body, so it can reject nothing the
/// whole-body call has not already rejected. Two reviewers independently failed
/// to construct an input that reaches it.
///
/// `Internal`, not `BadRequest`: every byte here comes from a Rust constant and
/// no caller can influence it, so a failure is a kernel defect rather than a
/// bad request. (`prepare_fork_report` answers `BadRequest` for the same checks
/// because its input is another track's user content.)
fn prepare_template_report(key: &str) -> Result<InitialReportSnapshot> {
    let template = template_by_key(key)
        .ok_or_else(|| CalmError::Internal(format!("track create: unknown template `{key}`")))?;
    compile_template(template)
}

/// Compile one **built-in roster** entry: recipe bytes in, validated report
/// plus task declarations out.
///
/// #1321 S3 — the one compiler for the roster half. Two callers reach it:
/// `POST /api/tracks` through [`prepare_template_report`], and
/// `GET /api/track-templates` (`routes::track_templates::current_definition`),
/// which projects the picker's task list off the result rather than re-parsing
/// the rendered body. Method for "two": `grep -rn "compile_template(" crates/`
/// returns three lines — this definition and those two calls — and `crates/`
/// holds every workspace member, so a Rust caller cannot be outside it.
///
/// This governs the roster half only. User-authored recipes (#1292) are
/// validated at their write boundary in `routes::track_recipes`, which answers
/// `BadRequest` — a user's bad body is a bad request, while a roster recipe
/// that does not compile is a kernel defect and stays `Internal`. Both
/// eventually run the same [`prepare_initial_report_payload`] core; what
/// differs is which failures each side can produce and how it answers them.
pub(super) fn compile_template(template: &Template) -> Result<InitialReportSnapshot> {
    prepare_initial_report_payload(template.key(), template.recipe())
}

/// The recipe-to-snapshot core, taking the payload rather than the key.
///
/// Production reaches this only through [`prepare_template_report`], so it is
/// not a test-only entrance: it is where the work happens, and the key lookup
/// is the thin part. Splitting them this way is what lets the "a corrupt recipe
/// is refused" cases feed a deliberately broken body — `prepare_template_report`
/// takes a key, and there is no key for a body that no constant produces.
fn prepare_initial_report_payload(
    label: &str,
    payload: TrackReportPayload,
) -> Result<InitialReportSnapshot> {
    crate::track_report_guard::validate_body_fences(&payload.body).map_err(|error| {
        CalmError::Internal(format!("track create: template `{label}` body: {error}"))
    })?;
    let doc = ReportDoc::from_payload(&payload);
    let blocks = doc.blocks_snapshot().map_err(|error| {
        CalmError::Internal(format!("track create: template `{label}` blocks: {error}"))
    })?;
    let (summary, body) = doc.project().map_err(|error| {
        CalmError::Internal(format!("track create: project template `{label}`: {error}"))
    })?;
    let (declarations, diagnostics) =
        calm_types::report_blocks::tasks::project_task_declarations(&blocks);
    let mut prepared = TrackReportPayload::new(summary, body);
    prepared.blocks = Some(blocks);
    Ok(InitialReportSnapshot {
        payload: prepared,
        doc,
        declarations,
        diagnostics,
    })
}

/// Issue #250 PR 2 — calendar window query parameters for
/// `GET /api/tracks`. Every field is optional so omitting all three
/// degenerates to "every track in the DB" (the route delegates to
/// `Repo::tracks_window` which builds the SQL `WHERE` clause from the
/// non-`None` subset).
///
/// The semantic for `since` + `until` is **inclusive at both
/// endpoints**:
///   * `created_at <= until`  — exclude tracks that hadn't been created
///     yet by the right edge of the window.
///   * `terminal_at IS NULL OR terminal_at >= since` — include any
///     track that's still open (never reached a terminal lifecycle
///     state) or whose terminal stamp lands inside / past the left
///     edge.
///
/// Together the two predicates implement the "the track is visible on
/// at least one day inside `[since, until]`" calendar contract from
/// the issue, even when the track hasn't terminated yet.
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct TracksWindowQuery {
    /// Lower bound (inclusive) in unix milliseconds. Track is included
    /// when `terminal_at IS NULL OR terminal_at >= since`. Omitting
    /// disables the lower-bound filter.
    pub since: Option<i64>,
    /// Upper bound (inclusive) in unix milliseconds. Track is included
    /// when `created_at <= until`. Omitting disables the upper-bound
    /// filter.
    pub until: Option<i64>,
    /// Optional per-area filter. Mirrors `list_tracks_by_area` for
    /// callers that want one area's window in a single endpoint.
    pub area_id: Option<String>,
}

/// Issue #250 PR 2 — calendar / dashboard window query.
///
/// `GET /api/tracks?since=<ms>&until=<ms>&area_id=<id>` — every
/// parameter is optional. Returns the full track row (so the frontend
/// can render lifecycle / area / terminal-at without an N+1 detail
/// fetch). Pre-#250 callers that hit `GET /api/tracks` would 405 on
/// the old `POST`-only route; this is an additive contract.
#[utoipa::path(
    get,
    path = "/api/tracks",
    tag = "tracks",
    params(TracksWindowQuery),
    responses(
        (status = 200, description = "Tracks overlapping the window, sorted by created_at", body = Vec<Track>),
        (status = 400, description = "Inverted window (since > until)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_tracks_window(
    State(state): State<RouteState>,
    Query(q): Query<TracksWindowQuery>,
) -> Result<Json<Vec<Track>>> {
    if let (Some(since), Some(until)) = (q.since, q.until)
        && since > until
    {
        return Err(CalmError::BadRequest(format!(
            "window query: `since` ({since}) must be <= `until` ({until})"
        )));
    }
    let mut tracks = state
        .repo
        .tracks_window(q.area_id.as_deref(), q.since, q.until)
        .await?;
    tracks.retain(user_visible_track);
    Ok(Json(tracks))
}

#[utoipa::path(
    get,
    path = "/api/tracks/{id}",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id")),
    responses(
        (status = 200, description = "Track detail (track + its cards + overlays)", body = TrackDetail),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_track_detail(
    State(s): State<RouteState>,
    Path(id): Path<String>,
) -> Result<Json<TrackDetail>> {
    let mut detail = s
        .repo
        .track_detail(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {id}")))?;
    // Tier A read-side guard (issue #198 concern 4) — mirror `list_overlays`
    // so kernel-owned overlay rows with a `schemaVersion` past what this
    // binary supports never reach the frontend through the track detail
    // route. This is the primary path the frontend uses to render
    // status/progress/eta/now overlays for a track (`adaptTrack(detail.track,
    // detail.overlays)` in `web/src/app/router.tsx`); without this filter a
    // future-version row written by a newer kernel binary would defeat the
    // PR #214 guard for the track-rendering path while still being correctly
    // filtered from `GET /api/overlays`. PR #214 review follow-up.
    detail.overlays = crate::routes::overlays::filter_unsupported_overlay_versions(detail.overlays);
    project_runtime_into_cards_payload(s.repo.as_ref(), &mut detail.cards).await?;
    Ok(Json(detail))
}

#[utoipa::path(
    post,
    path = "/api/tracks",
    tag = "tracks",
    request_body = CreateTrackRequest,
    responses(
        (status = 201, description = "Track created. With `first_message`, the message is also queued for the planner agent inside the harness-start transaction.", body = Track),
        (status = 400, description = "Malformed create (bad `cwd`, unknown `template_id`, invalid `template_input`), or — with `first_message` — an empty or over-long message. Decided before anything is minted.", body = ErrorBody),
        (status = 500, description = "Internal error. One case leaves the track behind: when the request carried a `first_message` and the planner harness start did not complete, the track, its cards and its workspace are already committed, and whether the message reached the agent is **unknown to the server** — depending on how far the start got, it may never have been handed over, or it may already have been delivered and answered. Nothing is rolled back, nothing compensates, and the create is not retryable — read the track back from `GET /api/tracks` and look before resending, because resending a message that did arrive delivers it twice. Without `first_message` the same harness failure is logged and still returns 201, because no user text was riding on it.", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn create_track(
    State(s): State<RouteState>,
    actor: Actor,
    Json(mut request): Json<CreateTrackRequest>,
) -> Result<Response> {
    // #1299 S1 — first, before every other check in this handler and therefore
    // before every mint it can reach. A rejected first message (blank or
    // over-long) must leave no track, no cards, no folder claim and no
    // materialized workspace behind, and this handler's own comment below
    // explains why "non-201 ⇒ no side effect" is otherwise not one of its
    // properties.
    //
    // `validate_first_message` is the conversation route's function, called not
    // restated: a message this endpoint accepts is delivered through the same
    // `Observation::UserMessage` slot `POST /api/cards/{id}/planner/input`
    // writes, so one ceiling has to govern both or one of them accepts what the
    // other later refuses.
    //
    // There is no create shape this endpoint accepts that skips the harness:
    // since #1318 S2 retired `as_template`, `create_track_with_planner_harness`
    // calls `start_planner_harness` unconditionally, so a `template_id` or
    // `recipe_id` create delivers the message exactly like a bare one. Pinned by
    // `track_create_first_message::a_template_create_delivers_the_first_message`
    // and `…::a_first_message_is_delivered_once_on_a_recipe_create`.
    let first_message = request.first_message.take();
    if let Some(text) = first_message.as_deref() {
        validate_first_message(text)?;
    }
    let (mut p, fork_report_from, recipe_id, cwd_omitted) = request.into_parts();

    // #1292 — two starting points is not a preference to resolve, it is a
    // request that does not name one thing. Refused here, before any other
    // work, so the `init` match below can treat the combination as
    // unreachable rather than silently picking a winner.
    if p.template_id.is_some() && recipe_id.is_some() {
        return Err(CalmError::BadRequest(
            "track create: give `template_id` or `recipe_id`, not both".into(),
        ));
    }
    // PR6 (#136) — track create now atomically mints a `CardRole::Planner`
    // codex card alongside the track row. Both rows commit in one tx
    // and both `Event::TrackUpdated` + `Event::CardAdded` envelopes
    // emit from the same commit, each tagged with its own scope so
    // per-track and per-card subscribers each see the relevant frame
    // without re-routing through ancestors.
    //
    // Issue #250 PR 2 — the body may carry `cwd` (the track's working
    // directory) and `attach_folder`. When `cwd` is present, it is the
    // source of truth for the planner daemon's working directory and must
    // either resolve to the body's `area_id` via the existing folder
    // claims, or — when `attach_folder = true` — get atomically claimed
    // as a new folder under that area inside the same tx that mints the
    // track row.
    //
    // Issue #1131 — when the client omits `cwd` (new FE title-only
    // create), persist `default_cwd()` and skip the claim scan entirely.
    // Legacy clients that still send `cwd` keep the #250 rules.

    // 0. Validate cwd up front before opening the tx. The route owns
    //    every cross-area correctness check so the inner writer
    //    (`track_create_tx`) stays a pure mechanical row insert. Order:
    //    omitted-cwd default → absolute-path shape → normalize →
    //    existing-claim resolution → optional folder attach.
    //
    //    #1209 — what "short-circuits before any DB write" actually covers.
    //    Every 4xx this handler can decide *before opening the transaction*
    //    (cwd shape, attached-workspace validation, area 404, unknown
    //    template, the `template_input` binding matrix) lands before any DB
    //    write.
    //
    //    #1300 rewrote the rest of this paragraph, which described a world
    //    with a separately-committing template seed in it. There is no such
    //    commit any more: template initialization is structural work inside
    //    the create transaction (`TrackInit::Template` →
    //    `prepare_template_report`, in the closure `create_track_structure`
    //    runs). So the folder-claim 409, the in-transaction 400s for an
    //    explicit `fork_report_from` (source missing / cross-area) and the
    //    in-transaction 500s now all roll the *whole* create back, template
    //    report included — there is nothing left behind for them to leave.
    //
    //    One failure still is not covered by that rollback, and it is the
    //    reason "non-201 ⇒ no side effect" is not a property of this handler:
    //    `materialize_workspace` runs *after* the transaction commits (the
    //    managed path is derived from the track id) and returns non-2xx with
    //    the track already persisted. Pinned by
    //    `materialize_failure_fails_the_create`.

    // #1209 — one lookup. The template is the concept; a plugin binding is an
    // attribute of it, not a second way in. Roster membership is the whole
    // admission test: whether some plugin claims the id, and whether that
    // plugin is running and trusted, cannot change the answer.
    let admission = match p.template_id.as_deref() {
        Some(template_id) => Some(admit_template(&s, template_id).await.ok_or_else(|| {
            CalmError::BadRequest(format!(
                "track create: `template_id` must reference a known track template; got `{template_id}`"
            ))
        })?),
        None => None,
    };
    // #1318 S2 — the stored `template_id` is the roster's key, not the
    // caller's string. All three consumers of an admitted id now read the same
    // value: the recipe lookup already did (`TrackInit::Template { key }`), the
    // plugin binding does since `admit_template` resolves it from the roster
    // entry, and the track row does from here on. Under today's exact-match
    // `template_by_key` the two strings are equal, so this is not a
    // behaviour change yet — it is the line that keeps them from diverging
    // the moment admission stops being exact (case folding, aliases), which
    // is precisely when a row carrying `"SMALL-CHANGE"` for roster key
    // `"small-change"` would start meaning something different to every
    // later reader of the column.
    if let Some(admission) = admission.as_ref() {
        p.template_id = Some(admission.key().to_string());
    }
    // The binding is read off the admitted template; the route no longer digs
    // through the registry a second time.
    let bound_plugin = admission.as_ref().and_then(|a| a.binding.as_ref());
    // #891 / #1110 S2 — `template_input` is only accepted against a bound
    // template whose owning plugin Manifest declares an `input_schema`;
    // validated here, before any DB write, so the inner writer persists
    // the blob verbatim. Still requires `template_id` this slice
    // (S5 deletes the template entity).
    //
    // 第二轮评审 NIT-3 — `bound_plugin` is `None` for two different reasons and
    // the 400 has to name the right one: no `template_id` at all, or an
    // admitted `template_id` whose owning plugin is not running ∧ trusted right
    // now. (An unknown `template_id` cannot reach here — `admit_template`
    // above already 400s.)
    let owner = match bound_plugin {
        Some(manifest) => crate::plugin_host::template_input::TemplateInputOwner::Plugin(manifest),
        None if p.template_id.is_some() => {
            crate::plugin_host::template_input::TemplateInputOwner::NoBoundPlugin
        }
        None => crate::plugin_host::template_input::TemplateInputOwner::NoTemplateId,
    };
    validate_template_input_binding(owner, p.template_input.as_ref())?;
    // #1110 S4 — copy the owning plugin id into `plugin_scope` in the same
    // insert. Unbound create leaves it None. Not a request field.
    p.plugin_scope = bound_plugin.map(|manifest| manifest.id.clone());

    // Issue #1131 — omitted / null cwd is a new branch *before* the
    // user-area claim scan (same spirit as the system-area exemption
    // below): store HOME, force attach_folder=false, do not insert a
    // area_folders row. Never claim `$HOME` — longest-prefix would
    // poison every other area. An *explicit* `cwd: "$HOME"` with
    // `attach_folder: false` still 409s when unclaimed; only omission
    // takes this branch.
    if !cwd_omitted && !p.cwd.starts_with('/') {
        return Err(CalmError::BadRequest(format!(
            "track create: `cwd` must be absolute (start with `/`); got `{}`",
            p.cwd
        )));
    }
    let normalized_cwd = normalize_path(&p.cwd);
    // #1147 S3 — design D3: "Attached 创建只做校验：绝对路径、目录存在、是 Git
    // 仓库". Until this slice only the first third existed, which was
    // survivable while the new FE had no way to attach anything; this slice
    // adds that way, so the gap closes with it.
    //
    // **Before the transaction, deliberately.** `materialize_workspace` runs
    // *after* the track transaction commits (the managed path needs the track
    // id), and `materialize_failure_fails_the_create` pins the consequence: a
    // failure there leaves an orphan track row behind. Validating an attached
    // target needs none of that ordering — the path came in the request — so
    // it happens here, where the answer is a 400 and no row exists at all.
    // `materialize_workspace` checks it again as the single contract point for
    // every other create entry.
    if !cwd_omitted {
        crate::workspace_materialize::validate_attached_workspace(std::path::Path::new(
            &normalized_cwd,
        ))?;
    }
    // Stamp the normalized cwd back onto the body before the track row
    // is minted — the `area_folder.path` we may attach below is also
    // the normalized form, so storing them in the same shape keeps
    // future "resolve by exact cwd" lookups simple.
    p.cwd = normalized_cwd.clone();

    // Issue #250 PR 2 fix — system area (kernel-internal scaffolding,
    // hosts the default Today terminal's track) is exempt from the
    // area_folders claim namespace. The user can't reach it through
    // any user-facing surface, and claiming a path under it (e.g. the
    // initial `/` placeholder useTodayTerminal used) would poison
    // every real area's descendant check. Look up the kind once here;
    // if System, skip both the pre-tx folder validation and the
    // in-tx attach. The cwd is still recorded on the track row (the
    // planner daemon chdirs into it) but no `area_folders` row is minted.
    let area = s
        .repo
        .area_get(p.area_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("area `{}`", p.area_id)))?;
    let is_system_area = area.kind == AreaKind::System;
    if is_system_area {
        p.attach_folder = false;
    }

    let attach_folder = p.attach_folder;
    let body_area_id = p.area_id.as_str().to_string();

    // Issue #275 — the whole cwd-vs-claim decision (covering-folder scan,
    // reverse-overlap check, and the INSERT when `attach_folder`) now runs
    // inside the track-create transaction. It used to be a pre-tx scan on a
    // separate pooled connection, which let two concurrent creates for
    // `/a` and `/a/b` both pass an empty-table scan and commit overlapping
    // claims — `UNIQUE(area_folders.path)` only rejects *equal* paths.
    // Overlapping rows made the two resolvers disagree, and the track
    // create then 409'd on an area the UI had just been told to use.
    //
    // The tx is already `BEGIN IMMEDIATE` (see
    // `SqlxRepo::write_with_actor_events`), so this adds one SELECT under
    // a writer lock the create was taking anyway — it does not widen the
    // lock window with any new I/O.
    let conflict = FolderConflictSlot::default();
    let folder_claim = if is_system_area || cwd_omitted {
        FolderClaim::Skip
    } else {
        FolderClaim::Enforce {
            attach: attach_folder,
            conflict: conflict.clone(),
        }
    };

    // #1300 — the report's source, as one value.
    //
    // An explicit `fork_report_from` still wins over `template_id`; that
    // priority is unchanged and pinned by
    // `explicit_fork_report_from_is_not_overwritten`. What changed is what the
    // losing branch costs: before #1300 a `template_id` unconditionally seeded
    // three hidden system-area tracks *first* (`ensure_templates`) and only then
    // consulted `fork_report_from`, so the combination wrote rows it then did
    // not use. Instantiating a recipe reads nothing and writes nothing outside
    // the create transaction.
    //
    // #1209 placed the seed here — after the cwd shape check, the
    // attached-workspace check and the area 404 — so none of those 4xx left
    // freshly minted tracks behind. Nothing is minted here any more, so that
    // ordering constraint is gone with the seeding it constrained.
    // #1292 — a recipe is a third source, resolved at the same priority as a
    // built-in template. An explicit `fork_report_from` still wins over both.
    let init = match (&admission, recipe_id, fork_report_from) {
        // Both a built-in and a recipe. The guard at the top of this function
        // already refused this combination before any work, so reaching here
        // means that guard was removed or bypassed. That is a reason to return
        // the same 400 — not to panic: the exclusivity is a property of *this*
        // match's inputs, and it should hold locally instead of depending on a
        // caller ~180 lines up that nothing mechanically ties to this arm.
        //
        // This arm sits *before* the fork arm, and matches `_` on
        // `fork_report_from`, on purpose: a request naming two starting points
        // is ambiguous whether or not it also asks for a fork, and ambiguity is
        // not something a priority rule gets to resolve. Ordering it after the
        // fork arm would let `template_id + recipe_id + fork_report_from`
        // silently take the fork path and swallow the contradiction — exactly
        // the hole this fallback exists to close.
        (Some(_), Some(_), _) => {
            return Err(CalmError::BadRequest(
                "track create: give `template_id` or `recipe_id`, not both".into(),
            ));
        }
        (_, _, Some(source_track_id)) => TrackInit::Fork { source_track_id },
        (Some(admission), None, None) => TrackInit::Template {
            key: admission.key(),
        },
        (None, Some(recipe_id), None) => TrackInit::Recipe { recipe_id },
        (None, None, None) => TrackInit::Blank,
    };

    let workspace_root = s.workspace_root.clone();
    let created = create_track_with_planner_harness(
        s,
        actor,
        p,
        first_message,
        CreateTrackOptions {
            folder_claim,
            body_area_id,
            normalized_cwd,
            init,
            // #1147 S2 — omitting `cwd` (the #1131 title-only create, i.e. what
            // the new FE sends) is the managed-default branch: the server picks
            // the directory. An explicit `cwd` is the attached branch and keeps
            // the #250 claim rules above verbatim.
            workspace_plan: if cwd_omitted {
                TrackWorkspacePlan::ManagedUnder(workspace_root)
            } else {
                TrackWorkspacePlan::AttachedFromCwd
            },
        },
    )
    .await;
    match created {
        Err(error) => match conflict.take() {
            Some(body) => Ok((StatusCode::CONFLICT, Json(body)).into_response()),
            None => Err(error),
        },
        ok => ok,
    }
}

/// #1209 — the single answer to "may this id create a track", plus the optional
/// plugin binding that comes with it.
///
/// The word *admission* is the point: this answers **admission**, not "what
/// does the template look like". The authority for the latter is the roster
/// entry's own `build_recipe` (`templates::Template::recipe`), a Rust constant
/// — which is why there is no
/// `title` and no report here. (#1300: before S2 the authority was a seeded
/// system-area template track found by a database lookup, and this sentence
/// named it. Both the track and the lookup are gone.)
pub(crate) struct TemplateAdmission {
    /// The admitted roster entry itself — **not** a key copied off it.
    ///
    /// #1318 S2 (第二轮评审 MAJOR-2) — this used to be a `pub key: &'static
    /// str` that `admit_template` assigned from `template.key`. Assignment is
    /// a discipline, and a discipline is exactly what the second review round
    /// broke: `key: if id == template.key { template.key } else {
    /// String::leak(id.to_string()) }` compiled, reflected the caller's
    /// spelling for every id that was *not* byte-identical to the roster's,
    /// and left the whole suite green — because the one test guarding this
    /// feeds a byte-identical fixture and so only ever exercises the other
    /// branch.
    ///
    /// Holding the borrow removes the assignment site. There is no longer a
    /// place to write a key, conditionally or otherwise; [`Self::key`] reads
    /// one out of the roster entry. Together with [`Template`]'s private
    /// fields — which make a forged entry `E0451` in this module — "the
    /// admission's key is the roster's" stops being a property a test has to
    /// chase and becomes the only thing the types can express.
    template: &'static Template,
    /// The owning plugin, when a running trusted one claims this id. `None` is
    /// an ordinary template, not a rejection.
    pub binding: Option<Manifest>,
}

impl TemplateAdmission {
    /// The roster's own `&'static` key, not the caller's string.
    ///
    /// It reaches **all three** consumers of an admitted id:
    ///
    ///   * the **recipe lookup** (`templates::template_by_key`, then
    ///     `Template::recipe`) inside the create transaction, via
    ///     `TrackInit::Template { key }`;
    ///   * the **track row**, since #1318 S2: `create_track` overwrites
    ///     `NewTrack::template_id` with `admission.key()` before the insert, so
    ///     `tracks.template_id` stores the roster's spelling;
    ///   * the **plugin binding**, also since #1318 S2 (第一轮评审 F5):
    ///     [`resolve_template_binding`] takes a `&'static Template` rather than
    ///     the caller's string, so `binding` — and therefore `plugin_scope` and
    ///     the `template_input` schema check — is decided by the same spelling
    ///     the other two read, and can no longer be handed anything else.
    ///
    /// All three now read the *same borrow*, [`Self::template`], rather than
    /// three copies of a value someone assigned.
    ///
    /// #1318 S2 (第三轮评审) — state precisely what that buys, because the
    /// previous wording ("a future case-folding or aliasing admission rule
    /// cannot hand an unnormalized key to any of them, since outside
    /// `crate::templates` no such value can be built") claimed more than the
    /// types deliver. What is closed is the **value of the key**: this struct
    /// has no key assignment site, and in safe Rust outside
    /// `crate::templates`'s subtree a `Template` carrying the caller's spelling
    /// is `E0451`. What is **not** closed is any *decision* an admission rule
    /// makes — `id` is necessarily in [`admit_template`]'s scope, so a
    /// conditional on it needs no forged `Template` at all. See the
    /// `## KNOWN GAPS` block on [`admit_template`].
    ///
    /// The third bullet is not decoration. Leaving the binding on the caller's
    /// string would have re-created, between a different pair of readers, the
    /// exact divergence the second bullet closes: under a case-folding
    /// admission rule a `"SMALL-CHANGE"` create would resolve `binding = None`
    /// (exact match against the manifest's `"small-change"` descriptor fails)
    /// and store `plugin_scope = NULL`, while the planner harness's
    /// `bound_template` — which reads the *row's* `template_id`, now normalized
    /// — would match. Creation-time and run-time would disagree about which
    /// plugin owns the track.
    ///
    /// Until #1318 S2 the second bullet read the opposite way:
    /// `CreateTrackRequest::into_parts` put the caller's original string on
    /// `NewTrack` and that is what landed in the column. The two spellings are
    /// identical only because `template_by_key` is an exact match today, so the
    /// overwrite changes no stored value yet — but the very rule this field
    /// guards against would have separated them, storing `"SMALL-CHANGE"` on a
    /// row whose report was instantiated from roster key `"small-change"`.
    pub(crate) fn key(&self) -> &'static str {
        self.template.key()
    }
}

/// Admit a caller-supplied `template_id`.
///
/// Roster membership is the only admission test; the binding is resolved
/// afterwards purely to be carried along. There is deliberately no fallback
/// arm here: a running trusted plugin declaring an id the roster does not have
/// gets `None`, i.e. a 400. That is the whole of #1209 — see §5 of
/// `docs/architecture/1209-template-workflow-unify.md` for why the alternative
/// (admitting it as a report-less pseudo-template) was rejected.
///
/// The binding is resolved from the admitted roster entry, **not** from `id`:
/// [`resolve_template_binding`] takes a `&'static Template`, so the *argument*
/// it receives cannot be the caller's spelling. Under today's exact-match
/// `template_by_key` the two strings are byte-identical on every input that
/// reaches this line, so this is not a behaviour change.
///
/// #1318 S2 (第二轮评审 MAJOR-2) — the admission carries the roster borrow, so
/// this function has no key to assign. The previous shape did, and a one-line
/// conditional there reflected the caller's spelling with the suite still
/// green.
///
/// # KNOWN GAPS
///
/// #1318 S2 (第三轮评审) — three review-built constructions, each independently
/// compiled or run, show that "the third consumer is closed by the type" is
/// **not** true, and the retraction is registered here rather than chased with
/// a fourth round of hardening. The root reason they all share: `id` is
/// necessarily in this function's scope, and no type design prevents a
/// conditional on a value that is in scope.
///
/// The threat model these gaps sit under is **unintentional drift** — the next
/// person adding case-folding or aliasing and not noticing they split creation
/// time from run time. It is not an adversary deliberately hiding a forgery;
/// against that, none of this is a control at all. (Same posture as
/// `scripts/report_write_boundary.sh`'s header in S1.)
///
/// **Gap 1 — a conditional on `id`, safe Rust, nothing forged.** `template` and
/// `binding` are two independent field initializers below, and `id` is live for
/// both. A future case-insensitive lookup admits this:
///
/// ```ignore
/// binding: if id == template.key() { resolve_template_binding(s, template).await } else { None }
/// ```
///
/// A `"SMALL-CHANGE"` create would then store the canonical key and the
/// canonical recipe but `plugin_scope = NULL`, while `planner_harness_start_adapter`
/// later finds the plugin from the row's canonical key — creation time and run
/// time disagreeing about who owns the track, which is exactly what
/// [`TemplateAdmission::key`]'s third bullet exists to prevent. Every existing
/// test passes only canonical spellings, so all of them stay green.
///
/// **Gap 2 — a second entry point inside `crate::templates`, safe Rust; measured
/// 68 passed, 0 failed.** `templates::tests::template_by_key_returns_the_rosters_own_borrow`
/// guards `template_by_key`'s return path, not the module. A channel added
/// `templates::template_admit`, a case-insensitive find that leaks a rebuilt
/// `Template` when the spelling differs, repointed this function at it, and ran
/// `nextest -E 'test(admission) or test(template)'`: **68/68 green**, with the
/// caller's spelling reaching all three consumers.
///
/// **Gap 3 — `transmute`; measured `clippy -D warnings` clean.** The crate has
/// no `#![forbid(unsafe_code)]` (`calm-server/src` already contains a dozen-odd
/// `unsafe` blocks) and `transmute` does not consult field visibility, so a
/// leaked `&'static (&'static str, &'static str)` can be reinterpreted as a
/// `&'static Template` here. It depends on `repr(Rust)`'s unspecified layout,
/// so it is not a sound program — but the retracted claim was about what the
/// **compiler** admits, and the compiler and the lint gate both admit it.
///
/// ## Why there is no bad path today
///
/// Observed, not inferred: `template_by_key` is an exact `==` match, so an
/// admitted id is byte-equal to a roster key; both surviving consumers of
/// [`TemplateAdmission::key`] read the roster borrow; and an un-normalized key
/// that somehow reached the create transaction would not silently mis-seed —
/// `templates::template_by_key`'s exact match returns `None`, so
/// [`prepare_template_report`] raises `CalmError::Internal` and the create
/// fails loudly instead.
///
/// That is a statement about **today's code**, not an impossibility proof. The
/// day `template_by_key` stops being an exact match, all three gaps above become
/// live, and nothing in the type system or the test suite will say so.
pub(crate) async fn admit_template(s: &RouteState, id: &str) -> Option<TemplateAdmission> {
    let template = template_by_key(id)?;
    Some(TemplateAdmission {
        template,
        binding: resolve_template_binding(s, template).await,
    })
}

/// Resolve an admitted roster [`Template`] to the owning plugin Manifest iff a
/// running **trusted** plugin registers its key — same filter as
/// `bound_template_descriptor` on the planner harness side. `None` covers
/// stopped and untrusted templates alike (the route deliberately does not
/// distinguish them in the 400).
///
/// #1318 S2 (第一轮评审 F5) — the parameter is the roster entry, not a
/// `&str`. It used to take the caller's `template_id` string, which was the
/// last consumer of an admitted id still reading the caller's spelling: with
/// `tracks.template_id` now normalized to `admission.key()`, a future
/// case-folding or aliasing admission rule would have made creation-time
/// binding (`plugin_scope`, `template_input` acceptance) disagree with the
/// planner harness's run-time `bound_template`, which reads the normalized
/// column.
///
/// #1318 S2 (第二轮评审 MAJOR) — this paragraph used to end "a `&'static
/// Template` can only come from `TEMPLATES`, so the divergence is closed at
/// compile time", and the F4 test cited that sentence to excuse itself from
/// covering this consumer. **The sentence was false when it was written.**
/// [`Template`]'s fields were `pub`, so `Box::leak(Box::new(Template { key:
/// String::leak(id.to_string()), title: template.title }))` compiled here and
/// produced a `&'static Template` carrying the caller's spelling; two review
/// channels independently built it and the suite stayed green.
///
/// That *particular expression* no longer compiles, for a checkable reason
/// rather than by assertion: `Template`'s fields are private with no
/// constructor, so it is `E0451` in this module — and outside
/// `crate::templates` and its descendant modules generally — and the only
/// `&'static Template` this file can name in safe Rust is a borrow of a roster
/// entry.
///
/// #1318 S2 (第三轮评审) — that is the whole of the claim, and it is narrower
/// than it reads. Three qualifications, all of them registered under
/// `## KNOWN GAPS` on [`admit_template`]:
///
///   * *safe* Rust only — `transmute` ignores field visibility and the crate
///     does not `forbid(unsafe_code)`;
///   * inside `crate::templates` (and its descendants) the forgery is still
///     expressible, which is why
///     `templates::tests::template_by_key_returns_the_rosters_own_borrow` is
///     not redundant with it — though that test guards `template_by_key`'s
///     return path, not the module, and a second entry point beside it went
///     68/68 green;
///   * and most importantly, it says nothing about a divergence built without
///     any forged `Template` at all — a conditional on `id` in
///     [`admit_template`], which is where the caller's spelling actually still
///     lives.
pub(crate) async fn resolve_template_binding(
    s: &RouteState,
    template: &'static Template,
) -> Option<Manifest> {
    let running_plugin_ids = s.plugin.running_plugin_ids().await;
    s.plugin.registry().list().into_iter().find(|manifest| {
        // #1321 S1 — "may this plugin own a template" has one definition,
        // shared with the per-track resolver that later re-checks the owner
        // this line picks. Restating it here is how the two ends of the
        // binding drifted apart in the first place.
        crate::track_binding::plugin_is_eligible_owner(&running_plugin_ids, &manifest.id)
            && manifest
                .templates
                .iter()
                .any(|descriptor| descriptor.id == template.key())
    })
}

/// #1321 S1 — create-time `template_input` validation now *is* the shared
/// judgement: the matrix moved to
/// [`crate::plugin_host::template_input::validate_template_input_binding`] so
/// the run-time owner-binding re-check calls the same function instead of
/// restating a subset of it. This wrapper adds nothing but the route's error
/// vocabulary, which is what keeps every pre-existing 400 body
/// byte-identical.
fn validate_template_input_binding(
    owner: crate::plugin_host::template_input::TemplateInputOwner<'_>,
    input: Option<&serde_json::Value>,
) -> Result<()> {
    crate::plugin_host::template_input::validate_template_input_binding(owner, input)
        .map_err(|reason| CalmError::BadRequest(format!("track create: {reason}")))
}

/// Issue #275 — the cwd claim scan runs **inside** the track-create
/// transaction, so its structured 409 (`FolderConflict`, not the generic
/// `{error, code}` envelope) has to travel back out through `Err`. The
/// closure parks the body here; [`create_track`] picks it up and renders
/// it. `Mutex` is only ever locked between `await` points.
#[derive(Clone, Default)]
struct FolderConflictSlot(std::sync::Arc<std::sync::Mutex<Option<FolderConflict>>>);

impl FolderConflictSlot {
    /// Park `body` and return the error that unwinds (and rolls back)
    /// the transaction. The message is a fallback only: the route reads
    /// the slot first and never surfaces this string.
    fn park(&self, body: FolderConflict) -> CalmError {
        let message = format!(
            "track create: cwd conflicts with folder claim `{}` (area `{}`)",
            body.conflict_path, body.area_id
        );
        *self.0.lock().expect("folder conflict slot poisoned") = Some(body);
        CalmError::Conflict(message)
    }

    fn take(&self) -> Option<FolderConflict> {
        self.0.lock().expect("folder conflict slot poisoned").take()
    }
}

/// Issue #275 — what the track-create transaction does about `area_folders`.
#[derive(Clone)]
enum FolderClaim {
    /// Don't scan, don't insert. The system area is exempt from the claim
    /// namespace entirely.
    Skip,
    /// Scan inside the track tx (`BEGIN IMMEDIATE`, so scan and insert are
    /// atomic against a concurrent claim) and act on the result:
    /// `attach` mints the claim when nothing covers the cwd; without it a
    /// cwd no area claims is refused rather than making a homeless track.
    Enforce {
        attach: bool,
        conflict: FolderConflictSlot,
    },
}

/// Which route is asking, purely so the refusal message names it. The RULES do
/// not vary — that is the point of there being one function.
#[derive(Clone, Copy)]
enum FolderClaimIntent {
    Create,
    /// #1147 S3 — `PATCH /api/tracks/{id}` pointing a track at an existing
    /// repository.
    Repoint,
}

impl FolderClaimIntent {
    fn label(self) -> &'static str {
        match self {
            FolderClaimIntent::Create => "track create",
            FolderClaimIntent::Repoint => "track workspace",
        }
    }
}

/// #1147 S3 — whether this pass may actually mint a `area_folders` row.
///
/// The re-point runs the claim rules **twice**, and the first pass must not
/// write. Its transaction commits (it is also the fence), so a claim minted
/// there survives a later refusal — and the re-point can still refuse after it,
/// on the pre-move re-check. That would leave the caller a 409 plus a claim
/// they never got a track for, in a route whose whole promise is "a refusal
/// changes nothing".
///
/// Measured, not reasoned about: with the first pass allowed to mint, deleting
/// the authoritative second pass turned **no test red** — because the claim was
/// already in the table. That is what surfaced this.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FolderClaimPass {
    /// Report the same conflicts, write nothing. Fail-fast only.
    ScanOnly,
    /// Report conflicts AND mint the claim. Must share a transaction with the
    /// write it authorises.
    Authoritative,
}

/// Issue #275's claim rules, in one place.
///
/// Extracted verbatim from `create_track_structure` by #1147 S3 so that pointing
/// an existing track at a directory obeys exactly the same rules as creating one
/// there. A second copy would be a second set of rules the moment either is
/// touched, and the invariant these enforce — *at most one claim covers any
/// path* — is not one that survives two implementations.
///
/// Must run first in its transaction: every branch either rolls the tx back or
/// leaves the claim table consistent for the write that follows.
async fn enforce_folder_claim_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    claim: &FolderClaim,
    area_id: &str,
    normalized_cwd: &str,
    intent: FolderClaimIntent,
    pass: FolderClaimPass,
) -> Result<()> {
    let FolderClaim::Enforce { attach, conflict } = claim else {
        return Ok(());
    };
    let existing = area_folders_list_all_tx(tx).await?;
    match find_owner(&existing, normalized_cwd) {
        // Some other area already covers this cwd. `Descendant` is the right
        // label from the cwd's point of view: the cwd is a descendant of an
        // existing folder owned by another area.
        Some(f) if f.area_id.as_str() != area_id => Err(conflict.park(FolderConflict {
            folder_id: f.id,
            area_id: f.area_id.clone(),
            conflict_path: f.path.clone(),
            conflict_kind: FolderConflictKind::Descendant,
        })),
        // Same area already covers it — `attach_folder` is a no-op.
        //
        // #275 behavior change. Before that fix the insert ran unconditionally
        // on the scan result, so this arm fell through into
        // `area_folder_create_tx`:
        //   - cwd == the existing claim → UNIQUE(path) → 409 for re-claiming
        //     your own folder;
        //   - cwd under the existing claim → a second, overlapping row, minted
        //     from plain HTTP with no concurrency at all.
        // The latter is the larger hole in the "at most one claim covers any
        // path" invariant — bigger and far easier to reach than the
        // scan/insert TOCTOU. Pinned by `post_api_tracks_attach_folder_*` in
        // `tests/cases/track_cwd_terminal_at.rs`.
        Some(_) => Ok(()),
        None if *attach => {
            // No claim covers the cwd and the caller wants to mint one. Check
            // the *reverse* overlap first: an existing folder that is a
            // descendant of the proposed cwd (`/a/b` exists, claim `/a`).
            // Refused for the same reason the area_folders route refuses it —
            // silently widening a narrower claim would make resolution
            // ambiguous.
            if let Some(f) = existing
                .iter()
                .find(|f| is_descendant_of(normalized_cwd, &f.path))
            {
                return Err(conflict.park(FolderConflict {
                    folder_id: f.id,
                    area_id: f.area_id.clone(),
                    conflict_path: f.path.clone(),
                    conflict_kind: FolderConflictKind::Ancestor,
                }));
            }
            if pass == FolderClaimPass::Authoritative {
                area_folder_create_tx(tx, area_id, normalized_cwd).await?;
            }
            Ok(())
        }
        // Nothing covers the cwd and the caller didn't opt in to attach.
        // Refuse so accidentally typing a stray path doesn't create a
        // "homeless" track.
        None => Err(CalmError::Conflict(format!(
            "{}: cwd `{normalized_cwd}` is not claimed by any area. Set \
             `attach_folder: true` to claim it for area `{area_id}`.",
            intent.label()
        ))),
    }
}

/// Where a new track's report comes from.
///
/// #1300 — this used to be `fork_report_from: Option<String>`, and "create from
/// a template" was expressed *through* it: the route lazily seeded three hidden
/// system-area tracks and then forked one of them. That made a template a kind
/// of track, which is the thing #1300 removes. A template is a read-only recipe;
/// instantiating it is structural initialization of a new track, not a copy of
/// an existing one.
///
/// The two data-carrying variants deliberately stay distinct rather than
/// collapsing into "some report snapshot". They share the *mechanism* below —
/// `prepare_*` produces a snapshot, one in-transaction writer persists it and
/// projects the tasks — but not the semantics: `Fork` copies a live track and
/// must rewrite its links and re-attribute its blocks, while `Template`
/// constructs from a constant that has no track to rewrite links against.
enum TrackInit {
    /// No report content; the track keeps the default skeleton.
    Blank,
    /// Instantiate a template recipe. The roster's own `&'static` key, never
    /// the caller's string — see [`TemplateAdmission::key`].
    Template { key: &'static str },
    /// Instantiate a **user-defined** recipe (`track_recipes` row, #1292).
    ///
    /// Distinct from [`TrackInit::Template`] rather than folded into it,
    /// because the two resolve from different places and only one of them
    /// can fail at runtime: a built-in key is a `&'static` borrow out of the
    /// roster and its payload is a Rust constant, while this one is a row
    /// that may have been deleted between the picker's read and this create.
    /// Collapsing them would make the infallible case carry the fallible
    /// one's error paths.
    Recipe { recipe_id: String },
    /// Copy an existing track's report.
    Fork { source_track_id: String },
}

struct CreateTrackOptions {
    folder_claim: FolderClaim,
    body_area_id: String,
    normalized_cwd: String,
    init: TrackInit,
    /// #1147 S2 — managed (server allocates under the workspace root) vs
    /// attached (the caller pointed at an existing directory). Decided by
    /// each create entry point; `create_track_structure` materializes the
    /// managed case right after the transaction commits.
    workspace_plan: TrackWorkspacePlan,
}

#[allow(deprecated)]
async fn create_track_with_planner_harness(
    s: RouteState,
    actor: Actor,
    p: NewTrack,
    // #1299 S1 — travels to `start_planner_harness` and nowhere else. The
    // create transaction never sees it: the message is delivered by the
    // `planner-harness-start` operation submitted after that transaction
    // commits, which is why an in-transaction refusal (an unknown `recipe_id`,
    // say) rolls the whole create back and leaves no operation carrying it.
    first_message: Option<String>,
    options: CreateTrackOptions,
) -> Result<Response> {
    let (track, _, planner_card_id, report_card_id) =
        create_track_structure(s.clone(), actor.clone(), p, options).await?;
    start_planner_harness(
        &s,
        &actor,
        &track,
        planner_card_id,
        report_card_id,
        first_message,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(track)).into_response())
}

#[allow(deprecated)]
async fn create_track_structure(
    s: RouteState,
    actor: Actor,
    p: NewTrack,
    options: CreateTrackOptions,
) -> Result<(Track, bool, String, String)> {
    let CreateTrackOptions {
        folder_claim,
        body_area_id,
        normalized_cwd,
        init,
        workspace_plan,
    } = options;
    // #1147 — captured before `s` is moved into the write closure. Only the
    // managed branch uses it; `materialize_workspace` ignores it for attached.
    let workspace_root_for_materialize = s.workspace_root.clone();
    let planner_card_id = new_id();
    let report_card_id = new_id();
    let actor_id = actor.to_actor_id();
    let actor_id_for_tx = actor_id.clone();
    let write_for_tx = s.write.clone();
    let planner_card_id_for_tx = planner_card_id.clone();
    let report_card_id_for_tx = report_card_id.clone();
    let area_id_for_attach = body_area_id;
    let normalized_cwd_for_tx = normalized_cwd;
    // #1115 — the fork path deliberately derives no `EditAuthor`. It used to
    // (`User` when no `X-Calm-Actor` header was present, `Planner` otherwise) and
    // hand it to `fork_guard::guard_forked_blocks`, which made that guard a
    // no-op for the browser fork — the single most common fork there is. The
    // fork's normalization and its belt are both author-independent now, so
    // nothing here may classify the caller.
    let ((track, created), _event_ids) = write_with_actor_events_typed(
        s.repo.as_ref(),
        None,
        &s.events,
        &s.write,
        move |tx| {
            Box::pin(async move {
                // #275 — claim scan + claim insert, atomic with the track
                // row because they share this BEGIN IMMEDIATE tx. Must
                // stay first: every branch below either rolls the tx back
                // or leaves the claim table consistent for `track_create_tx`.
                enforce_folder_claim_tx(
                    tx,
                    &folder_claim,
                    &area_id_for_attach,
                    &normalized_cwd_for_tx,
                    FolderClaimIntent::Create,
                    FolderClaimPass::Authoritative,
                )
                .await?;

                // #1292 S2/S3 — the recipe is read here, *before* the track
                // row, and read once.
                //
                // Inside this transaction, like `Fork` and unlike `Template`:
                // the row can be edited or deleted concurrently, so the create
                // must see one consistent version of it rather than one read
                // before the tx and a different reality inside.
                //
                // Before the insert rather than alongside the report snapshot
                // below, because S3 stamps the recipe's `revision` onto the
                // track row itself and that value has to be in hand when the
                // INSERT runs. Reading it twice — once for provenance, once for
                // the report — would let a concurrent edit land between the two
                // and produce a track whose recorded revision does not describe
                // the report it actually got.
                let recipe_source = match &init {
                    TrackInit::Recipe { recipe_id } => {
                        Some(track_recipe_get_tx(tx, recipe_id).await?.ok_or_else(|| {
                            CalmError::BadRequest(format!(
                                "track create: recipe `{recipe_id}` does not exist"
                            ))
                        })?)
                    }
                    _ => None,
                };
                let recipe_origin = recipe_source.as_ref().map(|recipe| TrackRecipeOrigin {
                    recipe_id: recipe.id.clone(),
                    revision: recipe.revision,
                });

                let track = track_create_tx(
                    tx,
                    p,
                    None,
                    &workspace_plan,
                    recipe_origin.as_ref(),
                    write_for_tx.area_cache(),
                )
                .await?;
                let track_id = track.id.clone();
                let area_id = track.area_id.clone();

                // #1300 — three initialization sources, one persistence
                // mechanism. `Template` builds from a constant and needs no
                // database read; `Fork` reads the source track inside this same
                // transaction, exactly as before.
                //
                // Matched on `(&init, recipe_source)` as one value so the
                // `Recipe` arm binds its recipe by pattern. The read has to
                // happen above (its `revision` is needed before the INSERT, and
                // reading twice would let a concurrent edit split the recorded
                // revision from the report), which leaves two places that both
                // depend on `init` being `Recipe`. Pairing them in the scrutinee
                // is what keeps the dependency visible here instead of resting
                // on an `expect` that reads as unconditional.
                let init_snapshot = match (&init, recipe_source) {
                    (TrackInit::Blank, _) => None,
                    (TrackInit::Template { key }, _) => Some(prepare_template_report(key)?),
                    (TrackInit::Recipe { recipe_id }, None) => {
                        // The read above is driven by the same `init`, so this
                        // arm needs the read to have been skipped on the very
                        // value that selects it. Not a caller error, so not a
                        // 400.
                        return Err(CalmError::Internal(format!(
                            "track create: recipe `{recipe_id}` was resolved to a Recipe init \
                             without the recipe row the same `init` was supposed to read"
                        )));
                    }
                    (TrackInit::Recipe { recipe_id }, Some(recipe)) => {
                        // The stored body is already normalized — the write
                        // boundary did it (`routes::track_recipes`). Nothing is
                        // re-normalized here, which is what makes "what the
                        // picker shows" and "what create produces" the same
                        // bytes rather than two transforms that must agree.
                        Some(prepare_initial_report_payload(
                            recipe_id,
                            TrackReportPayload::new(recipe.title, recipe.body),
                        )?)
                    }
                    (TrackInit::Fork { source_track_id }, _) => {
                    // #1292 S3 — a fork records no recipe provenance, and that
                    // holds even when the fork source was itself recipe-born or
                    // when this very request also named a `recipe_id` (that
                    // combination resolves to the fork, the same way
                    // `template_id` + `fork_report_from` does — see
                    // `explicit_fork_report_from_is_not_overwritten`), which is
                    // why the `_` here is a decision rather than a leftover.
                    //
                    // `child_track_adapter` refuses to pass provenance down
                    // because "a recipe id here would claim the child carries
                    // content it never got". A fork of a recipe-born track *did*
                    // get that content, so that argument does not carry over and
                    // the reason has to be a different one: `recipe_id` /
                    // `recipe_revision` name the recipe this track was
                    // instantiated from, and a fork was instantiated from a
                    // track. Copying the id here would assert a direct
                    // instantiation that never happened, and would go on
                    // asserting it after the fork's report is edited away from
                    // the recipe's content.
                    //
                    // The cost is real and is not being hidden: the `tracks` row
                    // this arm creates records neither the recipe nor the source
                    // track — no column on it names either, and this arm writes
                    // no fork edge anywhere else. Where a fork came from is a
                    // gap in *fork* provenance; it is a different column than
                    // this one, and stamping a recipe id the track was not
                    // instantiated from would not close it.
                    //
                    // Pinned by `a_fork_of_a_recipe_born_track_has_no_provenance`.
                    let source_track_id = source_track_id.as_str();
                    let source_id = TrackId::from(source_track_id.to_string());
                    let source_track = track_get_tx(tx, &source_id).await.map_err(|error| {
                        if matches!(error, CalmError::NotFound(_)) {
                            CalmError::BadRequest(format!(
                                "track create: fork source track `{source_track_id}` does not exist"
                            ))
                        } else {
                            error
                        }
                    })?;
                    let source_area_kind: String =
                        sqlx::query_scalar("SELECT kind FROM areas WHERE id=?1")
                            .bind(source_track.area_id.as_str())
                            .fetch_one(&mut **tx)
                            .await?;
                    if source_track.area_id != area_id
                        && source_area_kind != AreaKind::System.as_db_str()
                    {
                        return Err(CalmError::BadRequest(format!(
                            "track create: fork source track `{source_track_id}` must be in the target area or the system area"
                        )));
                    }
                    let (summary, blocks) =
                        report_blocks_snapshot_tx(tx, source_track_id).await?;
                    Some(prepare_fork_report(
                        summary,
                        blocks,
                        source_track_id,
                        track_id.as_str(),
                    )?)
                    }
                };

                let planner_card = card_create_with_id_tx(
                    tx,
                    planner_card_id_for_tx.clone(),
                    NewCard {
                        title: None,
                        track_id: track_id.clone(),
                        kind: "codex".into(),
                        sort: None,
                        // #1211 S1: on this user-driven create path the track
                        // title is no longer the track's intent, so create
                        // seeds no `prompt` here. The parameter stays because
                        // child tracks still pass the task goal their parent
                        // planner declared (`operation/child_track_adapter.rs`) —
                        // that is machine-written intent, not a title a human
                        // typed, and it is what seeds the child's harness when
                        // the child track starts.
                        payload: planner_harness_card_payload(None),
                    },
                    CardRole::Planner,
                    false,
                    write_for_tx.role_cache(),
                )
                .await?;

                let report_payload =
                    serde_json::to_value(TrackReportPayload::initial()).map_err(|e| {
                        CalmError::Internal(format!(
                            "track_create: serialize track-report payload: {e}"
                        ))
                    })?;
                let mut report_card = card_create_with_id_tx(
                    tx,
                    report_card_id_for_tx.clone(),
                    NewCard {
                        title: None,
                        track_id: track_id.clone(),
                        kind: "track-report".into(),
                        sort: Some(-1.0),
                        payload: report_payload,
                    },
                    CardRole::ReportCard,
                    false,
                    write_for_tx.role_cache(),
                )
                .await?;

                let mut init_projection = None;
                if let Some(InitialReportSnapshot {
                    payload,
                    mut doc,
                    declarations,
                    diagnostics,
                }) = init_snapshot
                {
                    // #1252 S2 — the structural door of the report write
                    // boundary. It takes no author, no actor, no event bus and
                    // no CAS input, so neither of the two things this closure
                    // must not do is expressible from here: it cannot emit a
                    // `track.report_edited` (the report card's only event is
                    // the `CardAdded` below) and it cannot reach
                    // `guard_task_declarations` (#1115 — there is no author to
                    // hand it). It is not event-free, though: the projection it
                    // returns is what the `plan.updated` further down is built
                    // from, and its `kernel_events` leg is refused inside the
                    // door itself rather than published from here (#1252 R1/F3).
                    // The fork's own release belt stays upstream in
                    // `prepare_fork_report`, next to the normalization it
                    // belts, so `TrackInit::Template` does not acquire it.
                    let (persisted_report, projection) =
                        crate::track_report::write::structural_init_report_tx(
                            tx,
                            crate::track_report::write::InitialReportTarget {
                                report_card_id: report_card.id.as_str(),
                                track_id: track_id.as_str(),
                                payload: &payload,
                                doc: &mut doc,
                                declarations: &declarations,
                                diagnostics: &diagnostics,
                            },
                        )
                        .await?;
                    report_card = persisted_report;
                    init_projection = Some(projection);
                }

                let track_scope = EventScope::Track {
                    track: track_id.clone(),
                    area: area_id.clone(),
                };
                let planner_card_scope = EventScope::Card {
                    card: planner_card.id.clone(),
                    track: track_id.clone(),
                    area: area_id.clone(),
                };
                let report_card_scope = EventScope::Card {
                    card: report_card.id.clone(),
                    track: track_id.clone(),
                    area: area_id.clone(),
                };
                let layout_overlay = overlay_upsert_tx(
                    tx,
                    NewOverlay {
                        plugin_id: "kernel".into(),
                        entity_kind: "view".into(),
                        entity_id: track_id.as_str().to_string(),
                        kind: "layout".into(),
                        payload: planner_harness_layout_payload(
                            planner_card.id.as_str(),
                            report_card.id.as_str(),
                        ),
                    },
                )
                .await?;
                let mut events = vec![
                    (
                        actor_id_for_tx.clone(),
                        track_scope.clone(),
                        Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
                            track.clone(),
                            None,
                        )),
                    ),
                    (
                        actor_id_for_tx.clone(),
                        planner_card_scope,
                        Event::CardAdded(planner_card),
                    ),
                    (
                        actor_id_for_tx.clone(),
                        report_card_scope,
                        Event::CardAdded(report_card),
                    ),
                    (
                        actor_id_for_tx.clone(),
                        track_scope,
                        Event::OverlaySet(layout_overlay),
                    ),
                ];
                if let Some(projection) = init_projection {
                    if !projection.changed_keys.is_empty() {
                        events.push((
                            actor_id_for_tx.clone(),
                            EventScope::Track {
                                track: track_id.clone(),
                                area: area_id.clone(),
                            },
                            Event::PlanUpdated {
                                track_id,
                                changed_keys: projection.changed_keys,
                                agent_message: None,
                            },
                        ));
                    }
                    events.extend(projection.kernel_events);
                }
                Ok(((track, true), events))
            })
        },
    )
    .await?;

    // #1147 S2 (design D3/D5) — materialize outside the transaction and
    // before the planner harness starts. `Attached` is a no-op: the directory is
    // the user's and the server never creates or `git init`s it.
    //
    // A failure here MUST surface as a non-2xx. The tempting shape is
    // `tracing::warn!` + `Ok(())` (as `start_planner_harness` below does for a
    // different, recoverable failure) — but that returns 201 for a track whose
    // first codex worker will then die with `spawn-failed`, which is #1147
    // itself replayed one layer down.
    crate::workspace_materialize::materialize_workspace(
        &track.workspace,
        &workspace_root_for_materialize,
        track.id.as_str(),
    )
    .map_err(|error| {
        tracing::error!(
            track_id = %track.id,
            path = %track.workspace.path,
            error = %error,
            "track create: workspace materialization failed"
        );
        error
    })?;

    Ok((track, created, planner_card_id, report_card_id))
}

async fn start_planner_harness(
    s: &RouteState,
    actor: &Actor,
    track: &Track,
    planner_card_id: String,
    report_card_id: String,
    first_message: Option<String>,
) -> Result<()> {
    // #1299 S1 — captured before `first_message` moves into the payload. This
    // is the entire switch between the two failure semantics below, and it is
    // read exactly once, after the match, so all four failure branches share
    // one decision rather than four copies of it.
    let carries_first_message = first_message.is_some();
    // #1211 S1: no goal is seeded on this user-driven create path. An omitted
    // title is stored as the empty string (`Untitled track` is only what the
    // frontend shows for a blank one) and the planner agent names the track once
    // it knows what the work is, so there is nothing here that could stand in
    // for the user's intent. Child tracks do NOT come through here — they start their
    // harness with the parent planner's declared task goal
    // (`scheduler/mod.rs`, `operation/child_track_adapter.rs`).
    let request = PlannerHarnessStartOperationPayload {
        actor: actor.to_actor_id(),
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(planner_card_id.clone()),
        report_card_id: Some(report_card_id),
        sort: None,
        cwd: track.workspace.path.clone(),
        goal: None,
        reset_harness_items: false,
        force_new_thread: false,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
        // #1299 S1 — `None` here is not "no message", it is the pre-#1299 shape
        // verbatim: `skip_serializing_if` drops the key entirely, so a create
        // that typed nothing writes byte-identical payload JSON and therefore
        // the same `payload_hash` an older binary would have written.
        first_message,
    };
    let op_payload = serde_json::to_value(&request)?;
    let payload_hash = stable_payload_hash(&serde_json::json!({
        "actor": actor.as_str(),
        "request": &request,
    }))?;
    // #1299 S1 — `Some(reason)` iff the harness did not start. Every one of the
    // four ways that can happen (submit rejected, wait errored, the operation
    // reached `Failed`, the operation reached `Stuck`) writes it, so the
    // decision after the match is reached from all four and cannot be true of
    // only the one that happened to be edited. The `warn!` lines are unchanged
    // and still fire on every failure regardless of `first_message`: they are
    // what the create-without-a-message path has always emitted.
    let mut harness_start_failed: Option<String> = None;
    match s
        .operation_runtime
        .submit(
            "planner-harness-start",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash,
            },
            op_payload,
        )
        .await
    {
        Ok(op_id) => match s.operation_runtime.wait(&op_id).await {
            Ok(result) => match result.outcome {
                // `SucceededViaCollision` is unreachable from this call site
                // today, and folding it in with `Succeeded` is only correct
                // while that holds. Two independent reasons it holds: this
                // path submits `idempotency_key: None`, and
                // `find_by_idempotency_key` returns `None` without looking at
                // the table when the key is absent, so `submit` never takes
                // its collision short-circuit; and the sole producer of the
                // variant (`operation_result_from`) needs a persisted
                // `phase_detail.completion == "idempotency_collision"`, which
                // nothing in this repository writes.
                //
                // #1384 changes the first reason. The moment a create carries
                // an `Idempotency-Key` that reaches this `OperationKey`, a
                // repeated create resolves to the FIRST operation and comes
                // back here as `SucceededViaCollision` — i.e. "the message was
                // delivered, by an earlier request, not by this one". Treating
                // that as `Succeeded` then answers 201 for a create that
                // delivered nothing. Split the arm in that slice, do not
                // inherit it.
                OperationOutcome::Succeeded { .. }
                | OperationOutcome::SucceededViaCollision { .. } => {}
                OperationOutcome::Failed {
                    last_error,
                    from_phase,
                    ..
                } => {
                    tracing::warn!(
                        planner_card_id,
                        track_id = %track.id,
                        ?from_phase,
                        error = %last_error,
                        "planner harness start operation failed; track created but planner agent is inert"
                    );
                    harness_start_failed =
                        Some(format!("operation failed in {from_phase:?}: {last_error}"));
                }
                OperationOutcome::Stuck { reason, from_phase } => {
                    tracing::warn!(
                        planner_card_id,
                        track_id = %track.id,
                        ?from_phase,
                        reason,
                        "planner harness start operation stuck; track created but planner agent is inert"
                    );
                    harness_start_failed =
                        Some(format!("operation stuck in {from_phase:?}: {reason}"));
                }
            },
            Err(e) => {
                tracing::warn!(
                    planner_card_id,
                    track_id = %track.id,
                    error = %e,
                    "planner harness start wait failed; track created but planner agent may be inert"
                );
                harness_start_failed = Some(format!("waiting for the operation failed: {e}"));
            }
        },
        Err(e) => {
            tracing::warn!(
                planner_card_id,
                track_id = %track.id,
                error = %e,
                "planner harness start submission failed; track created but planner agent is inert"
            );
            harness_start_failed = Some(format!("submitting the operation failed: {e}"));
        }
    }

    // #1299 S1 — a create that carried a `first_message` promised to deliver
    // it. The message is only ever written by the `planner-harness-start`
    // operation, so if that operation did not run to success the create did not
    // keep that promise, and a 201 would claim it did. Report the failure
    // instead.
    //
    // What this 5xx does NOT say: it does not say nothing happened. The track,
    // its two cards, its folder claim and (on the managed path) its
    // materialized workspace are all already committed by the time this
    // function runs — `create_track_structure` returned before it was called.
    // "non-201 ⇒ no side effect" is not a property of this handler and this
    // branch does not make it one; there is deliberately no compensating
    // delete here. Making the create *retryable* — an idempotency key that
    // survives the id mint — is #1384, not this slice.
    //
    // Without a `first_message` this is unreachable: the pre-#1299 semantics
    // stay byte-for-byte, `warn!` + 201, i.e. "the track exists and its planner
    // agent is inert", which is a documented and recoverable state because
    // nothing the user said was riding on that operation.
    if let Some(reason) = harness_start_failed
        && carries_first_message
    {
        return Err(CalmError::Internal(format!(
            "track create: the track was created but its planner harness start did not complete, \
             so the server cannot tell whether the first message reached the agent ({reason}). \
             Nothing is rolled back — the track, its cards and its workspace are already \
             committed. Open the track and check before sending the message again; if it did \
             arrive, sending it again delivers it twice."
        )));
    }

    Ok(())
}

/// The compiled starting report a create instantiates, before it is persisted.
///
/// #1321 S3 — a named struct rather than the 4-tuple this used to be. The
/// producers ([`prepare_initial_report_payload`], [`prepare_fork_report`]) and
/// its single production consumer (`structural_init_report_tx`, called from
/// `create_track_structure`; the unit tests below read it too) all name the
/// same four things, and a tuple made
/// `.2` vs `.3` — declarations vs diagnostics, both `Vec`s — a positional
/// question.
pub(super) struct InitialReportSnapshot {
    payload: TrackReportPayload,
    doc: ReportDoc,
    declarations: Vec<calm_types::report_blocks::tasks::TaskDeclaration>,
    diagnostics: Vec<Vec<calm_types::report_blocks::tasks::Diagnostic>>,
}

impl InitialReportSnapshot {
    /// The compiled `task` blocks' payloads, in document order.
    ///
    /// Read off [`Self::payload`]'s blocks — the ones
    /// [`prepare_initial_report_payload`] took from `ReportDoc`, after
    /// `validate_body_fences` accepted the body — so a fence this returns is a
    /// fence that parsed and passed its schema. It is not a second parse of the
    /// body: `GET /api/track-templates` used to run one (`split_body` →
    /// `parse_fence` → `filter_map`), where a fence that failed to parse was
    /// silently demoted to prose and its task disappeared from the picker.
    ///
    /// `None` blocks is `Internal`, not an empty list: both construction sites
    /// set them (`prepare_initial_report_payload` and `prepare_fork_report` —
    /// the two `Ok(InitialReportSnapshot { .. })` this file contains), and in
    /// safe Rust there can be no third anywhere else, because every field here
    /// is private, so a struct literal outside `routes::tracks` is `E0451`.
    /// Absence is therefore a defect rather than "this report has no tasks".
    pub(super) fn task_block_payloads(&self) -> Result<Vec<&serde_json::Value>> {
        let blocks = self.payload.blocks.as_ref().ok_or_else(|| {
            CalmError::Internal(
                "compiled initial report carries no blocks snapshot to project".to_string(),
            )
        })?;
        Ok(blocks
            .iter()
            .filter(|block| block.kind == calm_types::report_blocks::KIND_TASK)
            .map(|block| &block.payload)
            .collect())
    }
}

// #1252 S2 — `persist_initial_report_and_project_tasks_tx` used to live here.
// It was the second production caller of `card_update_with_crdt_tx`, i.e. the
// thing that made "the create paths write the report row outside the write
// boundary" true. It is now `track_report::write::structural_init_report_tx`,
// the boundary's structural door, and the row write plus the task projection —
// which have to stay in that order — are the private
// `write_report_row_and_project_tx` that door shares with `write::persist`.

fn prepare_fork_report(
    summary: String,
    mut blocks: Vec<ReportBlock>,
    source_track_id: &str,
    target_track_id: &str,
) -> Result<InitialReportSnapshot> {
    use std::collections::HashSet;

    use calm_types::report_blocks::{KIND_PROSE, KIND_TASK, flat_text, validate_payload};
    use calm_types::report_links::{
        UnsafeTrackLink, rewrite_track_destination, rewrite_track_links,
    };

    let copied_block_ids: HashSet<String> = blocks.iter().map(|block| block.id.clone()).collect();
    let mut unsafe_links: Vec<(String, &'static str, UnsafeTrackLink)> = Vec::new();
    for block in &mut blocks {
        let block_id = block.id.clone();
        if block.kind == KIND_PROSE {
            if let Some(markdown) = block.payload.get_mut("markdown")
                && let Some(source) = markdown.as_str()
            {
                match rewrite_track_links(
                    source,
                    source_track_id,
                    target_track_id,
                    &copied_block_ids,
                ) {
                    Ok(rewritten) => *markdown = serde_json::Value::String(rewritten),
                    Err(errors) => unsafe_links.extend(
                        errors
                            .into_iter()
                            .map(|error| (block_id.clone(), "markdown", error)),
                    ),
                }
            }
            // #1252 S0b — this arm `continue`s past the `validate_payload`
            // call at the bottom of the loop, so before #1252 a prose block
            // carrying a malformed ```neige-block fence forked through
            // verbatim and landed as prose in the target track.
            //
            // What `validate_body_fences` actually covers today (#1252 R1/F3
            // corrects an earlier "every other write end" claim here, which
            // was false at the time): its production call sites are
            // `track_report::apply_report_op`'s two whole-body arms —
            // `ReportDocOp::Replace` and `::WriteMarkdown` — plus this fork
            // exit. The `UpsertBlock` arms, which this note used to record
            // as an open *op-layer* gap, are covered since #1269 and its
            // follow-up by a *different* check, `track_report_guard::
            // validate_block_content`, which branches on the op's `kind`:
            // for prose it forbids any `neige-block` fence at all (stricter
            // than here — other fences, a ```rust code block say, still
            // land), and for a data kind whose content is a canonical fence
            // it schema-validates that fence's payload.
            // `ReportDoc::upsert_block` on its own only parses the fence and
            // matches its kind (`if kind != KIND_PROSE`), which is why both
            // cases have to be checked in the op arm. To be exact about the
            // reach of the *prose* gap that was closed: only a direct
            // `apply_report_op` call exercises it — no user request can,
            // because the MCP (#971) and REST (#990) block surfaces are the
            // only production builders of a prose `UpsertBlock` and both
            // refuse fenced prose at their own argument. (The same holds
            // for the non-prose half: the fourth `UpsertBlock` builder,
            // the task-delete rewrite, is server-synthesized and does not
            // reach that check at all — see `track_report_guard`'s module
            // doc, which enumerates all four construction sites.) And
            // "fenced prose" here means a fence
            // carried whole in one block; on the residual that a fence split
            // across two prose blocks still assembles in the projection, see
            // `track_report_guard::validate_block_content`.
            //
            // Deliberately only the fence check here: the fork exit does not
            // additionally run `validate_payload` on the prose block's own
            // `{"markdown": …}` payload — that is a separate behaviour
            // change. Nor is this the stricter prose rule the op layer and
            // the block surfaces apply; tightening fork to refuse
            // well-formed fences too would reject already-persisted source
            // tracks, so it stays at "malformed / schema-invalid".
            if let Some(markdown) = block.payload.get("markdown").and_then(|v| v.as_str()) {
                crate::track_report_guard::validate_body_fences(markdown).map_err(|error| {
                    CalmError::BadRequest(format!(
                        "track create: invalid forked report block {block_id}: {error}"
                    ))
                })?;
            }
            continue;
        }

        if block.kind == KIND_TASK
            && let Some(payload) = block.payload.as_object_mut()
        {
            for field in ["goal", "acceptance"] {
                if let Some(value) = payload.get_mut(field)
                    && let Some(source) = value.as_str()
                {
                    match rewrite_track_links(
                        source,
                        source_track_id,
                        target_track_id,
                        &copied_block_ids,
                    ) {
                        Ok(rewritten) => *value = serde_json::Value::String(rewritten),
                        Err(errors) => unsafe_links.extend(
                            errors
                                .into_iter()
                                .map(|error| (block_id.clone(), field, error)),
                        ),
                    }
                }
            }
            if let Some(serde_json::Value::Array(references)) = payload.get_mut("refs") {
                for reference in references {
                    if let Some(source) = reference.as_str() {
                        *reference = serde_json::Value::String(rewrite_track_destination(
                            source,
                            source_track_id,
                            target_track_id,
                            &copied_block_ids,
                        ));
                    }
                }
            }
            // #1292 — the three privilege fields, normalized by the one
            // function the recipe write path also calls. The long-form
            // rationale for each field (and for why `released_by_user` is
            // *removed* rather than written `false`) moved to
            // `crate::task_privilege::normalize_task_privilege_fields` with
            // the code; this call site is byte-for-byte equivalent to the
            // inline block it replaces.
            crate::task_privilege::normalize_task_privilege_fields(payload);
        }

        validate_payload(&block.kind, &block.payload).map_err(|error| {
            CalmError::BadRequest(format!(
                "track create: invalid forked report block {}: {error}",
                block.id
            ))
        })?;
    }

    if !unsafe_links.is_empty() {
        let details = unsafe_links
            .into_iter()
            .map(|(block_id, field, link)| {
                format!(
                    "- block {block_id} field {field}: destination source `{}` (decoded `{}`)",
                    link.source, link.decoded_destination
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(CalmError::BadRequest(format!(
            "track create: cannot safely rewrite fork link destinations:\n{details}\n\
             Write each link target in plain form (without character entities or backslash escapes, and without inline HTML in its label) and retry."
        )));
    }

    guard_forked_blocks(&blocks)?;
    let doc = ReportDoc::from_blocks_exact(&summary, &blocks).map_err(|error| {
        CalmError::BadRequest(format!(
            "track create: invalid fork report snapshot: {error}"
        ))
    })?;
    let (summary, body) = doc.project().map_err(|error| {
        CalmError::Internal(format!("track create: project fork report CRDT: {error}"))
    })?;
    let (declarations, diagnostics) =
        calm_types::report_blocks::tasks::project_task_declarations(&blocks);
    let mut payload = TrackReportPayload::new(summary, body);
    payload.blocks = Some(blocks);
    debug_assert_eq!(
        payload.body,
        payload
            .blocks
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(flat_text)
            .collect::<String>()
    );
    Ok(InitialReportSnapshot {
        payload,
        doc,
        declarations,
        diagnostics,
    })
}

/// The payload production writes on a planner-harness card.
///
/// `pub` rather than `pub(crate)` so integration fixtures that seed a planner card
/// row directly can mint the production shape instead of re-typing a partial
/// literal: `{"schemaVersion": 1}` alone drops `codex_source` and
/// `planner_harness`, and a future backend reader of either key would then find
/// the fixture silently unlike production (#1189 review F2).
pub fn planner_harness_card_payload(goal: Option<String>) -> serde_json::Value {
    let mut card_payload = serde_json::Map::new();
    card_payload.insert(
        "schemaVersion".into(),
        serde_json::Value::from(CODEX_PAYLOAD_SCHEMA_VERSION),
    );
    card_payload.insert(
        "codex_source".into(),
        serde_json::Value::String("shared".into()),
    );
    card_payload.insert("planner_harness".into(), serde_json::Value::Bool(true));
    if let Some(goal) = goal.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        card_payload.insert("prompt".into(), serde_json::Value::String(goal.to_string()));
    }
    serde_json::Value::Object(card_payload)
}

pub(crate) fn planner_harness_layout_payload(
    planner_card_id: &str,
    report_card_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1,
        "positions": {
            planner_card_id: {
                "x": 0, "y": 0, "w": 6, "h": 12
            },
            report_card_id: {
                "x": 6, "y": 0, "w": 6, "h": 12
            }
        }
    })
}

// ---------------------------------------------------------------------------
// #1147 S3 — changing a track's workspace
// ---------------------------------------------------------------------------

/// Test seam for the ONE timing predicate in this design.
///
/// Fires in the exact window design §更换与冻结 step 2 exists to close: after
/// the fence transaction has committed, before the pre-move re-check. A test
/// installs a hook here, writes into the workspace when it is signalled, and
/// then requires the re-point to answer 409. Delete the re-check and that test
/// goes red — which is the point, because no static assertion about the
/// database can stand in for it.
///
/// `fixtures`-only, exactly like `track_delete_teardown_hooks` above; a release
/// build compiles no call, no arguments, and no map.
#[cfg(feature = "fixtures")]
#[derive(Clone)]
pub struct WorkspaceRepointRaceHook {
    pub entered: std::sync::Arc<Notify>,
    pub release: std::sync::Arc<Notify>,
}

#[cfg(feature = "fixtures")]
fn workspace_repoint_race_hooks() -> &'static StdMutex<HashMap<String, WorkspaceRepointRaceHook>> {
    static HOOKS: OnceLock<StdMutex<HashMap<String, WorkspaceRepointRaceHook>>> = OnceLock::new();
    HOOKS.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn install_workspace_repoint_race_hook_for_test(
    track_id: &str,
    hook: WorkspaceRepointRaceHook,
) {
    workspace_repoint_race_hooks()
        .lock()
        .expect("workspace repoint hook mutex")
        .insert(track_id.to_string(), hook);
}

async fn wait_at_workspace_repoint_race_hook(track_id: &str) {
    #[cfg(feature = "fixtures")]
    {
        let hook = workspace_repoint_race_hooks()
            .lock()
            .expect("workspace repoint hook mutex")
            .remove(track_id);
        if let Some(hook) = hook {
            hook.entered.notify_one();
            hook.release.notified().await;
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = track_id;
}

/// Test seam for the shutdown-failure branch of the fence.
///
/// `PlannerHarness::shutdown` fails only on a persistence error deep inside the
/// run loop, which an integration test cannot provoke without dismantling the
/// runtime row the fence needs. The branch is still worth covering — it is the
/// one that used to kill a track's planner agent outright — so the failure is
/// injected here, the same deterministic-injection posture S5 used for N16
/// rather than a multi-threaded hammer. `fixtures`-only; a release build
/// compiles the bare `shutdown()` call.
#[cfg(feature = "fixtures")]
fn workspace_repoint_shutdown_failures() -> &'static StdMutex<HashMap<String, ()>> {
    static FAILURES: OnceLock<StdMutex<HashMap<String, ()>>> = OnceLock::new();
    FAILURES.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn fail_workspace_repoint_shutdown_for_test(track_id: &str) {
    workspace_repoint_shutdown_failures()
        .lock()
        .expect("workspace repoint shutdown failure mutex")
        .insert(track_id.to_string(), ());
}

async fn shutdown_fenced_harness(
    harness: &crate::harness::PlannerHarness,
    track_id: &str,
) -> Result<()> {
    #[cfg(feature = "fixtures")]
    {
        let forced = workspace_repoint_shutdown_failures()
            .lock()
            .expect("workspace repoint shutdown failure mutex")
            .remove(track_id)
            .is_some();
        if forced {
            return Err(CalmError::Internal(
                "injected planner harness shutdown failure (#1147 S3 test seam)".into(),
            ));
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = track_id;
    harness.shutdown().await
}

/// What the fence transaction decided, carried out to the filesystem half.
struct RepointFence {
    /// The workspace as read *inside* the transaction — the authority, not the
    /// unlocked read the route did to answer 404.
    old_workspace: TrackWorkspace,
    /// Every runtime the fence superseded, so the process-side shutdown below
    /// knows which live handles to kill and the compensating restart knows
    /// something was torn down.
    superseded_runtime_ids: Vec<String>,
}

/// #1147 S3 — point a track at a repository the user already has
/// (design §更换与冻结, transition `managed → attached`).
///
/// # Why this is not a column write
///
/// SQLite transactions do not isolate the filesystem, so "check inside the
/// transaction" cannot close the window between the check and the move. The
/// planner harness is deliberately *not* frozen at this point and has run
/// `sandbox-mode: workspace-write` since its first message, and the dispatcher
/// pushes observations that start fresh turns. Three steps, none optional:
///
/// 1. **A real fence, in the same transaction as the criteria.** Every active
///    runtime of the track is marked `superseded`, which is the state
///    `dispatcher::harness_runtime_id_for_planner_card` reads
///    (`session_projection_active_for_card`, `state IN
///    ('starting','running','idle','turn_pending')`) before it will deliver an
///    observation. After this commit a push has nowhere to land. An
///    `interrupt` would not do: it is asynchronous and says nothing about the
///    *next* turn. The in-memory half — `HarnessRegistry::remove` +
///    `shutdown()` — follows immediately, because `maybe_issue_turn` reads no
///    database state and would otherwise turn an already-queued observation
///    into a turn.
/// 2. **The criteria are re-evaluated before anything irreversible.** Anything
///    the in-flight turn wrote between the fence and here makes this a 409
///    with nothing moved and no column changed.
/// 3. **The move asserts its own preconditions.** The old managed directory
///    goes through S5's [`workspace_recycle::recycle_track_workspace`] — the
///    single controlled entry point — which re-checks `kind == Managed`,
///    canonical containment in the workspace root, the exact
///    `<root>/<area>/<track>` depth, and our ownership marker, and renames into
///    `.trash` rather than deleting. The `TrackWorkspace` handed to it is the
///    OLD value, read inside the fence transaction, so it describes the
///    directory being reclaimed rather than the row's new state.
///
/// # Order: write the row, then move the directory
///
/// The opposite order is what S5 chose for `DELETE`, and for the opposite
/// reason. There, a failure after the move would leave a track row whose
/// directory is unreachable. Here the failure that actually happens is a
/// **claim conflict**: `area_folders` is scanned twice — once in the fence
/// transaction to fail fast, once authoritatively in the write transaction —
/// and a claim minted by a concurrent request in between must be able to abort
/// this whole request cleanly. Moving first would make that abort leave the
/// old workspace in the trash while the row still points at it, and the next
/// retry would then fail its emptiness check forever (a missing path is not
/// provably empty). Writing first makes every abort a clean 409.
///
/// The price, stated: a crash between the commit and the rename leaves the old
/// managed directory on disk with no row naming it. That is a leak, not a
/// loss, and unlike S5's it is a **derivable** one —
/// `managed_workspace_path(root, area_id, track_id)` still names it — so a
/// future sweep can find it without any new bookkeeping.
///
/// # What a refusal leaves behind
///
/// Nothing on disk and nothing in the row. The one visible effect is that the
/// planner harness was torn down, so this function restarts it on the **old**
/// path before returning. That restart is the same operation
/// `POST /api/cards/{id}/reset` performs routinely, and harness items are
/// persisted per card, so the user's transcript survives.
async fn repoint_track_workspace(
    s: &RouteState,
    w: &WorkerState,
    actor: &Actor,
    track: &Track,
    requested: &TrackWorkspacePatch,
) -> Result<Response> {
    // Issue #985's rule, applied to a strictly more destructive field: moving
    // a directory is a human decision. This is the only thing between an agent
    // and pointing a track at any repository on the box.
    //
    // **Reachability, stated exactly** (same posture as S5's guard 4). Through
    // HTTP this is unreachable today, and not because it is redundant: the
    // only header form that maps to a non-`User` `ActorId` is `ai:codex`
    // (`Actor::to_actor_id` sends every other string to `User` by a documented
    // defensive default), and that form carries an empty card id, which a
    // guard further out already 403s. So the header cannot produce a caller
    // this check would be the first to stop.
    //
    // It lives here, inside the operation rather than in the PATCH envelope,
    // precisely so it is not vacuous: an internal caller holding a real
    // `ActorId::AiCodex(card)` gets it, and it has a fixture
    // (`a_non_user_actor_may_not_change_a_workspace`, through
    // `repoint_track_workspace_for_test`) that constructs the caller HTTP
    // cannot.
    if !matches!(actor.to_actor_id(), ActorId::User) {
        return Err(CalmError::Forbidden(
            "track workspace changes are user-only".into(),
        ));
    }

    // Scope (design §更换与冻结). `managed → attached` is the transition; there
    // is no `managed → managed` because a managed path is derived from the
    // area and track ids, so "re-allocate" would always re-derive the same
    // directory. Answered explicitly rather than accepted as a no-op, so a
    // client that asks for it learns that instead of believing it worked.
    if requested.kind != TrackWorkspaceKind::Attached {
        return Err(CalmError::BadRequest(
            "track workspace: only `attached` is a target — pointing a track at a repository \
             you already have. There is no `managed` target: a managed workspace's path is \
             derived from the track, so re-allocating one would produce the same directory."
                .into(),
        ));
    }

    // The system area's launchpad path is kernel-maintained
    // (`today_launchpad_ensure_tx` re-derives it on every `ensure`) and is the
    // documented exception to the freeze latch. A user PATCH must not touch
    // it. Same scope decision as S5's row-layer 403 on DELETE: the whole
    // system area, not a `purpose = launchpad` carve-out.
    let area = s.repo.area_get(track.area_id.as_str()).await?;
    if area.as_ref().is_none_or(|c| c.kind == AreaKind::System) {
        return Err(CalmError::Forbidden(format!(
            "track {} belongs to the system area; its workspace is kernel-maintained",
            track.id
        )));
    }

    // Validate the target BEFORE any write. Design D3, and the whole reason
    // #1147 exists: a path that does not exist or is not a Git work tree must
    // fail here with git's own words, not four steps later as a worker's
    // `spawn-failed`.
    let new_path = normalize_path(&requested.path);
    crate::workspace_materialize::validate_attached_workspace(std::path::Path::new(&new_path))?;

    let workspace_root = s.workspace_root.clone();
    let track_id = track.id.to_string();
    let area_id = track.area_id.as_str().to_string();

    // ---- Step 1: criteria + fence, in one BEGIN IMMEDIATE -----------------
    let fence_conflict = FolderConflictSlot::default();
    let fence_track_id = track_id.clone();
    let fence_area_id = area_id.clone();
    let fence_path = new_path.clone();
    let fence_claim = FolderClaim::Enforce {
        attach: requested.attach_folder,
        conflict: fence_conflict.clone(),
    };
    let fence = crate::db::write_in_tx_typed(s.repo.as_ref(), move |tx| {
        let track_id = fence_track_id.clone();
        let area_id = fence_area_id.clone();
        let new_path = fence_path.clone();
        let claim = fence_claim.clone();
        Box::pin(async move {
            // Authoritative re-read. The route's unlocked read answered 404
            // and scoped the event; every decision below comes from here.
            let old_workspace = crate::db::sqlite::track_workspace_read_tx(tx, &track_id).await?;
            if old_workspace.kind != TrackWorkspaceKind::Managed {
                return Err(CalmError::Conflict(format!(
                    "track {track_id} already has an attached workspace ({}); an attached \
                     repository belongs to you, and the server never moves, initializes or \
                     deletes one — so it is also never re-pointed away from",
                    old_workspace.path
                )));
            }
            if let Some(frozen_at) = old_workspace.frozen_at {
                return Err(CalmError::Conflict(format!(
                    "track {track_id} workspace was frozen at {frozen_at}; a workspace is a \
                     default that can be changed only before any work happens in it"
                )));
            }
            // The one predicate that does not enumerate writers: it asks the
            // disk. Runs under the writer lock so it is decided together with
            // the fence, and is repeated after the commit because SQLite
            // isolates none of it.
            let verdict = workspace_pristine(std::path::Path::new(&old_workspace.path));
            if !verdict.is_pristine() {
                return Err(CalmError::Conflict(
                    verdict.conflict_message(std::path::Path::new(&old_workspace.path)),
                ));
            }
            // Fail fast on the claim rules, WITHOUT minting anything: this
            // transaction commits (it is also the fence), so a row written
            // here would survive a later refusal. The write transaction runs
            // the same rules authoritatively. Running them here as well means
            // the common conflict is answered before the harness is torn down.
            enforce_folder_claim_tx(
                tx,
                &claim,
                &area_id,
                &new_path,
                FolderClaimIntent::Repoint,
                FolderClaimPass::ScanOnly,
            )
            .await?;
            // THE FENCE. Every active runtime of this track, not just the planner
            // harness: "no new turn may acquire the old path" is a statement
            // about the track, and a rule with one named exception is the shape
            // this design line keeps being hurt by.
            let runtime_ids: Vec<String> = sqlx::query_scalar(
                "SELECT id FROM worker_sessions WHERE track_id=?1 \
                 AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
            )
            .bind(&track_id)
            .fetch_all(&mut **tx)
            .await?;
            for runtime_id in &runtime_ids {
                crate::db::sqlite::session_mark_superseded_runtime_tx(tx, runtime_id)
                    .await
                    .map_err(|e| CalmError::Internal(format!("workspace repoint fence: {e}")))?;
            }
            Ok(RepointFence {
                old_workspace,
                superseded_runtime_ids: runtime_ids,
            })
        })
    })
    .await;
    let fence = match fence {
        Ok(fence) => fence,
        Err(error) => return folder_conflict_response(&fence_conflict, error),
    };

    // The in-memory half of the fence. `maybe_issue_turn` consults no durable
    // state, so without this an observation enqueued before the commit would
    // still become a turn — writing into a directory that is about to be
    // renamed, and (because a process's cwd follows the inode on Linux)
    // continuing to write into `.trash` afterwards until the GC erases it.
    //
    // # A shutdown failure must NOT abort this function
    //
    // `get` then `remove`-on-success, and the error is logged rather than
    // propagated. Both halves of that are load bearing, and the shape this
    // replaces got both wrong: it removed the entry FIRST and then used `?`,
    // so a failing shutdown returned 500 having already (a) committed the
    // fence — every runtime superseded — and (b) dropped the registry entry.
    // The restart below never ran, and the track's planner agent was dead for
    // good: superseded in the database, absent from the registry, with
    // nothing left that would ever start it again. This route's whole promise
    // is that a refusal leaves nothing behind except a re-opened harness, and
    // that promise has to hold on the failure paths too.
    //
    // Keeping the entry on failure is deliberate: the run loop may still be
    // alive, and the restart below goes through `reserve_replacing`, which
    // supersedes whatever occupies the slot. Dropping it here would strand
    // that loop with no handle. Continuing is also safe rather than merely
    // convenient — the durable fence already stops any NEW turn, and an
    // in-flight turn that refused to stop is exactly what the pre-move
    // re-check below exists to catch.
    for runtime_id in &fence.superseded_runtime_ids {
        let Some(harness) = w.harness.get(runtime_id) else {
            continue;
        };
        let outcome = shutdown_fenced_harness(&harness, &track_id).await;
        match outcome {
            Ok(()) => {
                let _ = w.harness.remove(runtime_id);
            }
            Err(error) => tracing::error!(
                track_id,
                runtime_id,
                error = %error,
                "workspace repoint: shutting the fenced planner harness down failed. \
                 Continuing: the database fence already refuses new turns, the \
                 pre-move re-check catches anything an in-flight turn writes, and \
                 the registry entry is left for the restart to supersede."
            ),
        }
    }

    let old_path = std::path::PathBuf::from(&fence.old_workspace.path);

    // Deterministic race window for the timing test. No-op in production.
    wait_at_workspace_repoint_race_hook(&track_id).await;

    // ---- Step 2: re-check before anything irreversible --------------------
    let verdict = workspace_pristine(&old_path);
    if let PristineVerdict::Dirty { .. } = &verdict {
        restart_planner_harness_at(s, actor, track, &fence.old_workspace.path).await;
        return Err(CalmError::Conflict(verdict.conflict_message(&old_path)));
    }

    // ---- The write: claim + workspace, one transaction --------------------
    let new_workspace = TrackWorkspace {
        kind: TrackWorkspaceKind::Attached,
        path: new_path.clone(),
        // Frozen, one-way. Two independent reasons, either sufficient:
        // `attached → *` is not a legal transition, so an unfrozen attached
        // row has no legal use; and S4 pins "no attached track is ever
        // unfrozen" over the whole table, because an unfrozen attached row is
        // exactly what a future PATCH branch that forgot to check `kind` would
        // relocate — i.e. would move a real user repository.
        frozen_at: Some(crate::model::now_ms()),
    };
    let scope = EventScope::Track {
        track: track.id.clone(),
        area: track.area_id.clone(),
    };
    let actor_id = actor.to_actor_id();
    let write_conflict = FolderConflictSlot::default();
    let write_claim = FolderClaim::Enforce {
        attach: requested.attach_folder,
        conflict: write_conflict.clone(),
    };
    let write_track_id = track_id.clone();
    let write_area_id = area_id.clone();
    let write_workspace = new_workspace.clone();
    let written =
        write_with_actor_events_typed(s.repo.as_ref(), None, &s.events, &s.write, move |tx| {
            let scope = scope.clone();
            let track_id = write_track_id.clone();
            let area_id = write_area_id.clone();
            let workspace = write_workspace.clone();
            let claim = write_claim.clone();
            let actor_id = actor_id.clone();
            Box::pin(async move {
                // Authoritative, and atomic with the workspace write because
                // they share this `BEGIN IMMEDIATE`.
                enforce_folder_claim_tx(
                    tx,
                    &claim,
                    &area_id,
                    &workspace.path,
                    FolderClaimIntent::Repoint,
                    FolderClaimPass::Authoritative,
                )
                .await?;
                crate::db::sqlite::track_workspace_write_tx(tx, &track_id, &workspace).await?;
                let track = track_get_tx(tx, &TrackId::from(track_id)).await?;
                let events = vec![(
                    actor_id,
                    scope,
                    Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
                        track.clone(),
                        None,
                    )),
                )];
                Ok((track, events))
            })
        })
        .await;
    let (updated, _ids) = match written {
        Ok(written) => written,
        Err(error) => {
            // Nothing moved and nothing was written — put the harness back
            // where it was and report.
            restart_planner_harness_at(s, actor, track, &fence.old_workspace.path).await;
            return folder_conflict_response(&write_conflict, error);
        }
    };

    // ---- Step 3: the old managed directory goes to the trash --------------
    let decision = workspace_recycle::recycle_track_workspace(
        &workspace_root,
        area.as_ref().map(|c| c.kind),
        &track_id,
        // The OLD workspace value, read inside the fence transaction. The row
        // now says `attached`, so re-reading it here would make guard 1 refuse
        // and leave the directory behind forever.
        &fence.old_workspace,
        crate::model::now_ms(),
    );
    match decision {
        // Never materialized, or already reclaimed. Nothing to move.
        Ok(workspace_recycle::RecycleDecision::Refused(
            workspace_recycle::RecycleRefusal::PathMissing,
        ))
        | Ok(workspace_recycle::RecycleDecision::Trashed { .. }) => {}
        // The row has already moved, so this is a leak, not a failure of the
        // re-point: the track is correctly attached to the user's repository
        // and the stale managed directory is at a path that is still
        // derivable. Loud, and not a 500 — telling the caller the request
        // failed would be a lie.
        Ok(workspace_recycle::RecycleDecision::Refused(refusal)) => {
            tracing::error!(
                track_id,
                path = %fence.old_workspace.path,
                reason = refusal.tag(),
                "workspace repoint: the track now points at the user's repository, but its old \
                 managed directory could not be reclaimed and is leaked on disk"
            );
        }
        Err(error) => {
            tracing::error!(
                track_id,
                path = %fence.old_workspace.path,
                error = %error,
                "workspace repoint: the track now points at the user's repository, but moving \
                 its old managed directory to the trash failed; it is leaked on disk"
            );
        }
    }
    workspace_recycle::gc_trash_best_effort(&workspace_root, crate::model::now_ms());

    // Re-open the planner thread on the new cwd. `force_new_thread` is the only
    // mechanism that re-reads `cwd`: a resumed codex thread keeps the cwd it
    // was minted with, so resuming here would leave the planner agent in the
    // directory that just went to the trash.
    restart_planner_harness_at(s, actor, &updated, &updated.workspace.path).await;

    Ok(Json(updated).into_response())
}

/// #1147 S3 — reach the re-point with a caller HTTP cannot produce.
///
/// The user-only guard's only non-`User` HTTP form (`ai:codex`) is stopped
/// further out by the empty-card-id check, so an integration test driving the
/// route can never distinguish "my guard fired" from "the outer one did". This
/// calls the operation directly with a chosen actor. `fixtures`-only.
#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub async fn repoint_track_workspace_for_test(
    s: &RouteState,
    w: &WorkerState,
    actor: &Actor,
    track: &Track,
    requested: &TrackWorkspacePatch,
) -> Result<Response> {
    repoint_track_workspace(s, w, actor, track, requested).await
}

/// Render a parked [`FolderConflict`] as the structured 409 the create route
/// returns, so a client sees the same body whichever route it reached the
/// claim rules through.
///
/// `FolderConflictSlot::park` stashes the body and returns a plain `Conflict`
/// whose message is only a fallback; without this the caller would get the bare
/// string and lose `folder_id` / `area_id` / `conflict_kind` — which is exactly
/// what the FE needs to say *which* area already owns the directory.
fn folder_conflict_response(slot: &FolderConflictSlot, error: CalmError) -> Result<Response> {
    match slot.take() {
        Some(body) => Ok((StatusCode::CONFLICT, Json(body)).into_response()),
        None => Err(error),
    }
}

/// Re-open the track's planner harness thread at `cwd`.
///
/// Best effort, and deliberately so: turning a harness hiccup into a 500 here
/// would be worse than useless — the workspace has already moved and the row
/// already says so, so the caller must not be told the whole operation failed.
///
/// `idempotency_key: None`, like every other non-launchpad planner-harness start
/// (`routes/tracks.rs::start_planner_harness`, `routes/cards.rs`'s reset). The
/// launchpad and child-track call sites need a workspace digest in their keys
/// because they are re-driven with the same key; this one is minted per
/// request and cannot collide.
async fn restart_planner_harness_at(s: &RouteState, actor: &Actor, track: &Track, cwd: &str) {
    // Same resolution the dispatcher uses (`resolve_planner_card`): the role
    // cache, not a `cards.kind` guess.
    let cards = match s.repo.cards_by_track(track.id.as_str()).await {
        Ok(cards) => cards,
        Err(error) => {
            tracing::warn!(track_id = %track.id, error = %error, "workspace repoint: planner card lookup failed");
            return;
        }
    };
    let planner_card_id = cards.into_iter().find_map(|card| {
        (s.write.verify_role(&card.id) == Some(CardRole::Planner)).then(|| card.id.to_string())
    });
    let Some(planner_card_id) = planner_card_id else {
        // No planner card on this track, so no harness to re-anchor.
        return;
    };
    let request = PlannerHarnessStartOperationPayload {
        actor: actor.to_actor_id(),
        track_id: track.id.to_string(),
        planner_card_id: CardId::from(planner_card_id.clone()),
        report_card_id: None,
        sort: None,
        cwd: cwd.to_string(),
        goal: None,
        // The transcript is NOT reset: harness items are persisted per card,
        // so re-opening the thread costs the agent its in-thread context, not
        // the user's history.
        reset_harness_items: false,
        force_new_thread: true,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
        first_message: None,
    };
    let hash = match stable_payload_hash(
        &serde_json::json!({"actor": actor.as_str(), "request": &request}),
    ) {
        Ok(hash) => hash,
        Err(error) => {
            tracing::warn!(track_id = %track.id, error = %error, "workspace repoint: payload hash failed");
            return;
        }
    };
    let payload = match serde_json::to_value(&request) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(track_id = %track.id, error = %error, "workspace repoint: payload encode failed");
            return;
        }
    };
    match s
        .operation_runtime
        .submit(
            "planner-harness-start",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash: hash,
            },
            payload,
        )
        .await
    {
        Ok(op) => match s.operation_runtime.wait(&op).await {
            Ok(result)
                if matches!(
                    result.outcome,
                    OperationOutcome::Succeeded { .. }
                        | OperationOutcome::SucceededViaCollision { .. }
                ) => {}
            other => tracing::warn!(
                track_id = %track.id,
                cwd,
                outcome = ?other.map(|r| r.outcome),
                "workspace repoint: planner harness restart did not succeed; the workspace is \
                 correct but the planner agent is inert"
            ),
        },
        Err(error) => tracing::warn!(
            track_id = %track.id,
            cwd,
            error = %error,
            "workspace repoint: planner harness restart submission failed"
        ),
    }
}

#[utoipa::path(
    patch,
    path = "/api/tracks/{id}",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id")),
    request_body = TrackPatch,
    responses(
        (status = 200, description = "Track updated", body = Track),
        (status = 400, description = "Unsupported workspace change", body = ErrorBody),
        (status = 403, description = "Workspace change refused (system area)", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 409, description = "Workspace is frozen, attached, or no longer empty", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn update_track(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    actor: Actor,
    Path(id): Path<String>,
    Json(p): Json<TrackPatch>,
) -> Result<Response> {
    // Need area_id for the scope. Track rows are immutable wrt their
    // parent area, so reading outside the txn is safe (same rationale as
    // the delete path below).
    let existing = s
        .repo
        .track_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {id}")))?;

    // #1147 S3 — a workspace change is a filesystem move bracketed by two
    // transactions, not a column write, so it does not compose with the
    // mechanical row patch below. Mixing them would make a partial failure
    // ("the title changed, the workspace did not") indistinguishable from
    // success at the wire, so the combination is a 400 rather than an
    // ordering puzzle nobody can reason about.
    if let Some(workspace) = p.workspace.as_ref() {
        // Destructured rather than enumerated as `p.title.is_some() || …`.
        // The rule is "the workspace travels alone", so it has to consider
        // EVERY other field, and a hand-written list silently stops being
        // exhaustive the day someone adds one — the omission would read as
        // "that field may ride along", which is the opposite of the rule.
        // Binding every field by name makes the next addition a compile
        // error here instead.
        let TrackPatch {
            workspace: _,
            title,
            sort,
            archived_at,
            pinned_at,
            lifecycle,
            task_budget,
            require_task_gates,
            planner_task_ceiling,
            automation_policy,
            tree_task_budget,
        } = &p;
        let mixes_other_fields = title.is_some()
            || sort.is_some()
            || archived_at.is_some()
            || pinned_at.is_some()
            || lifecycle.is_some()
            || task_budget.is_some()
            || require_task_gates.is_some()
            || planner_task_ceiling.is_some()
            || automation_policy.is_some()
            || tree_task_budget.is_some();
        if mixes_other_fields {
            return Err(CalmError::BadRequest(
                "track workspace changes must be sent on their own; a workspace re-point moves \
                 directories on disk and cannot share a transaction with row edits"
                    .into(),
            ));
        }
        return repoint_track_workspace(&s, &w, &actor, &existing, workspace).await;
    }

    // The guard fires on *mentioning* `lifecycle`, not on changing it: a PATCH
    // that re-sends the track's current lifecycle is 403 too. That is
    // deliberate — the chat track has no lifecycle the user may drive, so
    // accepting a no-op write would advertise an editable field, and the FSM
    // would then have to be trusted to keep every such write a no-op forever.
    if existing.purpose.as_deref() == Some(AREA_CHAT_PURPOSE) && p.lifecycle.is_some() {
        return Err(CalmError::Forbidden(
            "area chat track lifecycle cannot be changed".into(),
        ));
    }
    let scope = EventScope::Track {
        track: existing.id.clone(),
        area: existing.area_id.clone(),
    };
    let actor_id = actor.to_actor_id();

    // Issue #985 — track-level automation controls are human decisions.
    // Reject non-user actors before entering the eventized write so neither
    // the row nor a TrackUpdated event can land.
    if (p.planner_task_ceiling.is_some()
        || p.automation_policy.is_some()
        || p.tree_task_budget.is_some())
        && !matches!(actor_id, ActorId::User)
    {
        return Err(CalmError::Forbidden(
            "automation_policy, planner_task_ceiling and tree_task_budget are user-only".into(),
        ));
    }

    // Issue #145 — lifecycle transitions go through a typed state
    // machine. The validator runs *before* the write so an illegal
    // transition surfaces as `Forbidden` without persisting either
    // the row update or the event.
    //
    // Same-state requests (`p.lifecycle == Some(current)`) are an
    // idempotent silent success for authorized actors: the validator
    // returns `Ok(())`, we strip `lifecycle` from the patch (so
    // `track_update_tx` doesn't pointlessly rewrite the column /
    // bump `updated_at`), and we skip the `TrackLifecycleChanged`
    // emit. If after stripping the patch has no other fields set,
    // we return the existing row without touching the DB at all.
    // Worker / plugin actors still hit `Forbidden` here regardless
    // of from == to — idempotency only applies once the actor has
    // any lifecycle authority.
    let mut p = p;
    let lifecycle_change = if let Some(to) = p.lifecycle {
        validate_transition(existing.lifecycle, to, &actor_id)
            .map_err(|e| CalmError::Forbidden(format!("track lifecycle: {e}")))?;
        if existing.lifecycle == to {
            // Idempotent no-op for lifecycle; drop it from the patch
            // so the row write below is a true no-op when no other
            // field is set.
            p.lifecycle = None;
            None
        } else {
            Some((existing.lifecycle, to))
        }
    } else {
        None
    };

    // Issue #644 — scheduler budget sanity. `Some(None)` clears back to
    // the kernel default; a present value must be non-negative (0 is a
    // legal "hold new dispatches" budget per design §5.2's
    // `max(0, budget - running_cost)`).
    if let Some(Some(budget)) = p.task_budget
        && budget < 0
    {
        return Err(CalmError::BadRequest(format!(
            "task_budget must be >= 0 (got {budget}); pass null to reset to the kernel default"
        )));
    }
    if let Some(Some(ceiling)) = p.planner_task_ceiling
        && ceiling < 0
    {
        return Err(CalmError::BadRequest(format!(
            "planner_task_ceiling must be >= 0 (got {ceiling}); pass null to reset to the kernel default"
        )));
    }
    // Issue #985 slice 6 PR-B — same shape as `planner_task_ceiling`. 0 is legal
    // ("no new planner inventory anywhere in this tree"); the root-only rule is
    // enforced inside `track_update_tx`, which every writer shares.
    if let Some(Some(budget)) = p.tree_task_budget
        && !(0..=MAX_TREE_TASK_BUDGET).contains(&budget)
    {
        return Err(CalmError::BadRequest(format!(
            "tree_task_budget must be between 0 and {MAX_TREE_TASK_BUDGET} (got {budget}); pass null to reset to the kernel default"
        )));
    }
    if let Some(Some(policy)) = &p.automation_policy
        && !matches!(policy.as_str(), "auto-declare" | "declare-and-wait")
    {
        return Err(CalmError::BadRequest(format!(
            "automation_policy must be auto-declare or declare-and-wait (got {policy}); pass null to reset to the kernel default"
        )));
    }

    // If the patch is now entirely empty (lifecycle was a no-op and
    // no other field was supplied) there's nothing to write and
    // nothing to emit — return the track as-is. This is the
    // idempotent retry path for "planner re-sends the current state."
    let patch_has_other_changes = p.title.is_some()
        || p.sort.is_some()
        || p.archived_at.is_some()
        || p.pinned_at.is_some()
        || p.task_budget.is_some()
        || p.require_task_gates.is_some()
        || p.planner_task_ceiling.is_some()
        || p.automation_policy.is_some()
        || p.tree_task_budget.is_some();
    if lifecycle_change.is_none() && !patch_has_other_changes {
        return Ok(Json(existing).into_response());
    }

    // When a lifecycle change is part of the patch we emit *two*
    // events from the same txn: a `TrackLifecycleChanged` so dedicated
    // subscribers don't have to inspect every `TrackUpdated`, plus the
    // usual `TrackUpdated` so cache invalidation still sees the new
    // row shape. Both share scope + actor; both land or neither does.
    let area_id_for_event = existing.area_id.clone();
    let track_id_for_event = existing.id.clone();
    // `tree_task_budget` feeds every member's deterministic share, so changing
    // it invalidates every member's projection. Rebuild the bounded member set
    // in this same write transaction: after PATCH returns, no descendant can
    // retain a pending row admitted by the old budget and race a later claim.
    let projection_policy_changed = p.planner_task_ceiling.is_some()
        || p.automation_policy.is_some()
        || p.tree_task_budget.is_some();
    let tree_budget_changed = p.tree_task_budget.is_some();
    let p_for_tx = p.clone();
    let (track, _ids) =
        write_with_actor_events_typed(s.repo.as_ref(), None, &s.events, &s.write, move |tx| {
            let scope = scope.clone();
            Box::pin(async move {
                let track = track_update_tx(tx, &id, p_for_tx).await?;
                let projections = if projection_policy_changed {
                    if tree_budget_changed {
                        tasks_rebuild_tree_tx(tx, &id).await?
                    } else {
                        vec![(track.clone(), tasks_rebuild_tx(tx, &id).await?)]
                    }
                } else {
                    Vec::new()
                };
                let mut events: Vec<(ActorId, EventScope, Event)> = Vec::new();
                if let Some((from, to)) = lifecycle_change {
                    events.push((
                        actor_id.clone(),
                        scope.clone(),
                        Event::TrackLifecycleChanged {
                            id: track_id_for_event.clone(),
                            area_id: area_id_for_event.clone(),
                            from,
                            to,
                            agent_message: None,
                        },
                    ));
                }
                events.push((
                    actor_id.clone(),
                    scope.clone(),
                    Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
                        track.clone(),
                        None,
                    )),
                ));
                for (projected_track, projection) in projections {
                    if !projection.changed_keys.is_empty() {
                        events.push((
                            actor_id.clone(),
                            EventScope::Track {
                                track: projected_track.id.clone(),
                                area: projected_track.area_id.clone(),
                            },
                            Event::PlanUpdated {
                                track_id: projected_track.id,
                                changed_keys: projection.changed_keys,
                                agent_message: None,
                            },
                        ));
                    }
                    events.extend(projection.kernel_events);
                }
                Ok((track, events))
            })
        })
        .await?;
    Ok(Json(track).into_response())
}

async fn snapshot_track_deletion(
    s: &RouteState,
    pool: &sqlx::SqlitePool,
    track: &Track,
) -> Result<TrackDeletePlan> {
    let cards = s.repo.cards_by_track(track.id.as_str()).await?;
    let mut terminals = Vec::new();
    for card in &cards {
        if let Some(terminal) = s.repo.terminal_get_by_card(card.id.as_str()).await? {
            terminals.push(terminal);
        }
    }
    let active_runtime_ids = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE track_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
    )
    .bind(track.id.as_str())
    .fetch_all(pool)
    .await?;
    Ok(TrackDeletePlan {
        track_id: track.id.clone(),
        area_id: track.area_id.clone(),
        cards,
        terminals,
        active_runtime_ids,
    })
}

async fn teardown_track_deletion(
    s: &RouteState,
    w: &WorkerState,
    cs: &CodexShellState,
    plan: &TrackDeletePlan,
) -> Result<()> {
    wait_at_track_delete_teardown_hook(plan.track_id.as_str()).await;
    for card in &plan.cards {
        interrupt_shared_card_active_turn(s.repo.as_ref(), cs, card).await;
    }
    for terminal in &plan.terminals {
        reap_terminal_artifacts_with_renderer(Some(w.terminal_renderer.as_ref()), terminal).await;
    }
    for runtime_id in &plan.active_runtime_ids {
        if let Some(harness) = w.harness.get(runtime_id) {
            harness.shutdown().await?;
            let _ = w.harness.remove(runtime_id);
        }
    }
    Ok(())
}

#[allow(deprecated)]
async fn finish_track_deletion(
    s: &RouteState,
    plan: TrackDeletePlan,
    actor: ActorId,
) -> Result<()> {
    let write_for_tx = s.write.clone();
    let track_id = plan.track_id.clone();
    let area_id = plan.area_id.clone();
    let terminals = plan.terminals;
    let scope = EventScope::Track {
        track: track_id.clone(),
        area: area_id.clone(),
    };
    let (sweeps, _ids) =
        write_with_actor_events_typed(s.repo.as_ref(), None, &s.events, &s.write, move |tx| {
            Box::pin(async move {
                for terminal in &terminals {
                    match terminal_delete_tx(tx, &terminal.id)
                        .await
                        .map_err(CalmError::from)
                    {
                        Ok(()) => {}
                        Err(CalmError::NotFound(_)) => {}
                        Err(e) => return Err(e),
                    }
                }
                overlay_delete_card_overlays_by_track_tx(tx, track_id.as_str()).await?;
                overlay_delete_by_entity_tx(tx, "track", track_id.as_str()).await?;
                overlay_delete_by_entity_tx(tx, "view", track_id.as_str()).await?;
                let release = release_workspace_leases_for_track_tx(tx, track_id.as_str()).await?;
                let mut events = release.events;
                track_delete_tx(tx, track_id.as_str(), write_for_tx.area_cache()).await?;
                events.push((
                    actor,
                    scope,
                    Event::TrackDeleted {
                        id: track_id,
                        area_id,
                    },
                ));
                Ok((release.sweep.into_iter().collect::<Vec<_>>(), events))
            })
        })
        .await?;
    sweep_workspace_worktrees_for_tracks_repo(s.repo.as_ref(), &s.events, sweeps).await?;
    Ok(())
}

/// #1147 S5 — reclaim this track's managed workspace, between teardown and the
/// row delete.
///
/// **Ordering.** Teardown has already stopped every harness and terminal, so
/// nothing is writing into the directory; the row delete has not happened yet,
/// so a failure here aborts the whole DELETE with the track and its directory
/// both intact and the request retryable. The reverse order (row first) would
/// turn a rename failure into "the track is gone, its repository is not", which
/// is unretryable and needs a human.
///
/// Recycling is not conditional on that ordering being observed elsewhere: the
/// guards in [`workspace_recycle`] are what make the delete safe, not the
/// position of this call.
///
/// A *refusal* (guard not satisfied) is not an error — see
/// [`workspace_recycle::recycle_track_workspace`] for why the row must stay
/// deletable even when the directory cannot be proven ours.
///
/// `area_kind` is guard 4's input, read once by the caller (which needs it for
/// the row-layer 403 anyway). `None` — an area row we could not read — is "not
/// provably a user area", and the recycler refuses on it.
fn recycle_track_workspace_for_delete(
    s: &RouteState,
    track: &Track,
    area_kind: Option<AreaKind>,
) -> Result<()> {
    workspace_recycle::recycle_track_workspace(
        &s.workspace_root,
        area_kind,
        track.id.as_str(),
        &track.workspace,
        crate::model::now_ms(),
    )?;
    workspace_recycle::gc_trash_best_effort(&s.workspace_root, crate::model::now_ms());
    Ok(())
}

#[utoipa::path(
    delete,
    path = "/api/tracks/{id}",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id")),
    responses(
        (status = 204, description = "Track deleted"),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 403, description = "Track belongs to the system area and cannot be deleted via REST", body = ErrorBody),
        (status = 409, description = "Track has a descendant or active forge action", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn delete_track(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    State(cs): State<CodexShellState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    // Issue #197 — eager teardown for every terminal under the track.
    //
    // `terminals.card_id` is now `ON DELETE RESTRICT` (migration 0011)
    // so the prior model — let the FK cascade nuke the rows under us
    // and let the sweeper catch the leaked daemons ~60 s later —
    // doesn't work anymore: the cascade aborts the track-delete txn.
    // This handler now owns the full subtree teardown:
    //
    //   1. Best-effort unlocked descendant preflight, then snapshot
    //      cards/terminals/runtimes.
    //   2. Outside SQLite: interrupt turns and stop terminal/harness processes.
    //   3. Short IMMEDIATE tx: recheck descendants authoritatively in
    //      `track_delete_tx`, then remove terminal rows, overlays, leases and track.
    let track = s
        .repo
        .track_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("track {id}")))?;
    let track_id = track.id.clone();

    // #1147 S5 — the ROW-layer half of recycle guard 4.
    //
    // `workspace_recycle`'s guard 4 refuses to touch a system-area workspace on
    // disk, and `DELETE /api/areas/{id}` already 403s a system area. This route
    // was the asymmetric one: it deleted a system-area track row and returned
    // 204 while the directory (correctly) survived — and *that* is the leak,
    // because reclaiming a managed directory needs the track row that names it.
    // Once the row is gone the directory is unreachable forever, so every
    // launchpad delete + `ensure` cycle would strand one more orphan
    // repository.
    //
    // Same invariant as guard 4, one layer up: system scaffolding is
    // kernel-owned and not user-deletable. Kernel paths that legitimately
    // retire these rows do not come through this handler.
    //
    // **Scope is the whole system area, not just the launchpad — deliberately.**
    // The rule is written over the area, not over `purpose = launchpad`,
    // because carving out a purpose puts an exception into "the system area is
    // kernel-owned", and an invariant with an exception is the shape this
    // design line keeps getting hurt by. What the wide rule costs is that the
    // launchpad track — which *is* user-visible, on Today — cannot be deleted
    // through this handler either. That is the accepted price.
    //
    // What recreates the row is `routes::today::ensure_today_launchpad`, and it
    // is reached two ways: the explicit `POST /api/today/launchpad/ensure`, and
    // `POST /api/today/summary`, which calls it directly
    // (`routes::today_summary::write_today_summary`). So a permitted delete
    // would survive page loads — loading Today runs only the read-only resolve
    // and never POSTs either endpoint (INV-TODAYDOC-001) — but the next Today
    // summary that gets far enough to write would rebuild the launchpad
    // underneath the user. "Far enough" is literal: `write_today_summary`
    // computes the local day's activity window first and returns 409
    // `TodaySummaryNoActivity` when it is empty, and only past that check does
    // it call `ensure_today_launchpad`. On a day with no workspace activity
    // every summary POST stops at the 409 and rebuilds nothing, however many
    // times it is repeated; the explicit `POST /api/today/launchpad/ensure`
    // has no such precondition and rebuilds on any day. The ruling does not
    // rest on the deletion being hard to undo, or on it being harmless; it
    // rests on where the ownership boundary is drawn.
    //
    // #1300 — this paragraph used to justify itself by the *other* residents
    // of the system area, the three hidden template tracks `ensure_templates`
    // seeded. Those are gone: a template is a Rust constant
    // (`crate::templates`) and creating from one mints no hidden track. The
    // ruling did not depend on them — it is about where the boundary is drawn,
    // not about how many rows sit behind it — so it stands unchanged with the
    // launchpad track as today's only kernel-seeded resident.
    let owning_area = s.repo.area_get(track.area_id.as_str()).await?;
    if owning_area
        .as_ref()
        .is_some_and(|c| c.kind == AreaKind::System)
    {
        return Err(CalmError::Forbidden(format!(
            "track {id} belongs to the system area and cannot be deleted via the public API"
        )));
    }

    // Defensive TOCTOU guard only: this non-transactional read happens before
    // the teardown tx, so a forge-action can still become in-flight before the
    // sweep. It shrinks the race; durable parked recovery is the backstop, and
    // the airtight in-tx/lease-hold guard belongs to slice ⑤.
    let pool = w.repo.sqlite_pool().ok_or_else(|| {
        CalmError::Internal("delete_track forge-action fence requires sqlite-backed repo".into())
    })?;
    if track_has_active_forge_action(&pool, track_id.as_str()).await? {
        return Err(CalmError::Conflict(format!(
            "track {id} has an in-flight forge-action; retry after it settles"
        )));
    }

    // Experience-only preflight: the in-transaction guard in `track_delete_tx`
    // remains the sole correctness boundary for this route and raw Repo calls.
    // A child created after this read can still make the final delete return
    // Conflict after teardown; that rare race is safe and retryable.
    if let Some(child_id) =
        sqlx::query_scalar::<_, String>("SELECT id FROM tracks WHERE parent_track_id=?1 LIMIT 1")
            .bind(track_id.as_str())
            .fetch_optional(&pool)
            .await?
    {
        return Err(CalmError::Conflict(format!(
            "track {id} has child track {child_id}; cancel it if needed, then delete that child track first"
        )));
    }

    let plan = snapshot_track_deletion(&s, &pool, &track).await?;
    teardown_track_deletion(&s, &w, &cs, &plan).await?;
    recycle_track_workspace_for_delete(&s, &track, owning_area.map(|c| c.kind))?;
    finish_track_deletion(&s, plan, actor.to_actor_id()).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Issue #247 PR3 — user-facing track-report edit endpoint
// ---------------------------------------------------------------------------

/// A report link from another track that targets this track.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct TrackBacklink {
    pub src_track_id: String,
    pub src_track_title: String,
    pub src_block_id: String,
    pub dst_block_id: Option<String>,
    pub label: String,
    pub quote: report_backlinks::BacklinkQuote,
    pub updated_at: i64,
}

/// A bounded page of report backlinks.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct TrackBacklinksResponse {
    pub backlinks: Vec<TrackBacklink>,
    pub truncated: bool,
    pub skipped_sources: usize,
}

impl From<report_backlinks::Backlink> for TrackBacklink {
    fn from(value: report_backlinks::Backlink) -> Self {
        Self {
            src_track_id: value.src_track_id,
            src_track_title: value.src_track_title,
            src_block_id: value.src_block_id,
            dst_block_id: value.dst_block_id,
            label: value.label,
            quote: value.quote,
            updated_at: value.updated_at,
        }
    }
}

impl From<report_backlinks::BacklinkPage> for TrackBacklinksResponse {
    fn from(value: report_backlinks::BacklinkPage) -> Self {
        Self {
            backlinks: value
                .backlinks
                .into_iter()
                .map(TrackBacklink::from)
                .collect(),
            truncated: value.truncated,
            skipped_sources: value.skipped_sources,
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/tracks/{id}/backlinks",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id")),
    responses(
        (status = 200, description = "Report links from tracks in the same area", body = TrackBacklinksResponse),
        (status = 404, description = "Track not found", body = ErrorBody),
    ),
)]
pub(crate) async fn get_track_backlinks(
    State(s): State<RouteState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let page = report_backlinks::backlinks_for_track(s.repo.as_ref(), &id).await?;
    Ok(Json(TrackBacklinksResponse::from(page)))
}

/// Request body for `POST /api/tracks/:id/report`.
///
/// `summary` and `body` are required `String`s (per
/// `TrackReportPayload`'s [[required-over-option]] rule), and
/// `ifDocRev` is the required document-wide revision anchor. An empty
/// `summary` is valid; the caller must commit to *some* string.
///
/// **No `author` field.** Author is derived server-side from the
/// authenticated session and pinned to [`EditAuthor::User`] for this
/// endpoint — accepting one on the wire would let a User forge
/// `EditAuthor::Planner` and make a hand-typed edit look like the AI
/// did it. Even if a client serializes an `author` key the handler
/// ignores it (serde `deny_unknown_fields` would 400 it; this is the
/// stricter contract that closes the spoofing risk by construction).
///
/// `schemaVersion` is also intentionally absent — it's a server-managed
/// invariant pinned to [`TrackReportPayload::SCHEMA_VERSION`] and the
/// projected payload returned in the response reasserts the current
/// version. Letting clients write the version field would invite
/// silent shape drift the first time someone forgot to update both
/// sides.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateTrackReportBody {
    /// Expected document revision from the latest report read. Use zero
    /// for a document that has never been persisted through the CRDT path.
    pub if_doc_rev: u64,
    /// One-line summary the track-list sidebars surface. Empty string
    /// is a valid value; the caller must commit.
    pub summary: String,
    /// Markdown source. Sections are derived at render time by
    /// splitting at H1 (`^# `) headings; the kernel does not interpret
    /// the structure.
    pub body: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrackReportReadResponse {
    pub schema_version: u32,
    pub doc_rev: u64,
    pub summary: String,
    pub body: String,
    pub blocks: Vec<calm_types::track_report::ReportBlock>,
    pub task_diagnostics: Vec<crate::db::sqlite::BlockVerdict>,
}

#[utoipa::path(
    get,
    path = "/api/tracks/{id}/report",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id")),
    responses(
        (status = 200, description = "Current report with derived task diagnostics", body = TrackReportReadResponse),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody)
    ),
)]
pub(crate) async fn get_track_report(
    State(s): State<RouteState>,
    _principal: Principal,
    Path(id): Path<String>,
) -> Result<Response> {
    let (_, report_card, _) = resolve_report_for_track(s.repo.as_ref(), &id).await?;
    let snapshot = load_report_read_snapshot(s.repo.as_ref(), report_card.id.as_str()).await?;
    Ok((
        StatusCode::OK,
        Json(TrackReportReadResponse {
            schema_version: snapshot.schema_version,
            doc_rev: snapshot.doc_rev,
            summary: snapshot.summary,
            body: snapshot.body,
            blocks: snapshot.blocks,
            task_diagnostics: snapshot.task_diagnostics,
        }),
    )
        .into_response())
}

/// `POST /api/tracks/:id/report` — user-driven track-report edit. The
/// REST-side counterpart of the planner-MCP `calm.report.write` tool;
/// both paths funnel through the `track_report::write` module — this
/// one via `rest_user_replace`, the tool via `agent_report_op` — so the
/// dual-event invariant (`CardUpdated` + `TrackReportEdited`) and the
/// CRDT write happen identically regardless of who's editing.
///
/// **Auth contract** (issue #247 PR3 acceptance):
///
///   * No session cookie → 401 (`auth::require_session` middleware
///     short-circuits before this handler runs).
///   * Authenticated session BUT non-user actor declared via
///     `X-Calm-Actor` (worker / `ai:*` / etc.) → 403. Only
///     [`ActorId::User`] is allowed. This closes the "planner card's
///     own session cookie forwards a User edit" hole — a future
///     surface that lets the planner card hold a session must not be
///     able to bypass the User-only contract by claiming `ai:codex`.
///   * Track doesn't exist → 404.
///   * Track exists but the track-report card is missing → 500
///     (invariant violation; PR1 backfill guarantees the row).
///
/// The response is the *projected* [`TrackReportPayload`] read back
/// from the CRDT post-merge — not the request body verbatim — so the
/// frontend sees what every other reader will see (the JSON cache
/// mirrors the CRDT projection, which under single-writer is the
/// same bytes as the input, but reading from the doc keeps the
/// "CRDT is source of truth" contract true by construction).
#[utoipa::path(
    post,
    path = "/api/tracks/{id}/report",
    tag = "tracks",
    params(("id" = String, Path, description = "Track id")),
    request_body = UpdateTrackReportBody,
    responses(
        (status = 200, description = "Updated track-report payload", body = TrackReportPayload),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 403, description = "Non-user actor (worker / plugin / planner) rejected", body = ErrorBody),
        (status = 409, description = "Report document revision conflict", body = ErrorBody),
        (status = 404, description = "Track not found", body = ErrorBody),
        (status = 500, description = "Internal error (incl. missing report-card invariant)", body = ErrorBody),
    ),
)]
pub(crate) async fn update_track_report(
    State(s): State<RouteState>,
    // `Principal` extraction implicitly asserts the session middleware
    // has run — a missing/invalid cookie surfaces as 401 from
    // `auth::require_session` long before this handler is invoked.
    // We don't read any field off `_principal` today (single-user
    // owner model: there's exactly one User to attribute to). Held
    // here so the future multi-user split can attribute edits via
    // `principal.user_id` without changing the handler signature.
    _principal: Principal,
    actor: Actor,
    Path(id): Path<String>,
    Json(body): Json<UpdateTrackReportBody>,
) -> Result<Response> {
    // Server-side actor pinning. The route is gated to `ActorId::User`
    // only — anything else (worker / planner / plugin / kernel) is 403.
    //
    // **Direct string check, NOT `to_actor_id()`.** The typed mapping
    // has a defensive fallback that classifies anything outside its
    // explicit `"user"` / `"ai:codex"` arms as `ActorId::User` (so a
    // future relaxation can't synthesize a Kernel/Plugin identity from
    // an attacker-controlled header — see the rationale in
    // `actor::Actor::to_actor_id`). That fallback is the right call
    // for *event-log attribution* — better to mis-tag as User than to
    // forge a Kernel write — but it's the wrong shape for *gating*:
    // an `X-Calm-Actor: ai:claude` header would pass a
    // `matches!(actor.to_actor_id(), ActorId::User)` check and reach
    // the persist call. Since #1318 §1 the handler cannot name an
    // author at all — the entry point it calls fixes it
    // — so no audit-log corruption is possible by construction rather
    // than by a hardcoded argument a later edit could change. That
    // still leaves the gate itself to make honest: the OpenAPI /
    // handler doc both claim "any non-user actor → 403", and only this
    // raw string check makes that true of the *request*, not merely of
    // what got recorded. The only declared actor that reaches the
    // persist entry here is exactly `"user"`. Every other validated
    // header value (`ai:codex`, `ai:claude`, `ai:gpt5`, future `ai:*`)
    // is 403.
    super::track_report_blocks::require_rest_user_actor(&actor)?;

    // Resolve the track + report card + current payload. 404 on missing
    // track; 500 (Internal) on missing report card (invariant; PR1
    // backfill plus the partial unique index on `cards.kind =
    // 'track-report'` guarantee one report row per track).
    let target = track_report::ReportEditTarget::resolve(s.repo.as_ref(), &id).await?;

    // Build the next payload from the request body. `schemaVersion` is
    // always the current constant — the field is not on the wire shape
    // (see `UpdateTrackReportBody` doc) so we stamp it here.
    let if_doc_rev = body.if_doc_rev;
    let next = TrackReportPayload::new(body.summary, body.body);

    // Persist + emit. `EditAuthor::User` is the load-bearing
    // attribution — the wire shape doesn't accept `author` (see the
    // request-body doc), so nothing the caller sends can change it.
    // PR5's planner system prompt will wake on
    // `TrackReportEdited { author: User }` specifically.
    //
    // #1318 §1 — the constant no longer lives here. It is inside
    // `rest_user_replace`, one of three entry points into a module
    // whose writer is private to it; this handler cannot
    // name an author at all, so the "any non-user actor → 403" claim in
    // the doc above and the recorded author cannot drift apart by
    // editing this call. Read the old wording carefully before reusing
    // it: this was never "the only place `User` is written" — the REST
    // block endpoints record `User` too, through the sibling entry
    // `rest_user_block_op`, and since #1318 so would any other caller of
    // either REST entry.
    let updated = track_report::write::rest_user_replace(
        s.repo.as_ref(),
        &s.events,
        &s.write,
        target,
        next,
        if_doc_rev,
    )
    .await?;

    // Project the persisted payload out of the updated card row. This
    // is the CRDT-projected shape (the writer
    // re-derives summary/body from the doc post-update before writing
    // the JSON cache), so the response matches what the next reader
    // (frontend / other REST clients / WS subscribers) will see.
    let payload: TrackReportPayload = serde_json::from_value(updated.payload).map_err(|e| {
        CalmError::Internal(format!(
            "track-report edit: re-deserialize projected payload: {e}",
        ))
    })?;
    Ok((StatusCode::OK, Json(payload)).into_response())
}

#[cfg(test)]
mod tests {
    use super::{
        planner_harness_layout_payload, prepare_fork_report, prepare_initial_report_payload,
        prepare_template_report,
    };
    use crate::db::prelude::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::model::{NewArea, NewCard, NewTrack};
    use crate::routes::theme::RequestTheme;
    use crate::templates::TEMPLATES;
    use crate::track_report::write::{InitialReportTarget, structural_init_report_tx};
    use crate::track_report::{ReportBlock, TrackReportPayload};
    use crate::track_report_doc::ReportDoc;
    use serde_json::json;

    /// Every built-in recipe instantiates, and its declarations are the tasks
    /// it advertises.
    ///
    /// This is the unit half of #1300's evidence. The integration
    /// characterization test (`track_template_tracks.rs`) compares the created
    /// track's *report* against the recipe; it cannot see the `declarations`
    /// this returns, because a recipe's tasks are all `ready: false` and
    /// `task_projection` skips the insert for anything non-schedulable
    /// (`schedulable = ready && ..`). So the `tasks` table is empty on both
    /// sides of the switch and proves nothing about the producer.
    ///
    /// Which makes this the only place "the declarations are actually
    /// produced" is observable: replace the `project_task_declarations` call in
    /// `prepare_initial_report_payload` with an empty vec and only this test
    /// goes red.
    #[test]
    fn every_recipe_instantiates_and_declares_its_tasks() {
        for template in &TEMPLATES {
            let key = template.key();
            let compiled = prepare_template_report(key).unwrap_or_else(|error| {
                panic!("`{key}` must instantiate: {error}");
            });
            let payload = compiled.payload;
            let declarations = compiled.declarations;
            assert!(
                payload.blocks.as_ref().is_some_and(|b| !b.is_empty()),
                "`{key}`: no blocks"
            );
            let declared: Vec<&str> = declarations
                .iter()
                .map(|declaration| declaration.key.as_str())
                .collect();
            let fenced: Vec<String> =
                crate::templates::template_task_payloads_from_body(&template.recipe().body)
                    .iter()
                    .filter_map(|task| task.get("key").and_then(|k| k.as_str()).map(str::to_string))
                    .collect();
            assert_eq!(
                declared,
                fenced.iter().map(String::as_str).collect::<Vec<_>>(),
                "`{key}`: declarations must be the recipe's task keys, in order"
            );
        }
    }

    /// A recipe whose body does not parse is refused, not silently thinned.
    ///
    /// ## The two shapes, and why one guard covers both
    ///
    /// `validate_body_fences` runs `invalid_neige_fences` **and**
    /// `validate_payload` over every parseable fence, so both a fence that
    /// fails to parse and a fence that parses but violates its schema are
    /// caught by the same call. Case A is the first, case B the second.
    ///
    /// Case A is the one that would otherwise be invisible: `split_body` treats
    /// a malformed neige fence as prose, so an indented opener does not error —
    /// it produces a report with one fewer task and no complaint anywhere.
    ///
    /// **The must-red is a single mutation**: delete the `validate_body_fences`
    /// line in `prepare_initial_report_payload` and *both* cases go red. An
    /// earlier draft claimed two independent guards with one case each; there
    /// is only one guard on this path, and writing two must-reds against it
    /// would have been a claim the code cannot support.
    ///
    /// Both feed `prepare_initial_report_payload` rather than
    /// `prepare_template_report`: the latter takes a key, and there is no key
    /// for a body no constant produces.
    #[test]
    fn a_recipe_that_does_not_parse_is_refused() {
        let good = crate::templates::template_by_key("small-change")
            .expect("known key")
            .recipe();

        // A: an indented opener. `split_body` demotes it to prose.
        let indented = good
            .body
            .replacen("```neige-block task", " ```neige-block task", 1);
        assert_ne!(indented, good.body, "the fixture did not change the body");
        // `match` rather than `expect_err`: the Ok arm carries a `ReportDoc`,
        // which is deliberately not `Debug` (it wraps an automerge document),
        // and deriving one on a production type to satisfy a test bound is the
        // wrong direction.
        let error = match prepare_initial_report_payload(
            "small-change",
            TrackReportPayload::new(good.summary.clone(), indented),
        ) {
            Ok(_) => panic!("an indented fence opener must be refused, not demoted to prose"),
            Err(error) => error,
        };
        assert!(
            format!("{error}").contains("small-change"),
            "the error must name the recipe; got {error}"
        );

        // B: a well-formed fence whose payload violates the task schema.
        let broken =
            good.body
                .replacen("\"kind\": \"codex\"", "\"kind\": \"not-a-worker-kind\"", 1);
        assert_ne!(broken, good.body, "the fixture did not change the body");
        if prepare_initial_report_payload(
            "small-change",
            TrackReportPayload::new(good.summary, broken),
        )
        .is_ok()
        {
            panic!("a schema-invalid task payload must be refused");
        }
    }

    #[test]
    fn fork_revalidates_every_fence_payload() {
        let invalid = ReportBlock {
            id: "b_0001".into(),
            kind: "task".into(),
            rev: 4,
            payload: json!({
                "key": "build",
                "kind": "not-a-worker-kind",
                "goal": "build",
                "ready": true,
                "declared_by": "user"
            }),
        };
        let error = prepare_fork_report("summary".into(), vec![invalid], "source", "target")
            .err()
            .expect("invalid copied task must abort fork");
        assert!(error.to_string().contains("invalid forked report block"));
    }

    /// #1252 S0b — the `KIND_PROSE` arm `continue`s past the loop's
    /// `validate_payload`, so the fence check has to happen inside that arm.
    /// A malformed ```` ```neige-block ```` fence in prose is refused by
    /// `track_report_guard::validate_body_fences` at the whole-body write
    /// ends (`ReportDocOp::Replace` / `::WriteMarkdown`), and since #1269
    /// the prose `::UpsertBlock` arm refuses it at the op layer too — via
    /// the stricter prose branch of `validate_block_content`, behind
    /// MCP/REST surfaces that already refused it (#971 / #990). Forking is
    /// a write end as well.
    #[test]
    fn fork_rejects_malformed_neige_fence_in_a_prose_block() {
        let prose = ReportBlock {
            id: "b_0002".into(),
            kind: "prose".into(),
            rev: 1,
            payload: json!({"markdown": "# A\n```neige-block app\nnot json\n```\n"}),
        };
        let error = prepare_fork_report("summary".into(), vec![prose], "source", "target")
            .err()
            .expect("malformed prose fence must abort the fork");
        assert!(
            matches!(&error, crate::error::CalmError::BadRequest(_)),
            "must be a 400, got: {error:?}"
        );
        let rendered = error.to_string();
        assert!(
            rendered.contains("invalid forked report block b_0002"),
            "error must name the offending block: {rendered}"
        );
        assert!(
            rendered.contains("neige-block"),
            "error must name the malformed fence: {rendered}"
        );
    }

    /// The scope fence for the check above: a prose block whose fences are
    /// well formed still forks. Without this, "reject the fork" would pass
    /// just as well as the real rule.
    #[test]
    fn fork_keeps_prose_blocks_with_well_formed_fences() {
        let prose = ReportBlock {
            id: "b_0003".into(),
            kind: "prose".into(),
            rev: 1,
            payload: json!({"markdown": "# A\n\nplain prose, no fence\n"}),
        };
        prepare_fork_report("summary".into(), vec![prose], "source", "target")
            .expect("well-formed prose must fork");
    }

    /// #1252 S2 — the structural door writes the JSON cache, the CRDT bytes and
    /// the task projection as one operation, and the projection sees this
    /// write's cache.
    ///
    /// The task block's `refs` point at the prose block of the *same* snapshot,
    /// so it can only resolve if the payload cache already holds this write.
    /// Swap the two statements inside `write_report_row_and_project_tx` and this
    /// test collects a `reference_missing` diagnostic instead.
    ///
    /// Formerly `fork_persist_helper_writes_cache_crdt_and_projection_together`,
    /// against `persist_initial_report_and_project_tasks_tx` in this file.
    #[tokio::test]
    async fn structural_door_writes_cache_crdt_and_projection_together() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let area = repo
            .area_create(NewArea {
                name: "fork-helper".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                area_id: area.id,
                title: "fork helper".into(),
                sort: None,
                cwd: "/tmp/fork-helper".into(),
                template_id: None,
                plugin_scope: None,
                template_input: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let report = repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                kind: "track-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
                title: None,
            })
            .await
            .unwrap();
        let blocks = vec![
            ReportBlock {
                id: "b_1234".into(),
                kind: "prose".into(),
                rev: 1,
                payload: json!({"markdown": "projection target"}),
            },
            ReportBlock {
                id: "b_abcd".into(),
                kind: "task".into(),
                rev: 1,
                payload: json!({
                    "key": "projected",
                    "kind": "codex",
                    "goal": "project the fork",
                    "refs": [format!("neige://wave/{}#b_1234", track.id)],
                    "no_gate_reason": "covered by helper behavior test",
                    "ready": true,
                    "released_by_user": true,
                    "declared_by": "spec"
                }),
            },
        ];
        let mut doc = ReportDoc::from_blocks_exact("forked", &blocks).unwrap();
        let (summary, body) = doc.project().unwrap();
        let mut payload = TrackReportPayload::new(summary, body);
        payload.blocks = Some(blocks.clone());
        let payload_value = serde_json::to_value(&payload).unwrap();
        let (declarations, diagnostics) =
            calm_types::report_blocks::tasks::project_task_declarations(&blocks);

        let pool = repo.sqlite_pool().unwrap();
        let mut tx = pool.begin().await.unwrap();
        let (updated, projection) = structural_init_report_tx(
            &mut tx,
            InitialReportTarget {
                report_card_id: report.id.as_str(),
                track_id: track.id.as_str(),
                payload: &payload,
                doc: &mut doc,
                declarations: &declarations,
                diagnostics: &diagnostics,
            },
        )
        .await
        .unwrap();

        assert_eq!(updated.payload, payload_value);
        let persisted: (String, bool) =
            sqlx::query_as("SELECT json(payload),body_crdt IS NOT NULL FROM cards WHERE id=?1")
                .bind(report.id.as_str())
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&persisted.0).unwrap(),
            payload_value
        );
        assert!(persisted.1, "fork helper omitted CRDT bytes");
        assert!(
            projection
                .diagnostics
                .iter()
                .flat_map(|verdict| &verdict.diagnostics)
                .all(|diagnostic| diagnostic.code != "reference_missing"),
            "fork projection diagnostics: {:?}",
            projection.diagnostics
        );
        let task_key: String =
            sqlx::query_scalar("SELECT key FROM tasks WHERE track_id=?1 AND key='projected'")
                .bind(track.id.as_str())
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(task_key, "projected");
        tx.rollback().await.unwrap();
    }

    /// Pins the full-write assumption the events retention pruner's
    /// keep-latest `overlay.set` carve-out depends on (#854 slice 2):
    /// `fold_layout_positions` ignores a positions-less `overlay.set`
    /// (`.or(current)`), so keeping only `MAX(id)` per overlay quad is
    /// fold-preserving only if every kernel-emitted `view/layout`
    /// `overlay.set` carries the complete positions map. This is that
    /// writer. See `calm_truth::events_prune` module docs.
    #[test]
    fn planner_harness_layout_payload_is_a_full_positions_write() {
        let payload = planner_harness_layout_payload("planner-1", "report-1");
        let positions = payload
            .get("positions")
            .and_then(|v| v.as_object())
            .expect("layout overlay.set payload must carry a full positions object");
        assert!(positions.contains_key("planner-1"));
        assert!(positions.contains_key("report-1"));
    }

    /// #891 / #1110 S2 — the create-time `template_input` validation
    /// matrix. Schema-conformance details are pinned in
    /// `plugin_host::template_input`; this covers the binding combinations
    /// against the owning plugin Manifest.
    mod template_input_binding {
        use super::super::validate_template_input_binding;
        use crate::error::CalmError;
        use crate::plugin_host::manifest::Manifest;
        use crate::plugin_host::template_input::TemplateInputOwner;
        use serde_json::{Value, json};

        /// The three `owner` axis values, as short constructors, so each test
        /// below reads as one cell of the matrix.
        fn owned(plugin: &Manifest) -> TemplateInputOwner<'_> {
            TemplateInputOwner::Plugin(plugin)
        }

        fn plugin(input_schema: Option<Value>) -> Manifest {
            let mut v = json!({
                "manifest_version": 2,
                "id": "dev.neige.git-forge",
                "version": "1.0.0",
                "min_kernel_version": "0.0.1",
                "display_name": "Git Forge",
                "entrypoint": { "command": "bin/x" },
                "templates": [{ "id": "issue-development" }]
            });
            if let Some(schema) = input_schema {
                v["input_schema"] = schema;
            }
            Manifest::parse(&v.to_string()).expect("test plugin manifest")
        }

        fn schema(required: Value) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "issue_url": { "type": "string" },
                    "merge_policy": {
                        "type": "string",
                        "enum": ["hold-for-ratify", "auto-merge"]
                    }
                },
                "required": required,
                "additionalProperties": false
            })
        }

        /// 第二轮评审 MINOR-2 — every 400 this matrix produces ships through
        /// `create_track`, and the route's own vocabulary (`track create: `) is
        /// part of the body. Nothing in the repository asserted that prefix:
        /// deleting it left the whole `--lib` suite green, because the needles
        /// used here are all substrings of the bare reason. It is asserted on
        /// every arm now, so the wrapper cannot silently stop wrapping.
        const ROUTE_PREFIX: &str = "track create: ";

        fn expect_bad_request(owner: TemplateInputOwner<'_>, input: Option<&Value>, needle: &str) {
            match validate_template_input_binding(owner, input) {
                Err(CalmError::BadRequest(message)) => {
                    assert!(
                        message.starts_with(ROUTE_PREFIX),
                        "400 body `{message}` must keep the route prefix `{ROUTE_PREFIX}`"
                    );
                    assert!(message.contains(needle), "message `{message}` ∌ `{needle}`");
                }
                other => panic!("expected BadRequest containing `{needle}`, got {other:?}"),
            }
        }

        #[test]
        fn input_without_template_id_is_rejected() {
            expect_bad_request(
                TemplateInputOwner::NoTemplateId,
                Some(&json!({ "x": 1 })),
                "requires `template_id`",
            );
        }

        /// 第二轮评审 NIT-3 — the other cause of "no owning Manifest": the
        /// `template_id` **was** given and the roster admits it, but no running
        /// ∧ trusted plugin declares it. This cell used to answer "requires
        /// `template_id`", i.e. it asked for the field the caller had already
        /// sent. Reachable in production — `create_time_and_run_time_binding_
        /// agree_for_a_stopped_owner` (`track_binding::tests`) drives it
        /// through the real route.
        #[test]
        fn input_with_a_template_whose_owner_is_not_running_names_that_cause() {
            let message = match validate_template_input_binding(
                TemplateInputOwner::NoBoundPlugin,
                Some(&json!({ "x": 1 })),
            ) {
                Err(CalmError::BadRequest(message)) => message,
                other => panic!("expected BadRequest, got {other:?}"),
            };
            assert!(message.starts_with(ROUTE_PREFIX), "{message}");
            assert!(message.contains("running and trusted"), "{message}");
            // 第三轮评审 MINOR — the *second* clause must stay narrow too.
            // `NoBoundPlugin` also covers a stopped/untrusted owner whose
            // Manifest is still in the registry and still declares this
            // template; a bare "no plugin declares this template" would be
            // false in exactly the case this test drives.
            assert!(
                message.contains("no running and trusted plugin declares this template"),
                "the cause clause must be scoped to running ∧ trusted, not to all \
                 plugins — a stopped owner still declares the template: {message}"
            );
            // The discriminating half: it must NOT tell the caller to supply a
            // `template_id` they already supplied.
            assert!(
                !message.contains("requires `template_id`"),
                "a stopped owner must not be reported as a missing template_id: {message}"
            );
        }

        #[test]
        fn no_template_no_input_is_ok() {
            validate_template_input_binding(TemplateInputOwner::NoTemplateId, None)
                .expect("plain track create unchanged");
            // Same for an admitted template with no live owner: nothing to
            // validate, and nothing to refuse.
            validate_template_input_binding(TemplateInputOwner::NoBoundPlugin, None)
                .expect("unowned template without input is not an error");
        }

        #[test]
        fn input_against_schema_less_plugin_is_rejected_fail_closed() {
            let p = plugin(None);
            expect_bad_request(owned(&p), Some(&json!({ "x": 1 })), "does not declare");
            expect_bad_request(owned(&p), Some(&json!({ "x": 1 })), "plugin");
        }

        #[test]
        fn schema_less_binding_without_input_stays_valid() {
            let p = plugin(None);
            validate_template_input_binding(owned(&p), None).expect("bound create unchanged");
        }

        #[test]
        fn missing_input_with_required_schema_is_rejected() {
            let p = plugin(Some(schema(json!(["issue_url"]))));
            expect_bad_request(owned(&p), None, "requires `template_input`");
            expect_bad_request(owned(&p), None, "issue_url");
        }

        #[test]
        fn missing_input_with_no_required_fields_is_ok() {
            let p = plugin(Some(schema(json!([]))));
            validate_template_input_binding(owned(&p), None).expect("optional input omitted");
        }

        #[test]
        fn input_is_validated_against_the_plugin_schema() {
            let p = plugin(Some(schema(json!(["issue_url"]))));
            validate_template_input_binding(
                owned(&p),
                Some(&json!({ "issue_url": "u", "merge_policy": "auto-merge" })),
            )
            .expect("conforming input accepted");
            // INV-1110-003 — missing required / extra key / enum still 400.
            expect_bad_request(
                owned(&p),
                Some(&json!({ "merge_policy": "auto-merge" })),
                "template_input.issue_url",
            );
            expect_bad_request(
                owned(&p),
                Some(&json!({ "issue_url": "u", "ghost": true })),
                "template_input.ghost",
            );
            expect_bad_request(
                owned(&p),
                Some(&json!({ "issue_url": "u", "merge_policy": "yolo" })),
                "template_input.merge_policy",
            );
        }
    }

    /// #1318 S2 (第一轮评审 F4) — `admit_template` itself, not the two ends of
    /// the chain it sits between.
    ///
    /// The first review round found the evidence chain broken exactly here.
    /// `templates::tests::template_by_key_returns_the_rosters_own_borrow`
    /// constrains the *lookup*, and `track_template_tracks::
    /// create_stores_the_roster_key_as_template_id` observes the *column*;
    /// neither can see the line in between (`key: template.key`). The reviewer
    /// ran the adversarial construction — a naive case-insensitive
    /// `admit_template` that reflects the caller's spelling back — and both of
    /// those tests stayed green (1188 passed).
    ///
    /// So this asserts the assignment by data-pointer identity, the one form
    /// the caller's string cannot satisfy: the fixture below is a freshly
    /// allocated `String` with identical bytes, so an equality assertion would
    /// pass for a reflected key and discriminate nothing.
    ///
    /// ## What this test can and cannot see (第二轮评审)
    ///
    /// The fixture is `String::from(template.key())` — byte-identical to the
    /// roster's spelling. It therefore only ever exercises the branch where
    /// the caller already spelled the key correctly, and an `admit_template`
    /// that reflects the caller's string **conditionally** (`if id ==
    /// template.key { template.key } else { String::leak(id.to_string()) }`)
    /// stayed green under it. That is a real limitation of a pointer-identity
    /// test fed an identical fixture, and its sister test
    /// `track_template_tracks::create_stores_the_roster_key_as_template_id`
    /// already said so about itself; this one did not.
    ///
    /// One *spelling* of the conditional mutation no longer compiles:
    /// [`TemplateAdmission`] holds the `&'static Template` and
    /// [`TemplateAdmission::key`] reads it, so there is no key assignment site
    /// to make conditional, and producing one would require a forged
    /// `Template` (`E0451` in safe Rust outside `crate::templates`). What this
    /// test buys is the **unconditional** direction and the negative case,
    /// cheaply, without depending on that reasoning being right.
    ///
    /// ## 第三轮评审 — the third-consumer claim is withdrawn
    ///
    /// This comment used to say the plugin binding "is closed by construction
    /// rather than excused", because it is resolved from the same
    /// `&'static Template` in the same expression that builds the admission.
    /// That is not a closure. `id` is in [`admit_template`]'s scope for both
    /// field initializers, so
    /// `binding: if id == template.key() { resolve_template_binding(..).await } else { None }`
    /// is ordinary safe Rust that forges nothing and leaves every test in this
    /// repository green. Two further constructions (a second roster entry
    /// point inside `crate::templates`, measured 68/68 green; a `transmute`
    /// forgery, measured `clippy -D warnings` clean) make the same point from
    /// other directions. All three are registered verbatim under
    /// `## KNOWN GAPS` on [`admit_template`]; the binding consumer is
    /// **untested here and knowingly so**, not closed.
    ///
    /// ## What the pointer assertion below does and does not discriminate
    ///
    /// It compares `admission.key()` against `template.key()` — both sides read
    /// through the same accessor, so on its own it only pins that the accessor
    /// is *pointer-stable*, not that it hands back the `static`'s bytes. The
    /// 第二轮 refactor introduced exactly that weakening (before it, the right
    /// side was the raw field), and a channel confirmed it by making `key()`
    /// return an interned leak: 68/68 green. The missing half now lives where
    /// the private field is nameable —
    /// `templates::tests::the_accessors_hand_back_the_roster_fields_own_buffer`
    /// — and the two together restore what the pre-refactor assertion said.
    mod admission {
        use std::path::Path;
        use std::sync::Arc;

        use axum::extract::FromRef;

        use crate::card_role_cache::CardRoleCache;
        use crate::db::sqlite::SqlxRepo;
        use crate::event::EventBus;
        use crate::plugin_host::{PluginHost, PluginRegistry};
        use crate::routes::tracks::admit_template;
        use crate::state::{AppState, CodexClient, DaemonClient, RouteState, WriteContext};
        use crate::templates::TEMPLATES;
        use crate::track_area_cache::TrackAreaCache;
        use calm_truth::db::Repo;

        async fn route_state() -> RouteState {
            let repo = Arc::new(
                SqlxRepo::open("sqlite::memory:")
                    .await
                    .expect("open in-memory sqlite"),
            );
            let repo_dyn: Arc<dyn Repo> = repo.clone();
            let events = EventBus::new();
            let roles = CardRoleCache::new();
            let tracks = TrackAreaCache::new();
            let state = AppState::from_parts(
                repo_dyn.clone(),
                events.clone(),
                Arc::new(DaemonClient {
                    data_dir: std::env::temp_dir().join("calm-admit-template-test"),
                    proc_supervisor_sock: None,
                }),
                Arc::new(PluginHost::new_full(
                    Arc::new(PluginRegistry::empty()),
                    repo_dyn,
                    Path::new("").to_path_buf(),
                    std::env::temp_dir().join("calm-admit-template-plugin-test"),
                    Vec::new(),
                    events,
                    WriteContext::new(roles.clone(), tracks.clone()),
                )),
                Arc::new(CodexClient::new_stub()),
                Some(roles),
                Some(tracks),
            );
            RouteState::from_ref(&state)
        }

        #[tokio::test]
        async fn admission_key_is_the_rosters_own_borrow() {
            let s = route_state().await;
            for template in &TEMPLATES {
                let caller_spelling = String::from(template.key());
                assert_ne!(
                    caller_spelling.as_ptr(),
                    template.key().as_ptr(),
                    "the fixture must not accidentally be the roster's own buffer"
                );
                let admission = admit_template(&s, caller_spelling.as_str())
                    .await
                    .unwrap_or_else(|| panic!("`{}` must be admitted", template.key()));
                assert_eq!(
                    admission.key(),
                    template.key(),
                    "`{}`: admitted key changed spelling",
                    template.key()
                );
                assert!(
                    std::ptr::eq(admission.key().as_ptr(), template.key().as_ptr()),
                    "`{}`: `TemplateAdmission::key` must be the roster's own \
                     &'static str, not a value derived from the caller's string",
                    template.key()
                );
            }
            assert!(
                admit_template(&s, "missing-template").await.is_none(),
                "a non-roster id must not be admitted"
            );
        }
    }
}
