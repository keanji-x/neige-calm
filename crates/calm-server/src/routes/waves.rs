//! `/api/waves`, `/api/areas/:id/waves` — Wave CRUD. **Owned by Track B.**
//!
//! Writes go through `Repo::write_with_event` (via the
//! `write_with_event_typed` ergonomic wrapper). See `routes/areas.rs` for
//! the migration pattern; this file follows the same shape.
//!
//! ## PR6 (#136) — atomic spec-card binding
//!
//! `create_wave` now mints a wave **and** a `CardRole::Spec` codex card
//! in a single transaction via [`crate::db::write_with_events_typed`].
//! Two events leave the tx: [`Event::WaveUpdated`] (scope = Wave) and
//! [`Event::CardAdded`] (scope = Card).
//!
//! ## Spec harness start
//!
//! Wave creation now mints the kernel-owned spec card and report card, then
//! submits the `spec-harness-start` operation. Start failures are non-fatal:
//! the committed wave remains and the spec card can recover through the
//! harness runtime.
//!
//! ## Wave-delete teardown (issue #197)
//!
//! `delete_wave` first performs a best-effort descendant preflight and
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
    MAX_TREE_TASK_BUDGET, TaskProjectionOutcome, WaveWorkspacePlan, area_folder_create_tx,
    area_folders_list_all_tx, card_create_with_id_tx, card_update_with_crdt_tx,
    overlay_delete_by_entity_tx, overlay_delete_card_overlays_by_wave_tx, overlay_upsert_tx,
    project_tasks_tx, terminal_delete_tx, wave_create_tx, wave_delete_tx, wave_update_tx,
};
use crate::db::{RepoRead, write_with_actor_events_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::forge_trust::trusted_forge_plugin;
use crate::ids::{ActorId, CardId, WaveId};
use crate::model::{
    AreaKind, Card, CardPatch, CardRole, FolderConflict, FolderConflictKind, NewCard, NewOverlay,
    NewWave, RequestTheme, Wave, WaveDetail, WavePatch, WaveWorkspace, WaveWorkspaceKind,
    WaveWorkspacePatch, new_id,
};
use crate::operation::spec_harness_start_adapter::SpecHarnessStartOperationPayload;
use crate::operation::workspace_lease::{
    release_workspace_leases_for_wave_tx, sweep_workspace_worktrees_for_waves_repo,
    wave_has_active_forge_action,
};
use crate::operation::{OperationKey, OperationOutcome};
use crate::plugin_host::manifest::Manifest;
use crate::plugin_host::template_input::validate_template_input;
use crate::report_backlinks;
use crate::routes::area_folders::{find_owner, is_descendant_of, normalize_path};
use crate::routes::cards::interrupt_shared_card_active_turn;
use crate::routes::codex_cards::default_cwd;
use crate::routes::terminal_cards::stable_payload_hash;
use crate::session_projection_lookup::project_runtime_into_cards_payload;
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};
use crate::templates::{template_by_key, template_report};
use crate::terminal_sweeper::reap_terminal_artifacts_with_renderer;
use crate::validation::{
    CODEX_PAYLOAD_SCHEMA_VERSION, OVERLAY_TEMPLATE_ENTITY_KIND, OVERLAY_TEMPLATE_KIND,
    OVERLAY_TEMPLATE_PLUGIN_ID, is_template_overlay, template_overlay_payload,
    validate_overlay_payload,
};
use crate::wave_fs_view::{WaveFsContent, WaveFsEntry, WaveFsView};
use crate::wave_lifecycle::{validate_transition, wave_get_tx};
use crate::wave_report::{
    self, ReportBlock, WaveReportPayload, report_blocks_snapshot_tx, resolve_report_for_wave,
    tasks_rebuild_tree_tx, tasks_rebuild_tx,
};
use crate::wave_report_doc::ReportDoc;
use crate::wave_report_read::load_report_read_snapshot;
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
use std::collections::HashSet;
use utoipa::{IntoParams, ToSchema};

#[cfg(feature = "fixtures")]
use std::collections::HashMap;
#[cfg(feature = "fixtures")]
use std::sync::{Mutex as StdMutex, OnceLock};
#[cfg(feature = "fixtures")]
use tokio::sync::Notify;

#[cfg(feature = "fixtures")]
fn chat_wave_ensure_barriers()
-> &'static StdMutex<HashMap<String, std::sync::Arc<tokio::sync::Barrier>>> {
    static BARRIERS: OnceLock<StdMutex<HashMap<String, std::sync::Arc<tokio::sync::Barrier>>>> =
        OnceLock::new();
    BARRIERS.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn install_chat_wave_ensure_barrier_for_test(
    area_id: &str,
    barrier: std::sync::Arc<tokio::sync::Barrier>,
) {
    chat_wave_ensure_barriers()
        .lock()
        .expect("chat-wave ensure barrier lock poisoned")
        .insert(area_id.to_string(), barrier);
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn remove_chat_wave_ensure_barrier_for_test(area_id: &str) {
    chat_wave_ensure_barriers()
        .lock()
        .expect("chat-wave ensure barrier lock poisoned")
        .remove(area_id);
}

#[cfg(feature = "fixtures")]
async fn wait_at_chat_wave_ensure_barrier(area_id: &str) {
    let barrier = chat_wave_ensure_barriers()
        .lock()
        .expect("chat-wave ensure barrier lock poisoned")
        .get(area_id)
        .cloned();
    if let Some(barrier) = barrier {
        barrier.wait().await;
    }
}

mod fork_guard;

use fork_guard::guard_forked_blocks;

#[derive(Clone)]
struct WaveDeletePlan {
    wave_id: WaveId,
    area_id: crate::ids::AreaId,
    cards: Vec<Card>,
    terminals: Vec<crate::model::Terminal>,
    active_runtime_ids: Vec<String>,
}

#[cfg(feature = "fixtures")]
#[derive(Clone)]
pub struct WaveDeleteTeardownHook {
    pub entered: std::sync::Arc<Notify>,
    pub release: std::sync::Arc<Notify>,
}

#[cfg(feature = "fixtures")]
fn wave_delete_teardown_hooks() -> &'static StdMutex<HashMap<String, WaveDeleteTeardownHook>> {
    static HOOKS: OnceLock<StdMutex<HashMap<String, WaveDeleteTeardownHook>>> = OnceLock::new();
    HOOKS.get_or_init(|| StdMutex::new(HashMap::new()))
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn install_wave_delete_teardown_hook_for_test(wave_id: &str, hook: WaveDeleteTeardownHook) {
    wave_delete_teardown_hooks()
        .lock()
        .expect("wave delete hook mutex")
        .insert(wave_id.to_string(), hook);
}

async fn wait_at_wave_delete_teardown_hook(wave_id: &str) {
    #[cfg(feature = "fixtures")]
    {
        let hook = wave_delete_teardown_hooks()
            .lock()
            .expect("wave delete hook mutex")
            .remove(wave_id);
        if let Some(hook) = hook {
            hook.entered.notify_one();
            hook.release.notified().await;
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = wave_id;
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateWaveRequest {
    #[schema(value_type = String)]
    pub area_id: crate::ids::AreaId,
    /// Issue #1211 — on this user-driven create path the title is no longer
    /// the wave's intent, so the client may omit it entirely. Omitting it
    /// stores the **empty string** — there is no server-side default; the
    /// `Untitled wave` a user sees in a list is the frontend's display
    /// fallback (`fe/core/domain/wave.ts` `UNTITLED_WAVE_LABEL`). The spec
    /// agent then names the wave via `calm.wave.rename`, which only succeeds
    /// while the stored title is still blank. The type
    /// stays `String`: the empty string has always been a legal title and the
    /// server applies no non-empty validation.
    #[serde(default)]
    #[schema(required = false)]
    pub title: String,
    pub sort: Option<f64>,
    /// Issue #1131 — omitted / null → persist `default_cwd()` (`$HOME`, else
    /// process cwd) on the wave row and skip `area_folders`. Present values
    /// (including the empty string) keep the pre-#1131 absolute-path + claim
    /// rules. The SQLite column stays NOT NULL; only the request field is
    /// optional.
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub template_input: Option<serde_json::Value>,
    #[serde(default)]
    pub attach_folder: bool,
    pub theme: RequestTheme,
    /// One-time creation instruction: copy this wave's report snapshot into
    /// the new report inside the wave-create transaction.
    #[serde(default)]
    pub fork_report_from: Option<String>,
    /// When true, upsert the kernel view/template overlay in the same create
    /// transaction as the layout overlay and do not start the spec harness.
    #[serde(default)]
    pub as_template: bool,
}

impl CreateWaveRequest {
    /// `(body, fork_report_from, cwd_omitted, as_template)`. `cwd_omitted` is
    /// true when the client sent no `cwd` / `null`; that is a different branch
    /// from an explicit empty string, which still 400s.
    fn into_parts(self) -> (NewWave, Option<String>, bool, bool) {
        let cwd_omitted = self.cwd.is_none();
        (
            NewWave {
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
            cwd_omitted,
            self.as_template,
        )
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/waves", get(list_waves_window).post(create_wave))
        .route(
            "/api/waves/{id}",
            get(get_wave_detail).patch(update_wave).delete(delete_wave),
        )
        // Issue #247 PR3 — user-facing wave-report edit endpoint. Session-
        // authenticated; only `ActorId::User` is accepted (worker / spec /
        // plugin actors are rejected 403 even when carrying a valid
        // session cookie). The MCP `calm.report.{write,edit}` path is
        // unchanged; both paths funnel through the `wave_report::write`
        // module — different entry points, one private writer — so the
        // dual-event invariant + CRDT write stays one boundary.
        .route(
            "/api/waves/{id}/report",
            get(get_wave_report).post(update_wave_report),
        )
        .route("/api/waves/{id}/backlinks", get(get_wave_backlinks))
        .route("/api/waves/{id}/files/ls", get(list_wave_files))
        .route("/api/waves/{id}/files/cat", get(cat_wave_file))
        .route("/api/areas/{area_id}/waves", get(list_waves_by_area))
        .route(
            "/api/areas/{area_id}/chat-wave/ensure",
            axum::routing::post(ensure_area_chat_wave),
        )
}

fn is_unique_constraint(error: &CalmError, constraint: &str) -> bool {
    let CalmError::Db(sqlx::Error::Database(error)) = error else {
        return false;
    };
    error.is_unique_violation() && error.message().contains(constraint)
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn is_unique_constraint_for_test(error: &CalmError, constraint: &str) -> bool {
    is_unique_constraint(error, constraint)
}

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct WaveFsLsQuery {
    /// Logical path to list. Omitted or `/` lists the wave root.
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct WaveFsCatQuery {
    /// Logical path to read. Required.
    pub path: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/waves/{id}/files/ls",
    tag = "waves",
    params(("id" = String, Path, description = "Wave id"), WaveFsLsQuery),
    responses(
        (status = 200, description = "Wave file view directory entries", body = Vec<WaveFsEntry>),
        (status = 400, description = "Logical path not available", body = ErrorBody),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 403, description = "Referenced card is outside the wave", body = ErrorBody),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
// NOTE: no `Principal` extractor here.
//
// `update_wave_report` (POST) keeps `_principal: Principal` as an implicit
// session-middleware assertion — the route fires on user action, never
// during a11y/replay traffic. These GET routes fire on every wave page
// mount (the report sidebar lists root on first render); the replay
// binary intentionally does NOT attach `require_session` so its a11y
// suite can drive REST without a session, and a `Principal` extractor
// here would surface as a 401 → SessionProvider redirect → login page
// during a11y replay runs. The TODO below keeps the multi-user
// ownership hook visible without breaking the no-auth surface contract.
//
// TODO(#573 multi-user): ownership check
pub(crate) async fn list_wave_files(
    State(s): State<RouteState>,
    Path(id): Path<String>,
    Query(q): Query<WaveFsLsQuery>,
) -> Result<Json<Vec<WaveFsEntry>>> {
    let wave = s
        .repo
        .wave_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;
    // TODO(#573 multi-user): ownership check
    let view = WaveFsView::new(s.repo.as_ref(), &s.write);
    let entries = view.ls(&wave, q.path.as_deref()).await?;
    Ok(Json(entries))
}

#[utoipa::path(
    get,
    path = "/api/waves/{id}/files/cat",
    tag = "waves",
    params(("id" = String, Path, description = "Wave id"), WaveFsCatQuery),
    responses(
        (status = 200, description = "Wave file view content", body = WaveFsContent),
        (status = 400, description = "Missing path or logical path not available", body = ErrorBody),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 403, description = "Referenced card is outside the wave", body = ErrorBody),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
// See note on `list_wave_files` for why `Principal` is intentionally NOT
// extracted here. The `TODO(#573 multi-user)` lives next to `list_wave_files`.
pub(crate) async fn cat_wave_file(
    State(s): State<RouteState>,
    Path(id): Path<String>,
    Query(q): Query<WaveFsCatQuery>,
) -> Result<Json<WaveFsContent>> {
    let path = q
        .path
        .as_deref()
        .ok_or_else(|| CalmError::BadRequest("calm.wave.cat: missing `path` (string)".into()))?;
    let wave = s
        .repo
        .wave_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;
    // TODO(#573 multi-user): ownership check
    let view = WaveFsView::new(s.repo.as_ref(), &s.write);
    let content = view.cat(&wave, path).await?;
    Ok(Json(content))
}

#[utoipa::path(
    get,
    path = "/api/areas/{area_id}/waves",
    tag = "waves",
    params(("area_id" = String, Path, description = "Area id")),
    responses(
        (status = 200, description = "Waves under area", body = Vec<Wave>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_waves_by_area(
    State(s): State<RouteState>,
    Path(area_id): Path<String>,
) -> Result<Json<Vec<Wave>>> {
    let mut waves = s.repo.waves_by_area(&area_id).await?;
    retain_user_visible_waves(s.repo.as_ref(), &mut waves).await?;
    Ok(Json(waves))
}

/// Public wave lists hide the area conversation container and template waves
/// (#1110 S1). Keep this at the route boundary: repository readers such as
/// area deletion and backlink resolution require the complete set.
///
/// The `match` is spelled out rather than written `!= Some(AREA_CHAT_PURPOSE)`
/// purely for readability — both forms already keep NULL-purpose waves
/// visible, because Rust comparison against `Option` is total. The three-valued
/// logic trap this must not be confused with lives in SQL, where
/// `purpose <> 'area-chat'` drops NULL rows; the two hand-written predicates
/// that must spell out `purpose IS NULL OR ...` are in `session_repo_impl.rs`.
fn user_visible_wave(wave: &Wave) -> bool {
    match wave.purpose.as_deref() {
        None => true,
        Some(purpose) => purpose != AREA_CHAT_PURPOSE,
    }
}

async fn retain_user_visible_waves(repo: &dyn RepoRead, waves: &mut Vec<Wave>) -> Result<()> {
    let templates = template_wave_ids(repo).await?;
    waves.retain(|wave| user_visible_wave(wave) && !templates.contains(wave.id.as_str()));
    Ok(())
}

async fn template_wave_ids(repo: &dyn RepoRead) -> Result<HashSet<String>> {
    Ok(repo
        .overlays_by_kind(OVERLAY_TEMPLATE_ENTITY_KIND)
        .await?
        .into_iter()
        .filter(is_template_overlay)
        .map(|overlay| overlay.entity_id)
        .collect())
}

/// Build the initial report a template instantiates to.
///
/// #1300 — this replaces `ensure_templates` / `lookup_template_wave` /
/// `seed_template_wave` / `restamp_template_report_if_placeholder`. Those
/// lazily minted three hidden system-area waves and `POST /api/waves` then
/// forked one of them, which made a template a kind of wave. It is a read-only
/// recipe: instantiating it is structural initialization of a new wave, and it
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
/// at all (`wave_report_edit_guard.rs`), and every template report is a page of
/// them. That guard is not an obstacle to route around — refusing to let
/// non-humans declare tasks is its entire purpose.
///
/// So this is not a report *edit* with a better-chosen author; it is the same
/// structural initialization the fork path performs, on the same in-transaction
/// writer, with no author to name because no one is editing anything. That is
/// also why the constants can now declare `spec` directly
/// (`templates::report_from_tasks`) instead of writing `user` and having the
/// fork rewrite it one step later.
///
/// ## The single validation, and why there is not a second one
///
/// [`crate::wave_report_guard::validate_body_fences`] is not only a fence-shape
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
/// because its input is another wave's user content.)
fn prepare_template_report(key: &str) -> Result<InitialReportSnapshot> {
    let payload = template_report(key)
        .ok_or_else(|| CalmError::Internal(format!("wave create: unknown template `{key}`")))?;
    prepare_initial_report_payload(key, payload)
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
    payload: WaveReportPayload,
) -> Result<InitialReportSnapshot> {
    crate::wave_report_guard::validate_body_fences(&payload.body).map_err(|error| {
        CalmError::Internal(format!("wave create: template `{label}` body: {error}"))
    })?;
    let doc = ReportDoc::from_payload(&payload);
    let blocks = doc.blocks_snapshot().map_err(|error| {
        CalmError::Internal(format!("wave create: template `{label}` blocks: {error}"))
    })?;
    let (summary, body) = doc.project().map_err(|error| {
        CalmError::Internal(format!("wave create: project template `{label}`: {error}"))
    })?;
    let (declarations, diagnostics) =
        calm_types::report_blocks::tasks::project_task_declarations(&blocks);
    let mut prepared = WaveReportPayload::new(summary, body);
    prepared.blocks = Some(blocks);
    Ok((prepared, doc, declarations, diagnostics))
}

/// Issue #250 PR 2 — calendar window query parameters for
/// `GET /api/waves`. Every field is optional so omitting all three
/// degenerates to "every wave in the DB" (the route delegates to
/// `Repo::waves_window` which builds the SQL `WHERE` clause from the
/// non-`None` subset).
///
/// The semantic for `since` + `until` is **inclusive at both
/// endpoints**:
///   * `created_at <= until`  — exclude waves that hadn't been created
///     yet by the right edge of the window.
///   * `terminal_at IS NULL OR terminal_at >= since` — include any
///     wave that's still open (never reached a terminal lifecycle
///     state) or whose terminal stamp lands inside / past the left
///     edge.
///
/// Together the two predicates implement the "the wave is visible on
/// at least one day inside `[since, until]`" calendar contract from
/// the issue, even when the wave hasn't terminated yet.
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct WavesWindowQuery {
    /// Lower bound (inclusive) in unix milliseconds. Wave is included
    /// when `terminal_at IS NULL OR terminal_at >= since`. Omitting
    /// disables the lower-bound filter.
    pub since: Option<i64>,
    /// Upper bound (inclusive) in unix milliseconds. Wave is included
    /// when `created_at <= until`. Omitting disables the upper-bound
    /// filter.
    pub until: Option<i64>,
    /// Optional per-area filter. Mirrors `list_waves_by_area` for
    /// callers that want one area's window in a single endpoint.
    pub area_id: Option<String>,
}

/// Issue #250 PR 2 — calendar / dashboard window query.
///
/// `GET /api/waves?since=<ms>&until=<ms>&area_id=<id>` — every
/// parameter is optional. Returns the full wave row (so the frontend
/// can render lifecycle / area / terminal-at without an N+1 detail
/// fetch). Pre-#250 callers that hit `GET /api/waves` would 405 on
/// the old `POST`-only route; this is an additive contract.
#[utoipa::path(
    get,
    path = "/api/waves",
    tag = "waves",
    params(WavesWindowQuery),
    responses(
        (status = 200, description = "Waves overlapping the window, sorted by created_at", body = Vec<Wave>),
        (status = 400, description = "Inverted window (since > until)", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_waves_window(
    State(state): State<RouteState>,
    Query(q): Query<WavesWindowQuery>,
) -> Result<Json<Vec<Wave>>> {
    if let (Some(since), Some(until)) = (q.since, q.until)
        && since > until
    {
        return Err(CalmError::BadRequest(format!(
            "window query: `since` ({since}) must be <= `until` ({until})"
        )));
    }
    let mut waves = state
        .repo
        .waves_window(q.area_id.as_deref(), q.since, q.until)
        .await?;
    retain_user_visible_waves(state.repo.as_ref(), &mut waves).await?;
    Ok(Json(waves))
}

#[utoipa::path(
    get,
    path = "/api/waves/{id}",
    tag = "waves",
    params(("id" = String, Path, description = "Wave id")),
    responses(
        (status = 200, description = "Wave detail (wave + its cards + overlays)", body = WaveDetail),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn get_wave_detail(
    State(s): State<RouteState>,
    Path(id): Path<String>,
) -> Result<Json<WaveDetail>> {
    let mut detail = s
        .repo
        .wave_detail(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;
    // Tier A read-side guard (issue #198 concern 4) — mirror `list_overlays`
    // so kernel-owned overlay rows with a `schemaVersion` past what this
    // binary supports never reach the frontend through the wave detail
    // route. This is the primary path the frontend uses to render
    // status/progress/eta/now overlays for a wave (`adaptWave(detail.wave,
    // detail.overlays)` in `web/src/app/router.tsx`); without this filter a
    // future-version row written by a newer kernel binary would defeat the
    // PR #214 guard for the wave-rendering path while still being correctly
    // filtered from `GET /api/overlays`. PR #214 review follow-up.
    detail.overlays = crate::routes::overlays::filter_unsupported_overlay_versions(detail.overlays);
    project_runtime_into_cards_payload(s.repo.as_ref(), &mut detail.cards).await?;
    Ok(Json(detail))
}

#[utoipa::path(
    post,
    path = "/api/waves",
    tag = "waves",
    request_body = CreateWaveRequest,
    responses(
        (status = 201, description = "Wave created", body = Wave),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn create_wave(
    State(s): State<RouteState>,
    actor: Actor,
    Json(request): Json<CreateWaveRequest>,
) -> Result<Response> {
    let (mut p, fork_report_from, cwd_omitted, as_template) = request.into_parts();
    // PR6 (#136) — wave create now atomically mints a `CardRole::Spec`
    // codex card alongside the wave row. Both rows commit in one tx
    // and both `Event::WaveUpdated` + `Event::CardAdded` envelopes
    // emit from the same commit, each tagged with its own scope so
    // per-wave and per-card subscribers each see the relevant frame
    // without re-routing through ancestors.
    //
    // Issue #250 PR 2 — the body may carry `cwd` (the wave's working
    // directory) and `attach_folder`. When `cwd` is present, it is the
    // source of truth for the spec daemon's working directory and must
    // either resolve to the body's `area_id` via the existing folder
    // claims, or — when `attach_folder = true` — get atomically claimed
    // as a new folder under that area inside the same tx that mints the
    // wave row.
    //
    // Issue #1131 — when the client omits `cwd` (new FE title-only
    // create), persist `default_cwd()` and skip the claim scan entirely.
    // Legacy clients that still send `cwd` keep the #250 rules.

    // 0. Validate cwd up front before opening the tx. The route owns
    //    every cross-area correctness check so the inner writer
    //    (`wave_create_tx`) stays a pure mechanical row insert. Order:
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
    //    the create transaction (`WaveInit::Template` →
    //    `prepare_template_report`, in the closure `create_wave_structure`
    //    runs). So the folder-claim 409, the in-transaction 400s for an
    //    explicit `fork_report_from` (source missing / cross-area) and the
    //    in-transaction 500s now all roll the *whole* create back, template
    //    report included — there is nothing left behind for them to leave.
    //
    //    One failure still is not covered by that rollback, and it is the
    //    reason "non-201 ⇒ no side effect" is not a property of this handler:
    //    `materialize_workspace` runs *after* the transaction commits (the
    //    managed path is derived from the wave id) and returns non-2xx with
    //    the wave already persisted. Pinned by
    //    `materialize_failure_fails_the_create`.

    // #1209 — one lookup. The template is the concept; a plugin binding is an
    // attribute of it, not a second way in. Roster membership is the whole
    // admission test: whether some plugin claims the id, and whether that
    // plugin is running and trusted, cannot change the answer.
    let admission = match p.template_id.as_deref() {
        Some(template_id) => Some(admit_template(&s, template_id).await.ok_or_else(|| {
            CalmError::BadRequest(format!(
                "wave create: `template_id` must reference a known wave template; got `{template_id}`"
            ))
        })?),
        None => None,
    };
    // The binding is read off the admitted template; the route no longer digs
    // through the registry a second time.
    let bound_plugin = admission.as_ref().and_then(|a| a.binding.as_ref());
    // #891 / #1110 S2 — `template_input` is only accepted against a bound
    // template whose owning plugin Manifest declares an `input_schema`;
    // validated here, before any DB write, so the inner writer persists
    // the blob verbatim. Still requires `template_id` this slice
    // (S5 deletes the template entity).
    validate_template_input_binding(bound_plugin, p.template_input.as_ref())?;
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
            "wave create: `cwd` must be absolute (start with `/`); got `{}`",
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
    // *after* the wave transaction commits (the managed path needs the wave
    // id), and `materialize_failure_fails_the_create` pins the consequence: a
    // failure there leaves an orphan wave row behind. Validating an attached
    // target needs none of that ordering — the path came in the request — so
    // it happens here, where the answer is a 400 and no row exists at all.
    // `materialize_workspace` checks it again as the single contract point for
    // every other create entry.
    if !cwd_omitted {
        crate::workspace_materialize::validate_attached_workspace(std::path::Path::new(
            &normalized_cwd,
        ))?;
    }
    // Stamp the normalized cwd back onto the body before the wave row
    // is minted — the `area_folder.path` we may attach below is also
    // the normalized form, so storing them in the same shape keeps
    // future "resolve by exact cwd" lookups simple.
    p.cwd = normalized_cwd.clone();

    // Issue #250 PR 2 fix — system area (kernel-internal scaffolding,
    // hosts the default Today terminal's wave) is exempt from the
    // area_folders claim namespace. The user can't reach it through
    // any user-facing surface, and claiming a path under it (e.g. the
    // initial `/` placeholder useTodayTerminal used) would poison
    // every real area's descendant check. Look up the kind once here;
    // if System, skip both the pre-tx folder validation and the
    // in-tx attach. The cwd is still recorded on the wave row (the
    // spec daemon chdirs into it) but no `area_folders` row is minted.
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
    // inside the wave-create transaction. It used to be a pre-tx scan on a
    // separate pooled connection, which let two concurrent creates for
    // `/a` and `/a/b` both pass an empty-table scan and commit overlapping
    // claims — `UNIQUE(area_folders.path)` only rejects *equal* paths.
    // Overlapping rows made the two resolvers disagree, and the wave
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
    // three hidden system-area waves *first* (`ensure_templates`) and only then
    // consulted `fork_report_from`, so the combination wrote rows it then did
    // not use. Instantiating a recipe reads nothing and writes nothing outside
    // the create transaction.
    //
    // #1209 placed the seed here — after the cwd shape check, the
    // attached-workspace check and the area 404 — so none of those 4xx left
    // freshly minted waves behind. Nothing is minted here any more, so that
    // ordering constraint is gone with the seeding it constrained.
    let init = match (&admission, fork_report_from) {
        (_, Some(source_wave_id)) => WaveInit::Fork { source_wave_id },
        (Some(admission), None) => WaveInit::Template { key: admission.key },
        (None, None) => WaveInit::Blank,
    };

    let workspace_root = s.workspace_root.clone();
    let created = create_wave_with_spec_harness(
        s,
        actor,
        p,
        CreateWaveOptions {
            folder_claim,
            body_area_id,
            normalized_cwd,
            init,
            as_template,
            // #1147 S2 — omitting `cwd` (the #1131 title-only create, i.e. what
            // the new FE sends) is the managed-default branch: the server picks
            // the directory. An explicit `cwd` is the attached branch and keeps
            // the #250 claim rules above verbatim.
            workspace_plan: if cwd_omitted {
                WaveWorkspacePlan::ManagedUnder(workspace_root)
            } else {
                WaveWorkspacePlan::AttachedFromCwd
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

/// #1209 — the single answer to "may this id create a wave", plus the optional
/// plugin binding that comes with it.
///
/// The word *admission* is the point: this answers **admission**, not "what
/// does the template look like". The authority for the latter is
/// `templates::template_report`, a Rust constant — which is why there is no
/// `title` and no report here. (#1300: before S2 the authority was a seeded
/// system-area template wave found by a database lookup, and this sentence
/// named it. Both the wave and the lookup are gone.)
pub(crate) struct TemplateAdmission {
    /// The roster's own `&'static` key, not the caller's string.
    ///
    /// It reaches exactly one consumer: `WaveInit::Template { key }`, i.e. the
    /// **recipe lookup** (`templates::template_report`) inside the create
    /// transaction. That side is what this borrow protects — a future
    /// case-folding or aliasing admission rule cannot hand an unnormalized key
    /// to `template_report`, because the value passed on is the roster's, not
    /// the caller's.
    ///
    /// It does **not** reach the wave row. `CreateWaveRequest::into_parts`
    /// puts the caller's original `template_id` string on `NewWave` (`:247`),
    /// and that is what `wave_create_tx` binds into `waves.template_id`. The
    /// two are identical only because `template_by_key` is an exact match
    /// today; the very rule this field guards against would separate them —
    /// `"SMALL-CHANGE"` admitted against roster key `"small-change"` would
    /// instantiate the right recipe and store `"SMALL-CHANGE"` on the row.
    /// Normalizing the stored id is a behaviour change and deliberately not
    /// one this field makes.
    pub key: &'static str,
    /// The owning plugin, when a running trusted one claims this id. `None` is
    /// an ordinary template, not a rejection.
    pub binding: Option<Manifest>,
}

/// Admit a caller-supplied `template_id`.
///
/// Roster membership is the only admission test; the binding is resolved
/// afterwards purely to be carried along. There is deliberately no fallback
/// arm here: a running trusted plugin declaring an id the roster does not have
/// gets `None`, i.e. a 400. That is the whole of #1209 — see §5 of
/// `docs/architecture/1209-template-workflow-unify.md` for why the alternative
/// (admitting it as a report-less pseudo-template) was rejected.
pub(crate) async fn admit_template(s: &RouteState, id: &str) -> Option<TemplateAdmission> {
    let template = template_by_key(id)?;
    Some(TemplateAdmission {
        key: template.key,
        binding: resolve_template_binding(s, id).await,
    })
}

/// Resolve `template_id` to the owning plugin Manifest iff a running
/// **trusted** plugin registers it — same filter as
/// `bound_template_descriptor` on the spec harness side. `None` covers
/// unknown, stopped, and untrusted templates alike (the route
/// deliberately does not distinguish them in the 400).
pub(crate) async fn resolve_template_binding(
    s: &RouteState,
    template_id: &str,
) -> Option<Manifest> {
    let running_plugin_ids = s.plugin.running_plugin_ids().await;
    s.plugin.registry().list().into_iter().find(|manifest| {
        running_plugin_ids.contains(&manifest.id)
            && trusted_forge_plugin(&manifest.id)
            && manifest
                .templates
                .iter()
                .any(|template| template.id == template_id)
    })
}

/// #891 / #1110 S2 — create-time `template_input` validation matrix.
/// Fail-closed: input is only accepted when the bound plugin Manifest
/// declares an `input_schema`, and a schema with required fields makes
/// input mandatory. The kernel never applies schema `default`s — the
/// value persists exactly as the caller sent it. Descriptor-level
/// `input_schema` is never consulted.
fn validate_template_input_binding(
    plugin: Option<&Manifest>,
    input: Option<&serde_json::Value>,
) -> Result<()> {
    let Some(plugin) = plugin else {
        if input.is_some() {
            return Err(CalmError::BadRequest(
                "wave create: `template_input` requires `template_id`".into(),
            ));
        }
        return Ok(());
    };
    let plugin_id = &plugin.id;
    match (plugin.input_schema.as_ref(), input) {
        (None, None) => Ok(()),
        (None, Some(_)) => Err(CalmError::BadRequest(format!(
            "wave create: plugin `{plugin_id}` does not declare an input_schema; \
             `template_input` is not accepted"
        ))),
        (Some(schema), None) => {
            let required: Vec<&str> = schema
                .get("required")
                .and_then(serde_json::Value::as_array)
                .map(|keys| keys.iter().filter_map(serde_json::Value::as_str).collect())
                .unwrap_or_default();
            if required.is_empty() {
                Ok(())
            } else {
                Err(CalmError::BadRequest(format!(
                    "wave create: plugin `{plugin_id}` requires `template_input` \
                     (required: {required:?})"
                )))
            }
        }
        (Some(schema), Some(input)) => validate_template_input(schema, input)
            .map_err(|reason| CalmError::BadRequest(format!("wave create: {reason}"))),
    }
}

/// Issue #275 — the cwd claim scan runs **inside** the wave-create
/// transaction, so its structured 409 (`FolderConflict`, not the generic
/// `{error, code}` envelope) has to travel back out through `Err`. The
/// closure parks the body here; [`create_wave`] picks it up and renders
/// it. `Mutex` is only ever locked between `await` points.
#[derive(Clone, Default)]
struct FolderConflictSlot(std::sync::Arc<std::sync::Mutex<Option<FolderConflict>>>);

impl FolderConflictSlot {
    /// Park `body` and return the error that unwinds (and rolls back)
    /// the transaction. The message is a fallback only: the route reads
    /// the slot first and never surfaces this string.
    fn park(&self, body: FolderConflict) -> CalmError {
        let message = format!(
            "wave create: cwd conflicts with folder claim `{}` (area `{}`)",
            body.conflict_path, body.area_id
        );
        *self.0.lock().expect("folder conflict slot poisoned") = Some(body);
        CalmError::Conflict(message)
    }

    fn take(&self) -> Option<FolderConflict> {
        self.0.lock().expect("folder conflict slot poisoned").take()
    }
}

/// Issue #275 — what the wave-create transaction does about `area_folders`.
#[derive(Clone)]
enum FolderClaim {
    /// Don't scan, don't insert. The system area is exempt from the claim
    /// namespace entirely, and `ensure_area_chat_wave_inner` derives its
    /// cwd from claims that already exist.
    Skip,
    /// Scan inside the wave tx (`BEGIN IMMEDIATE`, so scan and insert are
    /// atomic against a concurrent claim) and act on the result:
    /// `attach` mints the claim when nothing covers the cwd; without it a
    /// cwd no area claims is refused rather than making a homeless wave.
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
    /// #1147 S3 — `PATCH /api/waves/{id}` pointing a wave at an existing
    /// repository.
    Repoint,
}

impl FolderClaimIntent {
    fn label(self) -> &'static str {
        match self {
            FolderClaimIntent::Create => "wave create",
            FolderClaimIntent::Repoint => "wave workspace",
        }
    }
}

/// #1147 S3 — whether this pass may actually mint a `area_folders` row.
///
/// The re-point runs the claim rules **twice**, and the first pass must not
/// write. Its transaction commits (it is also the fence), so a claim minted
/// there survives a later refusal — and the re-point can still refuse after it,
/// on the pre-move re-check. That would leave the caller a 409 plus a claim
/// they never got a wave for, in a route whose whole promise is "a refusal
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
/// Extracted verbatim from `create_wave_structure` by #1147 S3 so that pointing
/// an existing wave at a directory obeys exactly the same rules as creating one
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
        // scan/insert TOCTOU. Pinned by `post_api_waves_attach_folder_*` in
        // `tests/cases/wave_cwd_terminal_at.rs`.
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
        // "homeless" wave.
        None => Err(CalmError::Conflict(format!(
            "{}: cwd `{normalized_cwd}` is not claimed by any area. Set \
             `attach_folder: true` to claim it for area `{area_id}`.",
            intent.label()
        ))),
    }
}

/// Where a new wave's report comes from.
///
/// #1300 — this used to be `fork_report_from: Option<String>`, and "create from
/// a template" was expressed *through* it: the route lazily seeded three hidden
/// system-area waves and then forked one of them. That made a template a kind
/// of wave, which is the thing #1300 removes. A template is a read-only recipe;
/// instantiating it is structural initialization of a new wave, not a copy of
/// an existing one.
///
/// The two data-carrying variants deliberately stay distinct rather than
/// collapsing into "some report snapshot". They share the *mechanism* below —
/// `prepare_*` produces a snapshot, one in-transaction writer persists it and
/// projects the tasks — but not the semantics: `Fork` copies a live wave and
/// must rewrite its links and re-attribute its blocks, while `Template`
/// constructs from a constant that has no wave to rewrite links against.
enum WaveInit {
    /// No report content; the wave keeps the default skeleton.
    Blank,
    /// Instantiate a template recipe. The roster's own `&'static` key, never
    /// the caller's string — see [`TemplateAdmission::key`].
    Template { key: &'static str },
    /// Copy an existing wave's report.
    Fork { source_wave_id: String },
}

struct CreateWaveOptions {
    folder_claim: FolderClaim,
    body_area_id: String,
    normalized_cwd: String,
    init: WaveInit,
    as_template: bool,
    /// #1147 S2 — managed (server allocates under the workspace root) vs
    /// attached (the caller pointed at an existing directory). Decided by
    /// each create entry point; `create_wave_structure` materializes the
    /// managed case right after the transaction commits.
    workspace_plan: WaveWorkspacePlan,
}

#[allow(deprecated)]
async fn create_wave_with_spec_harness(
    s: RouteState,
    actor: Actor,
    p: NewWave,
    options: CreateWaveOptions,
) -> Result<Response> {
    let as_template = options.as_template;
    let (wave, _, spec_card_id, report_card_id) =
        create_wave_structure(s.clone(), actor.clone(), p, options, None).await?;
    if !as_template {
        start_spec_harness(&s, &actor, &wave, spec_card_id, report_card_id).await?;
    }
    Ok((StatusCode::CREATED, Json(wave)).into_response())
}

/// Ensure the area's single chat wave exists.
///
/// The workspace is selected only while creating the wave. When the area has
/// folder claims it is the claimed path with the fewest path components,
/// breaking ties lexicographically (attached semantics: the user pointed at
/// that directory). Area folder claims cannot be equal, ancestors, or
/// descendants of one another, so "closest to the area root" is defined here as
/// this deterministic shallow path ordering rather than containment.
///
/// #1147 D10 — an area with **no** claim gets a managed default instead of the
/// 409 this used to return. Since #1109 made areas pure namespaces, "no claim"
/// is the normal state of a new area, so that 409 made
/// `POST /api/areas/{id}/conversations` fail by definition for every new area.
///
/// Once created, later folder claims or changes deliberately do not update the
/// wave's workspace, so an existing conversation cannot drift between working
/// directories from one message to the next.
#[utoipa::path(
    post,
    path = "/api/areas/{area_id}/chat-wave/ensure",
    tag = "waves",
    params(("area_id" = String, Path, description = "Area id")),
    responses(
        (status = 200, description = "Existing chat wave", body = Wave),
        (status = 201, description = "Chat wave created", body = Wave),
        // #1147 D10 removed the "area has no claimed folder" 409: a claimless
        // area now gets a managed default. Do not re-add a 409 here without a
        // branch that can actually produce one.

        (status = 404, description = "Area not found", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn ensure_area_chat_wave(
    State(s): State<RouteState>,
    actor: Actor,
    Path(area_id): Path<String>,
) -> Result<Response> {
    let (wave, created) = ensure_area_chat_wave_inner(&s, actor, &area_id).await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(wave)).into_response())
}

/// The body of [`ensure_area_chat_wave`], shared with
/// `POST /api/areas/{area_id}/conversations` (#1098 slice 3). Returns the
/// wave and whether this call is the one that created it.
///
/// Both callers must agree byte-for-byte on the cwd rule and the concurrent
/// ensure race resolution, so there is exactly one implementation of them.
#[allow(deprecated)]
pub(crate) async fn ensure_area_chat_wave_inner(
    s: &RouteState,
    actor: Actor,
    area_id: &str,
) -> Result<(Wave, bool)> {
    let area_id = area_id.to_string();
    if s.repo.area_get(&area_id).await?.is_none() {
        return Err(CalmError::NotFound(format!("area {area_id}")));
    }
    // Preserve the existing wave (and therefore its cwd) before consulting
    // current folder claims. The partial unique index closes the concurrent
    // ensure race; the loser reads and returns the winner below.
    if let Some(wave) = s
        .repo
        .waves_by_area(&area_id)
        .await?
        .into_iter()
        .find(|wave| wave.purpose.as_deref() == Some(AREA_CHAT_PURPOSE))
    {
        return Ok((wave, false));
    }

    #[cfg(feature = "fixtures")]
    wait_at_chat_wave_ensure_barrier(&area_id).await;

    // #1147 D10 — an area with a `area_folders` claim still gets that folder
    // (attached semantics preserved: the user pointed at it, the server never
    // touches it). An area *without* a claim used to 409 here, which since
    // #1109 made areas pure namespaces means **every new area's conversation
    // entry point fails by definition** — `POST /api/areas/{id}/conversations`
    // calls this unconditionally. It now falls back to a managed default,
    // which is what "the workspace is a default, not a question we ask the
    // user" means (design §2.3).
    let claimed_cwd = s
        .repo
        .area_folders_by_area(&area_id)
        .await?
        .into_iter()
        .min_by(|left, right| {
            std::path::Path::new(&left.path)
                .components()
                .count()
                .cmp(&std::path::Path::new(&right.path).components().count())
                .then_with(|| left.path.cmp(&right.path))
        })
        .map(|folder| folder.path);
    let (cwd, workspace_plan) = match claimed_cwd {
        Some(path) => (path, WaveWorkspacePlan::AttachedFromCwd),
        None => (
            // Ignored by `ManagedUnder`, which derives the path from the wave
            // id; kept non-empty so the row's pre-workspace shape is unchanged.
            default_cwd(),
            WaveWorkspacePlan::ManagedUnder(s.workspace_root.clone()),
        ),
    };
    let p = NewWave {
        area_id: area_id.clone().into(),
        title: "Area chat".into(),
        sort: None,
        cwd: cwd.clone(),
        template_id: None,
        plugin_scope: None,
        template_input: None,
        attach_folder: false,
        theme: RequestTheme::default_dark(),
    };
    let attempt = create_wave_structure(
        s.clone(),
        actor,
        p,
        CreateWaveOptions {
            // The cwd was just picked from this area's own existing
            // claims, so there is nothing to scan and nothing to mint.
            folder_claim: FolderClaim::Skip,
            body_area_id: area_id.clone(),
            normalized_cwd: cwd,
            init: WaveInit::Blank,
            as_template: false,
            workspace_plan,
        },
        Some(AREA_CHAT_PURPOSE),
    )
    .await;
    let (wave, created) = match attempt {
        Ok((wave, _, _, _)) => (wave, true),
        Err(error) if is_unique_constraint(&error, "waves.area_id") => {
            let wave = s
                .repo
                .waves_by_area(&area_id)
                .await?
                .into_iter()
                .find(|wave| wave.purpose.as_deref() == Some(AREA_CHAT_PURPOSE))
                .ok_or_else(|| CalmError::Internal("chat wave ensure race had no winner".into()))?;
            (wave, false)
        }
        Err(error) => return Err(error),
    };
    Ok((wave, created))
}

#[allow(deprecated)]
async fn create_wave_structure(
    s: RouteState,
    actor: Actor,
    p: NewWave,
    options: CreateWaveOptions,
    purpose: Option<&'static str>,
) -> Result<(Wave, bool, String, String)> {
    let CreateWaveOptions {
        folder_claim,
        body_area_id,
        normalized_cwd,
        init,
        as_template,
        workspace_plan,
    } = options;
    // #1147 — captured before `s` is moved into the write closure. Only the
    // managed branch uses it; `materialize_workspace` ignores it for attached.
    let workspace_root_for_materialize = s.workspace_root.clone();
    let spec_card_id = new_id();
    let report_card_id = new_id();
    let actor_id = actor.to_actor_id();
    let actor_id_for_tx = actor_id.clone();
    let write_for_tx = s.write.clone();
    let spec_card_id_for_tx = spec_card_id.clone();
    let report_card_id_for_tx = report_card_id.clone();
    let area_id_for_attach = body_area_id;
    let normalized_cwd_for_tx = normalized_cwd;
    // #1115 — the fork path deliberately derives no `EditAuthor`. It used to
    // (`User` when no `X-Calm-Actor` header was present, `Spec` otherwise) and
    // hand it to `fork_guard::guard_forked_blocks`, which made that guard a
    // no-op for the browser fork — the single most common fork there is. The
    // fork's normalization and its belt are both author-independent now, so
    // nothing here may classify the caller.
    let ((wave, created), _event_ids) = write_with_actor_events_typed(
        s.repo.as_ref(),
        None,
        &s.events,
        &s.write,
        move |tx| {
            Box::pin(async move {
                // #275 — claim scan + claim insert, atomic with the wave
                // row because they share this BEGIN IMMEDIATE tx. Must
                // stay first: every branch below either rolls the tx back
                // or leaves the claim table consistent for `wave_create_tx`.
                enforce_folder_claim_tx(
                    tx,
                    &folder_claim,
                    &area_id_for_attach,
                    &normalized_cwd_for_tx,
                    FolderClaimIntent::Create,
                    FolderClaimPass::Authoritative,
                )
                .await?;

                let wave =
                    wave_create_tx(tx, p, purpose, &workspace_plan, write_for_tx.area_cache())
                        .await?;
                let wave_id = wave.id.clone();
                let area_id = wave.area_id.clone();

                // #1300 — three initialization sources, one persistence
                // mechanism. `Template` builds from a constant and needs no
                // database read; `Fork` reads the source wave inside this same
                // transaction, exactly as before.
                let init_snapshot = match &init {
                    WaveInit::Blank => None,
                    WaveInit::Template { key } => Some(prepare_template_report(key)?),
                    WaveInit::Fork { source_wave_id } => {
                    let source_wave_id = source_wave_id.as_str();
                    let source_id = WaveId::from(source_wave_id.to_string());
                    let source_wave = wave_get_tx(tx, &source_id).await.map_err(|error| {
                        if matches!(error, CalmError::NotFound(_)) {
                            CalmError::BadRequest(format!(
                                "wave create: fork source wave `{source_wave_id}` does not exist"
                            ))
                        } else {
                            error
                        }
                    })?;
                    let source_area_kind: String =
                        sqlx::query_scalar("SELECT kind FROM areas WHERE id=?1")
                            .bind(source_wave.area_id.as_str())
                            .fetch_one(&mut **tx)
                            .await?;
                    if source_wave.area_id != area_id
                        && source_area_kind != AreaKind::System.as_db_str()
                    {
                        return Err(CalmError::BadRequest(format!(
                            "wave create: fork source wave `{source_wave_id}` must be in the target area or the system area"
                        )));
                    }
                    let (summary, blocks) =
                        report_blocks_snapshot_tx(tx, source_wave_id).await?;
                    Some(prepare_fork_report(
                        summary,
                        blocks,
                        source_wave_id,
                        wave_id.as_str(),
                    )?)
                    }
                };

                let spec_card = card_create_with_id_tx(
                    tx,
                    spec_card_id_for_tx.clone(),
                    NewCard {
                        title: None,
                        wave_id: wave_id.clone(),
                        kind: "codex".into(),
                        sort: None,
                        // #1211 S1: on this user-driven create path the wave
                        // title is no longer the wave's intent, so create
                        // seeds no `prompt` here. The parameter stays because
                        // child waves still pass the task goal their parent
                        // spec declared (`operation/child_wave_adapter.rs`) —
                        // that is machine-written intent, not a title a human
                        // typed, and it is what seeds the child's harness when
                        // the child wave starts.
                        payload: spec_harness_card_payload(None),
                    },
                    CardRole::Spec,
                    false,
                    write_for_tx.role_cache(),
                )
                .await?;

                let report_payload =
                    serde_json::to_value(WaveReportPayload::initial()).map_err(|e| {
                        CalmError::Internal(format!(
                            "wave_create: serialize wave-report payload: {e}"
                        ))
                    })?;
                let mut report_card = card_create_with_id_tx(
                    tx,
                    report_card_id_for_tx.clone(),
                    NewCard {
                        title: None,
                        wave_id: wave_id.clone(),
                        kind: "wave-report".into(),
                        sort: Some(-1.0),
                        payload: report_payload,
                    },
                    CardRole::ReportCard,
                    false,
                    write_for_tx.role_cache(),
                )
                .await?;

                let mut init_projection = None;
                if let Some((payload, mut doc, declarations, diagnostics)) = init_snapshot {
                    let payload = serde_json::to_value(payload).map_err(|error| {
                        CalmError::Internal(format!(
                            "wave_create: serialize forked wave-report payload: {error}"
                        ))
                    })?;
                    let (persisted_report, projection) =
                        persist_initial_report_and_project_tasks_tx(
                        tx,
                        report_card.id.as_str(),
                        wave_id.as_str(),
                        payload,
                        &mut doc,
                        &declarations,
                        &diagnostics,
                        )
                        .await?;
                    report_card = persisted_report;
                    init_projection = Some(projection);
                }

                let wave_scope = EventScope::Wave {
                    wave: wave_id.clone(),
                    area: area_id.clone(),
                };
                let spec_card_scope = EventScope::Card {
                    card: spec_card.id.clone(),
                    wave: wave_id.clone(),
                    area: area_id.clone(),
                };
                let report_card_scope = EventScope::Card {
                    card: report_card.id.clone(),
                    wave: wave_id.clone(),
                    area: area_id.clone(),
                };
                let layout_overlay = overlay_upsert_tx(
                    tx,
                    NewOverlay {
                        plugin_id: "kernel".into(),
                        entity_kind: "view".into(),
                        entity_id: wave_id.as_str().to_string(),
                        kind: "layout".into(),
                        payload: spec_harness_layout_payload(
                            spec_card.id.as_str(),
                            report_card.id.as_str(),
                        ),
                    },
                )
                .await?;
                let template_overlay = if as_template {
                    // #1300 — the `template_key` variant of this payload was
                    // written only by the deleted seeding path. Two separate
                    // layers, worth not conflating:
                    //
                    //   * **Schema.** `template_key` is still part of the
                    //     `template` overlay payload schema and is still
                    //     pinned by `payload_validation.rs`; `validate_overlay_payload`
                    //     would accept a payload carrying it.
                    //   * **Route.** The narrow fact, and no wider one: a row
                    //     that `is_template_overlay` would recognise is by
                    //     definition `plugin_id = "kernel"` /
                    //     `entity_kind = "view"`, and since #1297
                    //     `overlays::ensure_overlay_write_allowed` rejects both
                    //     of those reserved namespaces with 403 *before*
                    //     `validate_overlay_payload` runs. So `POST /api/overlays`
                    //     answers 403 Forbidden for *that* row, not 201 — pinned
                    //     by `wave_template_overlay::
                    //     overlay_post_cannot_mark_an_existing_wave_as_template`.
                    //
                    //     It is NOT true that a `template_key` payload cannot
                    //     be stored at all. `entity_kind = "wave"` is
                    //     externally writable (`OVERLAY_ENTITY_SCOPE_REGISTRY`
                    //     in `calm-truth::validation`), and payload validation
                    //     is keyed on `kind`, not on the plugin, so
                    //     `{plugin_id: "p1", entity_kind: "wave", kind:
                    //     "template", payload: {schemaVersion: 1, template_key:
                    //     ".."}}` passes both the 403 guard and
                    //     `validate_overlay_payload` and lands in the table.
                    //     That row is a plugin-owned overlay: `is_template_overlay`
                    //     is false for it, so no kernel reader treats it as a
                    //     template marker.
                    //
                    // Kernel-internal writers like this one call
                    // `overlay_upsert_tx` directly and never traverse that
                    // router — but none of them mints a `template_key`, and no
                    // kernel reader consults one.
                    let payload = template_overlay_payload();
                    validate_overlay_payload(OVERLAY_TEMPLATE_KIND, &payload)?;
                    Some(
                        overlay_upsert_tx(
                            tx,
                            NewOverlay {
                                plugin_id: OVERLAY_TEMPLATE_PLUGIN_ID.into(),
                                entity_kind: OVERLAY_TEMPLATE_ENTITY_KIND.into(),
                                entity_id: wave_id.as_str().to_string(),
                                kind: OVERLAY_TEMPLATE_KIND.into(),
                                payload,
                            },
                        )
                        .await?,
                    )
                } else {
                    None
                };
                let mut events = vec![
                    (
                        actor_id_for_tx.clone(),
                        wave_scope.clone(),
                        Event::WaveUpdated(crate::event::WaveUpdatedPayload::new(
                            wave.clone(),
                            None,
                        )),
                    ),
                    (
                        actor_id_for_tx.clone(),
                        spec_card_scope,
                        Event::CardAdded(spec_card),
                    ),
                    (
                        actor_id_for_tx.clone(),
                        report_card_scope,
                        Event::CardAdded(report_card),
                    ),
                    (
                        actor_id_for_tx.clone(),
                        wave_scope,
                        Event::OverlaySet(layout_overlay),
                    ),
                ];
                if let Some(template_overlay) = template_overlay {
                    events.push((
                        actor_id_for_tx.clone(),
                        EventScope::Wave {
                            wave: wave_id.clone(),
                            area: area_id.clone(),
                        },
                        Event::OverlaySet(template_overlay),
                    ));
                }
                if let Some(projection) = init_projection {
                    if !projection.changed_keys.is_empty() {
                        events.push((
                            actor_id_for_tx.clone(),
                            EventScope::Wave {
                                wave: wave_id.clone(),
                                area: area_id.clone(),
                            },
                            Event::PlanUpdated {
                                wave_id,
                                changed_keys: projection.changed_keys,
                                agent_message: None,
                            },
                        ));
                    }
                    events.extend(projection.kernel_events);
                }
                Ok(((wave, true), events))
            })
        },
    )
    .await?;

    // #1147 S2 (design D3/D5) — materialize outside the transaction and
    // before the spec harness starts. `Attached` is a no-op: the directory is
    // the user's and the server never creates or `git init`s it.
    //
    // A failure here MUST surface as a non-2xx. The tempting shape is
    // `tracing::warn!` + `Ok(())` (as `start_spec_harness` below does for a
    // different, recoverable failure) — but that returns 201 for a wave whose
    // first codex worker will then die with `spawn-failed`, which is #1147
    // itself replayed one layer down.
    crate::workspace_materialize::materialize_workspace(
        &wave.workspace,
        &workspace_root_for_materialize,
        wave.id.as_str(),
    )
    .map_err(|error| {
        tracing::error!(
            wave_id = %wave.id,
            path = %wave.workspace.path,
            error = %error,
            "wave create: workspace materialization failed"
        );
        error
    })?;

    Ok((wave, created, spec_card_id, report_card_id))
}

async fn start_spec_harness(
    s: &RouteState,
    actor: &Actor,
    wave: &Wave,
    spec_card_id: String,
    report_card_id: String,
) -> Result<()> {
    // #1211 S1: no goal is seeded on this user-driven create path. An omitted
    // title is stored as the empty string (`Untitled wave` is only what the
    // frontend shows for a blank one) and the spec agent names the wave once
    // it knows what the work is, so there is nothing here that could stand in
    // for the user's intent. Child waves do NOT come through here — they start their
    // harness with the parent spec's declared task goal
    // (`scheduler/mod.rs`, `operation/child_wave_adapter.rs`).
    let request = SpecHarnessStartOperationPayload {
        actor: actor.to_actor_id(),
        wave_id: wave.id.to_string(),
        spec_card_id: CardId::from(spec_card_id.clone()),
        report_card_id: Some(report_card_id),
        sort: None,
        cwd: wave.workspace.path.clone(),
        goal: None,
        reset_harness_items: false,
        force_new_thread: false,
        profile: Default::default(),
        create_card: None,
        first_message_sha256: None,
    };
    let op_payload = serde_json::to_value(&request)?;
    let payload_hash = stable_payload_hash(&serde_json::json!({
        "actor": actor.as_str(),
        "request": &request,
    }))?;
    match s
        .operation_runtime
        .submit(
            "spec-harness-start",
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
                OperationOutcome::Succeeded { .. }
                | OperationOutcome::SucceededViaCollision { .. } => {}
                OperationOutcome::Failed {
                    last_error,
                    from_phase,
                    ..
                } => {
                    tracing::warn!(
                        spec_card_id,
                        wave_id = %wave.id,
                        ?from_phase,
                        error = %last_error,
                        "spec harness start operation failed; wave created but spec agent is inert"
                    );
                }
                OperationOutcome::Stuck { reason, from_phase } => {
                    tracing::warn!(
                        spec_card_id,
                        wave_id = %wave.id,
                        ?from_phase,
                        reason,
                        "spec harness start operation stuck; wave created but spec agent is inert"
                    );
                }
            },
            Err(e) => tracing::warn!(
                spec_card_id,
                wave_id = %wave.id,
                error = %e,
                "spec harness start wait failed; wave created but spec agent may be inert"
            ),
        },
        Err(e) => tracing::warn!(
            spec_card_id,
            wave_id = %wave.id,
            error = %e,
            "spec harness start submission failed; wave created but spec agent is inert"
        ),
    }

    Ok(())
}

type InitialReportSnapshot = (
    WaveReportPayload,
    ReportDoc,
    Vec<calm_types::report_blocks::tasks::TaskDeclaration>,
    Vec<Vec<calm_types::report_blocks::tasks::Diagnostic>>,
);

async fn persist_initial_report_and_project_tasks_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    report_card_id: &str,
    wave_id: &str,
    payload: serde_json::Value,
    doc: &mut ReportDoc,
    declarations: &[calm_types::report_blocks::tasks::TaskDeclaration],
    diagnostics: &[Vec<calm_types::report_blocks::tasks::Diagnostic>],
) -> Result<(Card, TaskProjectionOutcome)> {
    let report_card = card_update_with_crdt_tx(
        tx,
        report_card_id,
        CardPatch {
            title: None,
            kind: None,
            sort: None,
            payload: Some(payload),
            deletable: None,
        },
        doc.to_bytes(),
    )
    .await?;
    // Projection resolves block refs through this cache, so both effects are
    // deliberately one production operation with one fixed internal order.
    let projection = project_tasks_tx(tx, wave_id, declarations, diagnostics).await?;
    Ok((report_card, projection))
}

fn prepare_fork_report(
    summary: String,
    mut blocks: Vec<ReportBlock>,
    source_wave_id: &str,
    target_wave_id: &str,
) -> Result<InitialReportSnapshot> {
    use std::collections::HashSet;

    use calm_types::report_blocks::{KIND_PROSE, KIND_TASK, flat_text, validate_payload};
    use calm_types::report_links::{UnsafeWaveLink, rewrite_wave_destination, rewrite_wave_links};

    let copied_block_ids: HashSet<String> = blocks.iter().map(|block| block.id.clone()).collect();
    let mut unsafe_links: Vec<(String, &'static str, UnsafeWaveLink)> = Vec::new();
    for block in &mut blocks {
        let block_id = block.id.clone();
        if block.kind == KIND_PROSE {
            if let Some(markdown) = block.payload.get_mut("markdown")
                && let Some(source) = markdown.as_str()
            {
                match rewrite_wave_links(source, source_wave_id, target_wave_id, &copied_block_ids)
                {
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
            // verbatim and landed as prose in the target wave.
            //
            // What `validate_body_fences` actually covers today (#1252 R1/F3
            // corrects an earlier "every other write end" claim here, which
            // was false at the time): its production call sites are
            // `wave_report::apply_report_op`'s two whole-body arms —
            // `ReportDocOp::Replace` and `::WriteMarkdown` — plus this fork
            // exit. The `UpsertBlock` arms, which this note used to record
            // as an open *op-layer* gap, are covered since #1269 and its
            // follow-up by a *different* check, `wave_report_guard::
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
            // reach that check at all — see `wave_report_guard`'s module
            // doc, which enumerates all four construction sites.) And
            // "fenced prose" here means a fence
            // carried whole in one block; on the residual that a fence split
            // across two prose blocks still assembles in the projection, see
            // `wave_report_guard::validate_block_content`.
            //
            // Deliberately only the fence check here: the fork exit does not
            // additionally run `validate_payload` on the prose block's own
            // `{"markdown": …}` payload — that is a separate behaviour
            // change. Nor is this the stricter prose rule the op layer and
            // the block surfaces apply; tightening fork to refuse
            // well-formed fences too would reject already-persisted source
            // waves, so it stays at "malformed / schema-invalid".
            if let Some(markdown) = block.payload.get("markdown").and_then(|v| v.as_str()) {
                crate::wave_report_guard::validate_body_fences(markdown).map_err(|error| {
                    CalmError::BadRequest(format!(
                        "wave create: invalid forked report block {block_id}: {error}"
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
                    match rewrite_wave_links(
                        source,
                        source_wave_id,
                        target_wave_id,
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
                        *reference = serde_json::Value::String(rewrite_wave_destination(
                            source,
                            source_wave_id,
                            target_wave_id,
                            &copied_block_ids,
                        ));
                    }
                }
            }
            let tombstone = payload
                .get("tombstone")
                .is_some_and(|value| !value.is_null());
            payload.insert(
                "declared_by".into(),
                serde_json::Value::String("spec".into()),
            );
            if tombstone {
                // #1111 — `tombstoned_by` is the second *attribution* field on
                // a task block: `wave_report_edit_guard::guard_task_declarations`
                // treats `declared_by == "user" || tombstoned_by == "user"` as
                // user-owned, and `tombstoned_by` is immutable once a block is
                // a tombstone. Copying a template's `tombstoned_by: "user"`
                // would hand every forked wave a block no spec author can ever
                // edit or delete — the same template-as-backdoor hole §7.2
                // closed for `declared_by`. Normalize both together.
                //
                // Non-tombstone blocks are deliberately left alone: a residual
                // `tombstoned_by` there is rejected by `validate_payload`
                // ("must be absent from a non-tombstone task") a few lines
                // below, so a corrupt source fails the fork closed instead of
                // being silently repaired into a shape it never validly had.
                //
                // `released_by_user` is the third privilege field; it is
                // normalized in the `else` arm below, because the tombstone
                // schema (`report_blocks/kinds.rs:158-166`) forbids the field
                // outright — see that arm's comment.
                payload.insert(
                    "tombstoned_by".into(),
                    serde_json::Value::String("spec".into()),
                );
                payload.remove("ready");
            } else {
                payload.insert("ready".into(), serde_json::Value::Bool(false));
                // #1115 — `released_by_user` is the third and last privilege
                // field on a task block, and the one that answers "did a HUMAN
                // approve this task in THIS wave". `declared_by` is rewritten to
                // `"spec"` two lines up, which is exactly the shape
                // `declare_and_wait` exists to hold back
                // (`task_projection.rs:709-719`: `effective_wait &&
                // declared_by == "spec" && !released_by_user && !tombstone`).
                // Copying a template's `released_by_user: true` would hand the
                // copy a standing exemption from a decision the new wave's user
                // never made — and `report-blocks/task.tsx:185` would then hide
                // the "Allow this task" button from her, because the flag is
                // already set. Same source semantics as `ready: false` above:
                // nothing in a template was decided for *this* wave.
                //
                // Removed rather than written as an explicit `false`. Absent and
                // `false` are identical to every reader (`tasks.rs:732-734`
                // `.unwrap_or(false)`; `task.tsx:185` tests falsiness), but they
                // are NOT identical to `wave_report_edit_guard.rs:162-167`,
                // which compares the raw `Option<&Value>` and rejects any
                // non-user edit that changes it. Blocks produced by the
                // plan-template generator carry no such key
                // (`plan_template_task_block_payload`; `plan.rs:917-925` lists
                // `released_by_user` among the template exclusions, pinned by a
                // field-set equality meta-test) — that is a property of that
                // one generator, not a global invariant, since an agent writing
                // blocks over MCP could schema-legally include an explicit
                // `false`. Absent is nonetheless the canonical shape, so
                // writing an explicit `false` here would make forked blocks the
                // only ones a spec author must echo the field back on —
                // re-creating, on this field, the "template block the spec can
                // never edit" failure #1111 just closed. `ready` is written explicitly only because
                // `kinds.rs:243-245` makes it *required* on a live task; this
                // one is optional, so the absent form is available and is the
                // one that matches a fresh declaration byte for byte.
                payload.remove("released_by_user");
            }
        }

        validate_payload(&block.kind, &block.payload).map_err(|error| {
            CalmError::BadRequest(format!(
                "wave create: invalid forked report block {}: {error}",
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
            "wave create: cannot safely rewrite fork link destinations:\n{details}\n\
             Write each link target in plain form (without character entities or backslash escapes, and without inline HTML in its label) and retry."
        )));
    }

    guard_forked_blocks(&blocks)?;
    let doc = ReportDoc::from_blocks_exact(&summary, &blocks).map_err(|error| {
        CalmError::BadRequest(format!(
            "wave create: invalid fork report snapshot: {error}"
        ))
    })?;
    let (summary, body) = doc.project().map_err(|error| {
        CalmError::Internal(format!("wave create: project fork report CRDT: {error}"))
    })?;
    let (declarations, diagnostics) =
        calm_types::report_blocks::tasks::project_task_declarations(&blocks);
    let mut payload = WaveReportPayload::new(summary, body);
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
    Ok((payload, doc, declarations, diagnostics))
}

/// The payload production writes on a spec-harness card.
///
/// `pub` rather than `pub(crate)` so integration fixtures that seed a spec card
/// row directly can mint the production shape instead of re-typing a partial
/// literal: `{"schemaVersion": 1}` alone drops `codex_source` and
/// `spec_harness`, and a future backend reader of either key would then find
/// the fixture silently unlike production (#1189 review F2).
pub fn spec_harness_card_payload(goal: Option<String>) -> serde_json::Value {
    let mut card_payload = serde_json::Map::new();
    card_payload.insert(
        "schemaVersion".into(),
        serde_json::Value::from(CODEX_PAYLOAD_SCHEMA_VERSION),
    );
    card_payload.insert(
        "codex_source".into(),
        serde_json::Value::String("shared".into()),
    );
    card_payload.insert("spec_harness".into(), serde_json::Value::Bool(true));
    if let Some(goal) = goal.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        card_payload.insert("prompt".into(), serde_json::Value::String(goal.to_string()));
    }
    serde_json::Value::Object(card_payload)
}

pub(crate) fn spec_harness_layout_payload(
    spec_card_id: &str,
    report_card_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1,
        "positions": {
            spec_card_id: {
                "x": 0, "y": 0, "w": 6, "h": 12
            },
            report_card_id: {
                "x": 6, "y": 0, "w": 6, "h": 12
            }
        }
    })
}

// ---------------------------------------------------------------------------
// #1147 S3 — changing a wave's workspace
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
/// `fixtures`-only, exactly like `wave_delete_teardown_hooks` above; a release
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
pub fn install_workspace_repoint_race_hook_for_test(wave_id: &str, hook: WorkspaceRepointRaceHook) {
    workspace_repoint_race_hooks()
        .lock()
        .expect("workspace repoint hook mutex")
        .insert(wave_id.to_string(), hook);
}

async fn wait_at_workspace_repoint_race_hook(wave_id: &str) {
    #[cfg(feature = "fixtures")]
    {
        let hook = workspace_repoint_race_hooks()
            .lock()
            .expect("workspace repoint hook mutex")
            .remove(wave_id);
        if let Some(hook) = hook {
            hook.entered.notify_one();
            hook.release.notified().await;
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = wave_id;
}

/// Test seam for the shutdown-failure branch of the fence.
///
/// `SpecHarness::shutdown` fails only on a persistence error deep inside the
/// run loop, which an integration test cannot provoke without dismantling the
/// runtime row the fence needs. The branch is still worth covering — it is the
/// one that used to kill a wave's spec agent outright — so the failure is
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
pub fn fail_workspace_repoint_shutdown_for_test(wave_id: &str) {
    workspace_repoint_shutdown_failures()
        .lock()
        .expect("workspace repoint shutdown failure mutex")
        .insert(wave_id.to_string(), ());
}

async fn shutdown_fenced_harness(
    harness: &crate::harness::SpecHarness,
    wave_id: &str,
) -> Result<()> {
    #[cfg(feature = "fixtures")]
    {
        let forced = workspace_repoint_shutdown_failures()
            .lock()
            .expect("workspace repoint shutdown failure mutex")
            .remove(wave_id)
            .is_some();
        if forced {
            return Err(CalmError::Internal(
                "injected spec harness shutdown failure (#1147 S3 test seam)".into(),
            ));
        }
    }
    #[cfg(not(feature = "fixtures"))]
    let _ = wave_id;
    harness.shutdown().await
}

/// What the fence transaction decided, carried out to the filesystem half.
struct RepointFence {
    /// The workspace as read *inside* the transaction — the authority, not the
    /// unlocked read the route did to answer 404.
    old_workspace: WaveWorkspace,
    /// Every runtime the fence superseded, so the process-side shutdown below
    /// knows which live handles to kill and the compensating restart knows
    /// something was torn down.
    superseded_runtime_ids: Vec<String>,
}

/// #1147 S3 — point a wave at a repository the user already has
/// (design §更换与冻结, transition `managed → attached`).
///
/// # Why this is not a column write
///
/// SQLite transactions do not isolate the filesystem, so "check inside the
/// transaction" cannot close the window between the check and the move. The
/// spec harness is deliberately *not* frozen at this point and has run
/// `sandbox-mode: workspace-write` since its first message, and the dispatcher
/// pushes observations that start fresh turns. Three steps, none optional:
///
/// 1. **A real fence, in the same transaction as the criteria.** Every active
///    runtime of the wave is marked `superseded`, which is the state
///    `dispatcher::harness_runtime_id_for_spec_card` reads
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
///    goes through S5's [`workspace_recycle::recycle_wave_workspace`] — the
///    single controlled entry point — which re-checks `kind == Managed`,
///    canonical containment in the workspace root, the exact
///    `<root>/<area>/<wave>` depth, and our ownership marker, and renames into
///    `.trash` rather than deleting. The `WaveWorkspace` handed to it is the
///    OLD value, read inside the fence transaction, so it describes the
///    directory being reclaimed rather than the row's new state.
///
/// # Order: write the row, then move the directory
///
/// The opposite order is what S5 chose for `DELETE`, and for the opposite
/// reason. There, a failure after the move would leave a wave row whose
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
/// `managed_workspace_path(root, area_id, wave_id)` still names it — so a
/// future sweep can find it without any new bookkeeping.
///
/// # What a refusal leaves behind
///
/// Nothing on disk and nothing in the row. The one visible effect is that the
/// spec harness was torn down, so this function restarts it on the **old**
/// path before returning. That restart is the same operation
/// `POST /api/cards/{id}/reset` performs routinely, and harness items are
/// persisted per card, so the user's transcript survives.
async fn repoint_wave_workspace(
    s: &RouteState,
    w: &WorkerState,
    actor: &Actor,
    wave: &Wave,
    requested: &WaveWorkspacePatch,
) -> Result<Response> {
    // Issue #985's rule, applied to a strictly more destructive field: moving
    // a directory is a human decision. This is the only thing between an agent
    // and pointing a wave at any repository on the box.
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
    // `repoint_wave_workspace_for_test`) that constructs the caller HTTP
    // cannot.
    if !matches!(actor.to_actor_id(), ActorId::User) {
        return Err(CalmError::Forbidden(
            "wave workspace changes are user-only".into(),
        ));
    }

    // Scope (design §更换与冻结). `managed → attached` is the transition; there
    // is no `managed → managed` because a managed path is derived from the
    // area and wave ids, so "re-allocate" would always re-derive the same
    // directory. Answered explicitly rather than accepted as a no-op, so a
    // client that asks for it learns that instead of believing it worked.
    if requested.kind != WaveWorkspaceKind::Attached {
        return Err(CalmError::BadRequest(
            "wave workspace: only `attached` is a target — pointing a wave at a repository \
             you already have. There is no `managed` target: a managed workspace's path is \
             derived from the wave, so re-allocating one would produce the same directory."
                .into(),
        ));
    }

    // The system area's launchpad path is kernel-maintained
    // (`today_launchpad_ensure_tx` re-derives it on every `ensure`) and is the
    // documented exception to the freeze latch. A user PATCH must not touch
    // it. Same scope decision as S5's row-layer 403 on DELETE: the whole
    // system area, not a `purpose = launchpad` carve-out.
    let area = s.repo.area_get(wave.area_id.as_str()).await?;
    if area.as_ref().is_none_or(|c| c.kind == AreaKind::System) {
        return Err(CalmError::Forbidden(format!(
            "wave {} belongs to the system area; its workspace is kernel-maintained",
            wave.id
        )));
    }

    // Validate the target BEFORE any write. Design D3, and the whole reason
    // #1147 exists: a path that does not exist or is not a Git work tree must
    // fail here with git's own words, not four steps later as a worker's
    // `spawn-failed`.
    let new_path = normalize_path(&requested.path);
    crate::workspace_materialize::validate_attached_workspace(std::path::Path::new(&new_path))?;

    let workspace_root = s.workspace_root.clone();
    let wave_id = wave.id.to_string();
    let area_id = wave.area_id.as_str().to_string();

    // ---- Step 1: criteria + fence, in one BEGIN IMMEDIATE -----------------
    let fence_conflict = FolderConflictSlot::default();
    let fence_wave_id = wave_id.clone();
    let fence_area_id = area_id.clone();
    let fence_path = new_path.clone();
    let fence_claim = FolderClaim::Enforce {
        attach: requested.attach_folder,
        conflict: fence_conflict.clone(),
    };
    let fence = crate::db::write_in_tx_typed(s.repo.as_ref(), move |tx| {
        let wave_id = fence_wave_id.clone();
        let area_id = fence_area_id.clone();
        let new_path = fence_path.clone();
        let claim = fence_claim.clone();
        Box::pin(async move {
            // Authoritative re-read. The route's unlocked read answered 404
            // and scoped the event; every decision below comes from here.
            let old_workspace = crate::db::sqlite::wave_workspace_read_tx(tx, &wave_id).await?;
            if old_workspace.kind != WaveWorkspaceKind::Managed {
                return Err(CalmError::Conflict(format!(
                    "wave {wave_id} already has an attached workspace ({}); an attached \
                     repository belongs to you, and the server never moves, initializes or \
                     deletes one — so it is also never re-pointed away from",
                    old_workspace.path
                )));
            }
            if let Some(frozen_at) = old_workspace.frozen_at {
                return Err(CalmError::Conflict(format!(
                    "wave {wave_id} workspace was frozen at {frozen_at}; a workspace is a \
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
            // THE FENCE. Every active runtime of this wave, not just the spec
            // harness: "no new turn may acquire the old path" is a statement
            // about the wave, and a rule with one named exception is the shape
            // this design line keeps being hurt by.
            let runtime_ids: Vec<String> = sqlx::query_scalar(
                "SELECT id FROM worker_sessions WHERE wave_id=?1 \
                 AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
            )
            .bind(&wave_id)
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
    // The restart below never ran, and the wave's spec agent was dead for
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
        let outcome = shutdown_fenced_harness(&harness, &wave_id).await;
        match outcome {
            Ok(()) => {
                let _ = w.harness.remove(runtime_id);
            }
            Err(error) => tracing::error!(
                wave_id,
                runtime_id,
                error = %error,
                "workspace repoint: shutting the fenced spec harness down failed. \
                 Continuing: the database fence already refuses new turns, the \
                 pre-move re-check catches anything an in-flight turn writes, and \
                 the registry entry is left for the restart to supersede."
            ),
        }
    }

    let old_path = std::path::PathBuf::from(&fence.old_workspace.path);

    // Deterministic race window for the timing test. No-op in production.
    wait_at_workspace_repoint_race_hook(&wave_id).await;

    // ---- Step 2: re-check before anything irreversible --------------------
    let verdict = workspace_pristine(&old_path);
    if let PristineVerdict::Dirty { .. } = &verdict {
        restart_spec_harness_at(s, actor, wave, &fence.old_workspace.path).await;
        return Err(CalmError::Conflict(verdict.conflict_message(&old_path)));
    }

    // ---- The write: claim + workspace, one transaction --------------------
    let new_workspace = WaveWorkspace {
        kind: WaveWorkspaceKind::Attached,
        path: new_path.clone(),
        // Frozen, one-way. Two independent reasons, either sufficient:
        // `attached → *` is not a legal transition, so an unfrozen attached
        // row has no legal use; and S4 pins "no attached wave is ever
        // unfrozen" over the whole table, because an unfrozen attached row is
        // exactly what a future PATCH branch that forgot to check `kind` would
        // relocate — i.e. would move a real user repository.
        frozen_at: Some(crate::model::now_ms()),
    };
    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        area: wave.area_id.clone(),
    };
    let actor_id = actor.to_actor_id();
    let write_conflict = FolderConflictSlot::default();
    let write_claim = FolderClaim::Enforce {
        attach: requested.attach_folder,
        conflict: write_conflict.clone(),
    };
    let write_wave_id = wave_id.clone();
    let write_area_id = area_id.clone();
    let write_workspace = new_workspace.clone();
    let written =
        write_with_actor_events_typed(s.repo.as_ref(), None, &s.events, &s.write, move |tx| {
            let scope = scope.clone();
            let wave_id = write_wave_id.clone();
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
                crate::db::sqlite::wave_workspace_write_tx(tx, &wave_id, &workspace).await?;
                let wave = wave_get_tx(tx, &WaveId::from(wave_id)).await?;
                let events = vec![(
                    actor_id,
                    scope,
                    Event::WaveUpdated(crate::event::WaveUpdatedPayload::new(wave.clone(), None)),
                )];
                Ok((wave, events))
            })
        })
        .await;
    let (updated, _ids) = match written {
        Ok(written) => written,
        Err(error) => {
            // Nothing moved and nothing was written — put the harness back
            // where it was and report.
            restart_spec_harness_at(s, actor, wave, &fence.old_workspace.path).await;
            return folder_conflict_response(&write_conflict, error);
        }
    };

    // ---- Step 3: the old managed directory goes to the trash --------------
    let decision = workspace_recycle::recycle_wave_workspace(
        &workspace_root,
        area.as_ref().map(|c| c.kind),
        &wave_id,
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
        // re-point: the wave is correctly attached to the user's repository
        // and the stale managed directory is at a path that is still
        // derivable. Loud, and not a 500 — telling the caller the request
        // failed would be a lie.
        Ok(workspace_recycle::RecycleDecision::Refused(refusal)) => {
            tracing::error!(
                wave_id,
                path = %fence.old_workspace.path,
                reason = refusal.tag(),
                "workspace repoint: the wave now points at the user's repository, but its old \
                 managed directory could not be reclaimed and is leaked on disk"
            );
        }
        Err(error) => {
            tracing::error!(
                wave_id,
                path = %fence.old_workspace.path,
                error = %error,
                "workspace repoint: the wave now points at the user's repository, but moving \
                 its old managed directory to the trash failed; it is leaked on disk"
            );
        }
    }
    workspace_recycle::gc_trash_best_effort(&workspace_root, crate::model::now_ms());

    // Re-open the spec thread on the new cwd. `force_new_thread` is the only
    // mechanism that re-reads `cwd`: a resumed codex thread keeps the cwd it
    // was minted with, so resuming here would leave the spec agent in the
    // directory that just went to the trash.
    restart_spec_harness_at(s, actor, &updated, &updated.workspace.path).await;

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
pub async fn repoint_wave_workspace_for_test(
    s: &RouteState,
    w: &WorkerState,
    actor: &Actor,
    wave: &Wave,
    requested: &WaveWorkspacePatch,
) -> Result<Response> {
    repoint_wave_workspace(s, w, actor, wave, requested).await
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

/// Re-open the wave's spec harness thread at `cwd`.
///
/// Best effort, and deliberately so: it mirrors `start_spec_harness`, whose
/// failures are warnings ("the wave exists but the spec agent is inert")
/// rather than a failed request. Turning a harness hiccup into a 500 here
/// would be worse than useless — the workspace has already moved and the row
/// already says so, so the caller must not be told the whole operation failed.
///
/// `idempotency_key: None`, like every other non-launchpad spec-harness start
/// (`routes/waves.rs::start_spec_harness`, `routes/cards.rs`'s reset). The
/// launchpad and child-wave call sites need a workspace digest in their keys
/// because they are re-driven with the same key; this one is minted per
/// request and cannot collide.
async fn restart_spec_harness_at(s: &RouteState, actor: &Actor, wave: &Wave, cwd: &str) {
    // Same resolution the dispatcher uses (`resolve_spec_card`): the role
    // cache, not a `cards.kind` guess.
    let cards = match s.repo.cards_by_wave(wave.id.as_str()).await {
        Ok(cards) => cards,
        Err(error) => {
            tracing::warn!(wave_id = %wave.id, error = %error, "workspace repoint: spec card lookup failed");
            return;
        }
    };
    let spec_card_id = cards.into_iter().find_map(|card| {
        (s.write.verify_role(&card.id) == Some(CardRole::Spec)).then(|| card.id.to_string())
    });
    let Some(spec_card_id) = spec_card_id else {
        // Template waves (`as_template`) never start a harness. Nothing to
        // re-anchor.
        return;
    };
    let request = SpecHarnessStartOperationPayload {
        actor: actor.to_actor_id(),
        wave_id: wave.id.to_string(),
        spec_card_id: CardId::from(spec_card_id.clone()),
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
    };
    let hash = match stable_payload_hash(
        &serde_json::json!({"actor": actor.as_str(), "request": &request}),
    ) {
        Ok(hash) => hash,
        Err(error) => {
            tracing::warn!(wave_id = %wave.id, error = %error, "workspace repoint: payload hash failed");
            return;
        }
    };
    let payload = match serde_json::to_value(&request) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(wave_id = %wave.id, error = %error, "workspace repoint: payload encode failed");
            return;
        }
    };
    match s
        .operation_runtime
        .submit(
            "spec-harness-start",
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
                wave_id = %wave.id,
                cwd,
                outcome = ?other.map(|r| r.outcome),
                "workspace repoint: spec harness restart did not succeed; the workspace is \
                 correct but the spec agent is inert"
            ),
        },
        Err(error) => tracing::warn!(
            wave_id = %wave.id,
            cwd,
            error = %error,
            "workspace repoint: spec harness restart submission failed"
        ),
    }
}

#[utoipa::path(
    patch,
    path = "/api/waves/{id}",
    tag = "waves",
    params(("id" = String, Path, description = "Wave id")),
    request_body = WavePatch,
    responses(
        (status = 200, description = "Wave updated", body = Wave),
        (status = 400, description = "Unsupported workspace change", body = ErrorBody),
        (status = 403, description = "Workspace change refused (system area)", body = ErrorBody),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 409, description = "Workspace is frozen, attached, or no longer empty", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn update_wave(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    actor: Actor,
    Path(id): Path<String>,
    Json(p): Json<WavePatch>,
) -> Result<Response> {
    // Need area_id for the scope. Wave rows are immutable wrt their
    // parent area, so reading outside the txn is safe (same rationale as
    // the delete path below).
    let existing = s
        .repo
        .wave_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;

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
        let WavePatch {
            workspace: _,
            title,
            sort,
            archived_at,
            pinned_at,
            lifecycle,
            task_budget,
            require_task_gates,
            spec_task_ceiling,
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
            || spec_task_ceiling.is_some()
            || automation_policy.is_some()
            || tree_task_budget.is_some();
        if mixes_other_fields {
            return Err(CalmError::BadRequest(
                "wave workspace changes must be sent on their own; a workspace re-point moves \
                 directories on disk and cannot share a transaction with row edits"
                    .into(),
            ));
        }
        return repoint_wave_workspace(&s, &w, &actor, &existing, workspace).await;
    }

    // The guard fires on *mentioning* `lifecycle`, not on changing it: a PATCH
    // that re-sends the wave's current lifecycle is 403 too. That is
    // deliberate — the chat wave has no lifecycle the user may drive, so
    // accepting a no-op write would advertise an editable field, and the FSM
    // would then have to be trusted to keep every such write a no-op forever.
    if existing.purpose.as_deref() == Some(AREA_CHAT_PURPOSE) && p.lifecycle.is_some() {
        return Err(CalmError::Forbidden(
            "area chat wave lifecycle cannot be changed".into(),
        ));
    }
    let scope = EventScope::Wave {
        wave: existing.id.clone(),
        area: existing.area_id.clone(),
    };
    let actor_id = actor.to_actor_id();

    // Issue #985 — wave-level automation controls are human decisions.
    // Reject non-user actors before entering the eventized write so neither
    // the row nor a WaveUpdated event can land.
    if (p.spec_task_ceiling.is_some()
        || p.automation_policy.is_some()
        || p.tree_task_budget.is_some())
        && !matches!(actor_id, ActorId::User)
    {
        return Err(CalmError::Forbidden(
            "automation_policy, spec_task_ceiling and tree_task_budget are user-only".into(),
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
    // `wave_update_tx` doesn't pointlessly rewrite the column /
    // bump `updated_at`), and we skip the `WaveLifecycleChanged`
    // emit. If after stripping the patch has no other fields set,
    // we return the existing row without touching the DB at all.
    // Worker / plugin actors still hit `Forbidden` here regardless
    // of from == to — idempotency only applies once the actor has
    // any lifecycle authority.
    let mut p = p;
    let lifecycle_change = if let Some(to) = p.lifecycle {
        validate_transition(existing.lifecycle, to, &actor_id)
            .map_err(|e| CalmError::Forbidden(format!("wave lifecycle: {e}")))?;
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
    if let Some(Some(ceiling)) = p.spec_task_ceiling
        && ceiling < 0
    {
        return Err(CalmError::BadRequest(format!(
            "spec_task_ceiling must be >= 0 (got {ceiling}); pass null to reset to the kernel default"
        )));
    }
    // Issue #985 slice 6 PR-B — same shape as `spec_task_ceiling`. 0 is legal
    // ("no new spec inventory anywhere in this tree"); the root-only rule is
    // enforced inside `wave_update_tx`, which every writer shares.
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
    // nothing to emit — return the wave as-is. This is the
    // idempotent retry path for "spec re-sends the current state."
    let patch_has_other_changes = p.title.is_some()
        || p.sort.is_some()
        || p.archived_at.is_some()
        || p.pinned_at.is_some()
        || p.task_budget.is_some()
        || p.require_task_gates.is_some()
        || p.spec_task_ceiling.is_some()
        || p.automation_policy.is_some()
        || p.tree_task_budget.is_some();
    if lifecycle_change.is_none() && !patch_has_other_changes {
        return Ok(Json(existing).into_response());
    }

    // When a lifecycle change is part of the patch we emit *two*
    // events from the same txn: a `WaveLifecycleChanged` so dedicated
    // subscribers don't have to inspect every `WaveUpdated`, plus the
    // usual `WaveUpdated` so cache invalidation still sees the new
    // row shape. Both share scope + actor; both land or neither does.
    let area_id_for_event = existing.area_id.clone();
    let wave_id_for_event = existing.id.clone();
    // `tree_task_budget` feeds every member's deterministic share, so changing
    // it invalidates every member's projection. Rebuild the bounded member set
    // in this same write transaction: after PATCH returns, no descendant can
    // retain a pending row admitted by the old budget and race a later claim.
    let projection_policy_changed = p.spec_task_ceiling.is_some()
        || p.automation_policy.is_some()
        || p.tree_task_budget.is_some();
    let tree_budget_changed = p.tree_task_budget.is_some();
    let p_for_tx = p.clone();
    let (wave, _ids) =
        write_with_actor_events_typed(s.repo.as_ref(), None, &s.events, &s.write, move |tx| {
            let scope = scope.clone();
            Box::pin(async move {
                let wave = wave_update_tx(tx, &id, p_for_tx).await?;
                let projections = if projection_policy_changed {
                    if tree_budget_changed {
                        tasks_rebuild_tree_tx(tx, &id).await?
                    } else {
                        vec![(wave.clone(), tasks_rebuild_tx(tx, &id).await?)]
                    }
                } else {
                    Vec::new()
                };
                let mut events: Vec<(ActorId, EventScope, Event)> = Vec::new();
                if let Some((from, to)) = lifecycle_change {
                    events.push((
                        actor_id.clone(),
                        scope.clone(),
                        Event::WaveLifecycleChanged {
                            id: wave_id_for_event.clone(),
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
                    Event::WaveUpdated(crate::event::WaveUpdatedPayload::new(wave.clone(), None)),
                ));
                for (projected_wave, projection) in projections {
                    if !projection.changed_keys.is_empty() {
                        events.push((
                            actor_id.clone(),
                            EventScope::Wave {
                                wave: projected_wave.id.clone(),
                                area: projected_wave.area_id.clone(),
                            },
                            Event::PlanUpdated {
                                wave_id: projected_wave.id,
                                changed_keys: projection.changed_keys,
                                agent_message: None,
                            },
                        ));
                    }
                    events.extend(projection.kernel_events);
                }
                Ok((wave, events))
            })
        })
        .await?;
    Ok(Json(wave).into_response())
}

async fn snapshot_wave_deletion(
    s: &RouteState,
    pool: &sqlx::SqlitePool,
    wave: &Wave,
) -> Result<WaveDeletePlan> {
    let cards = s.repo.cards_by_wave(wave.id.as_str()).await?;
    let mut terminals = Vec::new();
    for card in &cards {
        if let Some(terminal) = s.repo.terminal_get_by_card(card.id.as_str()).await? {
            terminals.push(terminal);
        }
    }
    let active_runtime_ids = sqlx::query_scalar(
        "SELECT id FROM worker_sessions WHERE wave_id=?1 \
         AND state IN ('starting','running','idle','turn_pending') ORDER BY id",
    )
    .bind(wave.id.as_str())
    .fetch_all(pool)
    .await?;
    Ok(WaveDeletePlan {
        wave_id: wave.id.clone(),
        area_id: wave.area_id.clone(),
        cards,
        terminals,
        active_runtime_ids,
    })
}

async fn teardown_wave_deletion(
    s: &RouteState,
    w: &WorkerState,
    cs: &CodexShellState,
    plan: &WaveDeletePlan,
) -> Result<()> {
    wait_at_wave_delete_teardown_hook(plan.wave_id.as_str()).await;
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
async fn finish_wave_deletion(s: &RouteState, plan: WaveDeletePlan, actor: ActorId) -> Result<()> {
    let write_for_tx = s.write.clone();
    let wave_id = plan.wave_id.clone();
    let area_id = plan.area_id.clone();
    let terminals = plan.terminals;
    let scope = EventScope::Wave {
        wave: wave_id.clone(),
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
                overlay_delete_card_overlays_by_wave_tx(tx, wave_id.as_str()).await?;
                overlay_delete_by_entity_tx(tx, "wave", wave_id.as_str()).await?;
                overlay_delete_by_entity_tx(tx, "view", wave_id.as_str()).await?;
                let release = release_workspace_leases_for_wave_tx(tx, wave_id.as_str()).await?;
                let mut events = release.events;
                wave_delete_tx(tx, wave_id.as_str(), write_for_tx.area_cache()).await?;
                events.push((
                    actor,
                    scope,
                    Event::WaveDeleted {
                        id: wave_id,
                        area_id,
                    },
                ));
                Ok((release.sweep.into_iter().collect::<Vec<_>>(), events))
            })
        })
        .await?;
    sweep_workspace_worktrees_for_waves_repo(s.repo.as_ref(), &s.events, sweeps).await?;
    Ok(())
}

/// #1147 S5 — reclaim this wave's managed workspace, between teardown and the
/// row delete.
///
/// **Ordering.** Teardown has already stopped every harness and terminal, so
/// nothing is writing into the directory; the row delete has not happened yet,
/// so a failure here aborts the whole DELETE with the wave and its directory
/// both intact and the request retryable. The reverse order (row first) would
/// turn a rename failure into "the wave is gone, its repository is not", which
/// is unretryable and needs a human.
///
/// Recycling is not conditional on that ordering being observed elsewhere: the
/// guards in [`workspace_recycle`] are what make the delete safe, not the
/// position of this call.
///
/// A *refusal* (guard not satisfied) is not an error — see
/// [`workspace_recycle::recycle_wave_workspace`] for why the row must stay
/// deletable even when the directory cannot be proven ours.
///
/// `area_kind` is guard 4's input, read once by the caller (which needs it for
/// the row-layer 403 anyway). `None` — an area row we could not read — is "not
/// provably a user area", and the recycler refuses on it.
fn recycle_wave_workspace_for_delete(
    s: &RouteState,
    wave: &Wave,
    area_kind: Option<AreaKind>,
) -> Result<()> {
    workspace_recycle::recycle_wave_workspace(
        &s.workspace_root,
        area_kind,
        wave.id.as_str(),
        &wave.workspace,
        crate::model::now_ms(),
    )?;
    workspace_recycle::gc_trash_best_effort(&s.workspace_root, crate::model::now_ms());
    Ok(())
}

#[utoipa::path(
    delete,
    path = "/api/waves/{id}",
    tag = "waves",
    params(("id" = String, Path, description = "Wave id")),
    responses(
        (status = 204, description = "Wave deleted"),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 403, description = "Wave belongs to the system area and cannot be deleted via REST", body = ErrorBody),
        (status = 409, description = "Wave has a descendant or active forge action", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn delete_wave(
    State(s): State<RouteState>,
    State(w): State<WorkerState>,
    State(cs): State<CodexShellState>,
    actor: Actor,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    // Issue #197 — eager teardown for every terminal under the wave.
    //
    // `terminals.card_id` is now `ON DELETE RESTRICT` (migration 0011)
    // so the prior model — let the FK cascade nuke the rows under us
    // and let the sweeper catch the leaked daemons ~60 s later —
    // doesn't work anymore: the cascade aborts the wave-delete txn.
    // This handler now owns the full subtree teardown:
    //
    //   1. Best-effort unlocked descendant preflight, then snapshot
    //      cards/terminals/runtimes.
    //   2. Outside SQLite: interrupt turns and stop terminal/harness processes.
    //   3. Short IMMEDIATE tx: recheck descendants authoritatively in
    //      `wave_delete_tx`, then remove terminal rows, overlays, leases and wave.
    let wave = s
        .repo
        .wave_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;
    let wave_id = wave.id.clone();

    // #1147 S5 — the ROW-layer half of recycle guard 4.
    //
    // `workspace_recycle`'s guard 4 refuses to touch a system-area workspace on
    // disk, and `DELETE /api/areas/{id}` already 403s a system area. This route
    // was the asymmetric one: it deleted a system-area wave row and returned
    // 204 while the directory (correctly) survived — and *that* is the leak,
    // because reclaiming a managed directory needs the wave row that names it.
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
    // launchpad wave — which *is* user-visible, on Today — cannot be deleted
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
    // of the system area, the three hidden template waves `ensure_templates`
    // seeded. Those are gone: a template is a Rust constant
    // (`crate::templates`) and creating from one mints no hidden wave. The
    // ruling did not depend on them — it is about where the boundary is drawn,
    // not about how many rows sit behind it — so it stands unchanged with the
    // launchpad wave as today's only kernel-seeded resident.
    let owning_area = s.repo.area_get(wave.area_id.as_str()).await?;
    if owning_area
        .as_ref()
        .is_some_and(|c| c.kind == AreaKind::System)
    {
        return Err(CalmError::Forbidden(format!(
            "wave {id} belongs to the system area and cannot be deleted via the public API"
        )));
    }

    // Defensive TOCTOU guard only: this non-transactional read happens before
    // the teardown tx, so a forge-action can still become in-flight before the
    // sweep. It shrinks the race; durable parked recovery is the backstop, and
    // the airtight in-tx/lease-hold guard belongs to slice ⑤.
    let pool = w.repo.sqlite_pool().ok_or_else(|| {
        CalmError::Internal("delete_wave forge-action fence requires sqlite-backed repo".into())
    })?;
    if wave_has_active_forge_action(&pool, wave_id.as_str()).await? {
        return Err(CalmError::Conflict(format!(
            "wave {id} has an in-flight forge-action; retry after it settles"
        )));
    }

    // Experience-only preflight: the in-transaction guard in `wave_delete_tx`
    // remains the sole correctness boundary for this route and raw Repo calls.
    // A child created after this read can still make the final delete return
    // Conflict after teardown; that rare race is safe and retryable.
    if let Some(child_id) =
        sqlx::query_scalar::<_, String>("SELECT id FROM waves WHERE parent_wave_id=?1 LIMIT 1")
            .bind(wave_id.as_str())
            .fetch_optional(&pool)
            .await?
    {
        return Err(CalmError::Conflict(format!(
            "wave {id} has child wave {child_id}; cancel it if needed, then delete that child wave first"
        )));
    }

    let plan = snapshot_wave_deletion(&s, &pool, &wave).await?;
    teardown_wave_deletion(&s, &w, &cs, &plan).await?;
    recycle_wave_workspace_for_delete(&s, &wave, owning_area.map(|c| c.kind))?;
    finish_wave_deletion(&s, plan, actor.to_actor_id()).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Issue #247 PR3 — user-facing wave-report edit endpoint
// ---------------------------------------------------------------------------

/// A report link from another wave that targets this wave.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct WaveBacklink {
    pub src_wave_id: String,
    pub src_wave_title: String,
    pub src_block_id: String,
    pub dst_block_id: Option<String>,
    pub label: String,
    pub quote: report_backlinks::BacklinkQuote,
    pub updated_at: i64,
}

/// A bounded page of report backlinks.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct WaveBacklinksResponse {
    pub backlinks: Vec<WaveBacklink>,
    pub truncated: bool,
    pub skipped_sources: usize,
}

impl From<report_backlinks::Backlink> for WaveBacklink {
    fn from(value: report_backlinks::Backlink) -> Self {
        Self {
            src_wave_id: value.src_wave_id,
            src_wave_title: value.src_wave_title,
            src_block_id: value.src_block_id,
            dst_block_id: value.dst_block_id,
            label: value.label,
            quote: value.quote,
            updated_at: value.updated_at,
        }
    }
}

impl From<report_backlinks::BacklinkPage> for WaveBacklinksResponse {
    fn from(value: report_backlinks::BacklinkPage) -> Self {
        Self {
            backlinks: value
                .backlinks
                .into_iter()
                .map(WaveBacklink::from)
                .collect(),
            truncated: value.truncated,
            skipped_sources: value.skipped_sources,
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/waves/{id}/backlinks",
    tag = "waves",
    params(("id" = String, Path, description = "Wave id")),
    responses(
        (status = 200, description = "Report links from waves in the same area", body = WaveBacklinksResponse),
        (status = 404, description = "Wave not found", body = ErrorBody),
    ),
)]
pub(crate) async fn get_wave_backlinks(
    State(s): State<RouteState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let page = report_backlinks::backlinks_for_wave(s.repo.as_ref(), &id).await?;
    Ok(Json(WaveBacklinksResponse::from(page)))
}

/// Request body for `POST /api/waves/:id/report`.
///
/// `summary` and `body` are required `String`s (per
/// `WaveReportPayload`'s [[required-over-option]] rule), and
/// `ifDocRev` is the required document-wide revision anchor. An empty
/// `summary` is valid; the caller must commit to *some* string.
///
/// **No `author` field.** Author is derived server-side from the
/// authenticated session and pinned to [`EditAuthor::User`] for this
/// endpoint — accepting one on the wire would let a User forge
/// `EditAuthor::Spec` and make a hand-typed edit look like the AI
/// did it. Even if a client serializes an `author` key the handler
/// ignores it (serde `deny_unknown_fields` would 400 it; this is the
/// stricter contract that closes the spoofing risk by construction).
///
/// `schemaVersion` is also intentionally absent — it's a server-managed
/// invariant pinned to [`WaveReportPayload::SCHEMA_VERSION`] and the
/// projected payload returned in the response reasserts the current
/// version. Letting clients write the version field would invite
/// silent shape drift the first time someone forgot to update both
/// sides.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateWaveReportBody {
    /// Expected document revision from the latest report read. Use zero
    /// for a document that has never been persisted through the CRDT path.
    pub if_doc_rev: u64,
    /// One-line summary the wave-list sidebars surface. Empty string
    /// is a valid value; the caller must commit.
    pub summary: String,
    /// Markdown source. Sections are derived at render time by
    /// splitting at H1 (`^# `) headings; the kernel does not interpret
    /// the structure.
    pub body: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WaveReportReadResponse {
    pub schema_version: u32,
    pub doc_rev: u64,
    pub summary: String,
    pub body: String,
    pub blocks: Vec<calm_types::wave_report::ReportBlock>,
    pub task_diagnostics: Vec<crate::db::sqlite::BlockVerdict>,
}

#[utoipa::path(
    get,
    path = "/api/waves/{id}/report",
    tag = "waves",
    params(("id" = String, Path, description = "Wave id")),
    responses(
        (status = 200, description = "Current report with derived task diagnostics", body = WaveReportReadResponse),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 404, description = "Wave not found", body = ErrorBody)
    ),
)]
pub(crate) async fn get_wave_report(
    State(s): State<RouteState>,
    _principal: Principal,
    Path(id): Path<String>,
) -> Result<Response> {
    let (_, report_card, _) = resolve_report_for_wave(s.repo.as_ref(), &id).await?;
    let snapshot = load_report_read_snapshot(s.repo.as_ref(), report_card.id.as_str()).await?;
    Ok((
        StatusCode::OK,
        Json(WaveReportReadResponse {
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

/// `POST /api/waves/:id/report` — user-driven wave-report edit. The
/// REST-side counterpart of the spec-MCP `calm.report.write` tool;
/// both paths funnel through the `wave_report::write` module — this
/// one via `rest_user_replace`, the tool via `agent_report_op` — so the
/// dual-event invariant (`CardUpdated` + `WaveReportEdited`) and the
/// CRDT write happen identically regardless of who's editing.
///
/// **Auth contract** (issue #247 PR3 acceptance):
///
///   * No session cookie → 401 (`auth::require_session` middleware
///     short-circuits before this handler runs).
///   * Authenticated session BUT non-user actor declared via
///     `X-Calm-Actor` (worker / `ai:*` / etc.) → 403. Only
///     [`ActorId::User`] is allowed. This closes the "spec card's
///     own session cookie forwards a User edit" hole — a future
///     surface that lets the spec card hold a session must not be
///     able to bypass the User-only contract by claiming `ai:codex`.
///   * Wave doesn't exist → 404.
///   * Wave exists but the wave-report card is missing → 500
///     (invariant violation; PR1 backfill guarantees the row).
///
/// The response is the *projected* [`WaveReportPayload`] read back
/// from the CRDT post-merge — not the request body verbatim — so the
/// frontend sees what every other reader will see (the JSON cache
/// mirrors the CRDT projection, which under single-writer is the
/// same bytes as the input, but reading from the doc keeps the
/// "CRDT is source of truth" contract true by construction).
#[utoipa::path(
    post,
    path = "/api/waves/{id}/report",
    tag = "waves",
    params(("id" = String, Path, description = "Wave id")),
    request_body = UpdateWaveReportBody,
    responses(
        (status = 200, description = "Updated wave-report payload", body = WaveReportPayload),
        (status = 401, description = "Missing or invalid session", body = ErrorBody),
        (status = 403, description = "Non-user actor (worker / plugin / spec) rejected", body = ErrorBody),
        (status = 409, description = "Report document revision conflict", body = ErrorBody),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 500, description = "Internal error (incl. missing report-card invariant)", body = ErrorBody),
    ),
)]
pub(crate) async fn update_wave_report(
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
    Json(body): Json<UpdateWaveReportBody>,
) -> Result<Response> {
    // Server-side actor pinning. The route is gated to `ActorId::User`
    // only — anything else (worker / spec / plugin / kernel) is 403.
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
    super::wave_report_blocks::require_rest_user_actor(&actor)?;

    // Resolve the wave + report card + current payload. 404 on missing
    // wave; 500 (Internal) on missing report card (invariant; PR1
    // backfill plus the partial unique index on `cards.kind =
    // 'wave-report'` guarantee one report row per wave).
    let target = wave_report::ReportEditTarget::resolve(s.repo.as_ref(), &id).await?;

    // Build the next payload from the request body. `schemaVersion` is
    // always the current constant — the field is not on the wire shape
    // (see `UpdateWaveReportBody` doc) so we stamp it here.
    let if_doc_rev = body.if_doc_rev;
    let next = WaveReportPayload::new(body.summary, body.body);

    // Persist + emit. `EditAuthor::User` is the load-bearing
    // attribution — the wire shape doesn't accept `author` (see the
    // request-body doc), so nothing the caller sends can change it.
    // PR5's spec system prompt will wake on
    // `WaveReportEdited { author: User }` specifically.
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
    let updated = wave_report::write::rest_user_replace(
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
    let payload: WaveReportPayload = serde_json::from_value(updated.payload).map_err(|e| {
        CalmError::Internal(format!(
            "wave-report edit: re-deserialize projected payload: {e}",
        ))
    })?;
    Ok((StatusCode::OK, Json(payload)).into_response())
}

#[cfg(test)]
mod tests {
    use super::{
        persist_initial_report_and_project_tasks_tx, prepare_fork_report,
        prepare_initial_report_payload, prepare_template_report, spec_harness_layout_payload,
    };
    use crate::db::prelude::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::model::{NewArea, NewCard, NewWave};
    use crate::routes::theme::RequestTheme;
    use crate::templates::TEMPLATES;
    use crate::wave_report::{ReportBlock, WaveReportPayload};
    use crate::wave_report_doc::ReportDoc;
    use serde_json::json;

    /// Every built-in recipe instantiates, and its declarations are the tasks
    /// it advertises.
    ///
    /// This is the unit half of #1300's evidence. The integration
    /// characterization test (`wave_template_waves.rs`) compares the created
    /// wave's *report* against the recipe; it cannot see the `declarations`
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
            let key = template.key;
            let (payload, _doc, declarations, _diagnostics) = prepare_template_report(key)
                .unwrap_or_else(|error| {
                    panic!("`{key}` must instantiate: {error}");
                });
            assert!(
                payload.blocks.as_ref().is_some_and(|b| !b.is_empty()),
                "`{key}`: no blocks"
            );
            let declared: Vec<&str> = declarations
                .iter()
                .map(|declaration| declaration.key.as_str())
                .collect();
            let fenced: Vec<String> = crate::templates::template_task_payloads(key)
                .expect("known key")
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
        let good = crate::templates::template_report("small-change").expect("known key");

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
            WaveReportPayload::new(good.summary.clone(), indented),
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
            WaveReportPayload::new(good.summary, broken),
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
    /// `wave_report_guard::validate_body_fences` at the whole-body write
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

    #[tokio::test]
    async fn fork_persist_helper_writes_cache_crdt_and_projection_together() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let area = repo
            .area_create(NewArea {
                name: "fork-helper".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
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
                wave_id: wave.id.clone(),
                kind: "wave-report".into(),
                sort: Some(-1.0),
                payload: serde_json::to_value(WaveReportPayload::initial()).unwrap(),
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
                    "refs": [format!("neige://wave/{}#b_1234", wave.id)],
                    "no_gate_reason": "covered by helper behavior test",
                    "ready": true,
                    "released_by_user": true,
                    "declared_by": "spec"
                }),
            },
        ];
        let mut doc = ReportDoc::from_blocks_exact("forked", &blocks).unwrap();
        let (summary, body) = doc.project().unwrap();
        let mut payload = WaveReportPayload::new(summary, body);
        payload.blocks = Some(blocks.clone());
        let payload_value = serde_json::to_value(payload).unwrap();
        let (declarations, diagnostics) =
            calm_types::report_blocks::tasks::project_task_declarations(&blocks);

        let pool = repo.sqlite_pool().unwrap();
        let mut tx = pool.begin().await.unwrap();
        let (updated, projection) = persist_initial_report_and_project_tasks_tx(
            &mut tx,
            report.id.as_str(),
            wave.id.as_str(),
            payload_value.clone(),
            &mut doc,
            &declarations,
            &diagnostics,
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
            sqlx::query_scalar("SELECT key FROM tasks WHERE wave_id=?1 AND key='projected'")
                .bind(wave.id.as_str())
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
    fn spec_harness_layout_payload_is_a_full_positions_write() {
        let payload = spec_harness_layout_payload("spec-1", "report-1");
        let positions = payload
            .get("positions")
            .and_then(|v| v.as_object())
            .expect("layout overlay.set payload must carry a full positions object");
        assert!(positions.contains_key("spec-1"));
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
        use serde_json::{Value, json};

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

        fn expect_bad_request(plugin: Option<&Manifest>, input: Option<&Value>, needle: &str) {
            match validate_template_input_binding(plugin, input) {
                Err(CalmError::BadRequest(message)) => {
                    assert!(message.contains(needle), "message `{message}` ∌ `{needle}`");
                }
                other => panic!("expected BadRequest containing `{needle}`, got {other:?}"),
            }
        }

        #[test]
        fn input_without_template_id_is_rejected() {
            expect_bad_request(None, Some(&json!({ "x": 1 })), "requires `template_id`");
        }

        #[test]
        fn no_template_no_input_is_ok() {
            validate_template_input_binding(None, None).expect("plain wave create unchanged");
        }

        #[test]
        fn input_against_schema_less_plugin_is_rejected_fail_closed() {
            let p = plugin(None);
            expect_bad_request(Some(&p), Some(&json!({ "x": 1 })), "does not declare");
            expect_bad_request(Some(&p), Some(&json!({ "x": 1 })), "plugin");
        }

        #[test]
        fn schema_less_binding_without_input_stays_valid() {
            let p = plugin(None);
            validate_template_input_binding(Some(&p), None).expect("bound create unchanged");
        }

        #[test]
        fn missing_input_with_required_schema_is_rejected() {
            let p = plugin(Some(schema(json!(["issue_url"]))));
            expect_bad_request(Some(&p), None, "requires `template_input`");
            expect_bad_request(Some(&p), None, "issue_url");
        }

        #[test]
        fn missing_input_with_no_required_fields_is_ok() {
            let p = plugin(Some(schema(json!([]))));
            validate_template_input_binding(Some(&p), None).expect("optional input omitted");
        }

        #[test]
        fn input_is_validated_against_the_plugin_schema() {
            let p = plugin(Some(schema(json!(["issue_url"]))));
            validate_template_input_binding(
                Some(&p),
                Some(&json!({ "issue_url": "u", "merge_policy": "auto-merge" })),
            )
            .expect("conforming input accepted");
            // INV-1110-003 — missing required / extra key / enum still 400.
            expect_bad_request(
                Some(&p),
                Some(&json!({ "merge_policy": "auto-merge" })),
                "template_input.issue_url",
            );
            expect_bad_request(
                Some(&p),
                Some(&json!({ "issue_url": "u", "ghost": true })),
                "template_input.ghost",
            );
            expect_bad_request(
                Some(&p),
                Some(&json!({ "issue_url": "u", "merge_policy": "yolo" })),
                "template_input.merge_policy",
            );
        }
    }
}
