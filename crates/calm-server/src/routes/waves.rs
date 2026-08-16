//! `/api/waves`, `/api/coves/:id/waves` — Wave CRUD. **Owned by Track B.**
//!
//! Writes go through `Repo::write_with_event` (via the
//! `write_with_event_typed` ergonomic wrapper). See `routes/coves.rs` for
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

use crate::COVE_CHAT_PURPOSE;
use crate::actor::Actor;
use crate::auth::Principal;
use crate::db::sqlite::{
    MAX_TREE_TASK_BUDGET, TaskProjectionOutcome, card_create_with_id_tx, card_update_with_crdt_tx,
    cove_folder_create_tx, cove_folders_list_all_tx, overlay_delete_by_entity_tx,
    overlay_delete_card_overlays_by_wave_tx, overlay_upsert_tx, project_tasks_tx,
    terminal_delete_tx, wave_create_tx, wave_delete_tx, wave_update_tx,
};
use crate::db::write_with_actor_events_typed;
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{EditAuthor, Event, EventScope};
use crate::forge_trust::trusted_forge_plugin;
use crate::ids::{ActorId, CardId, WaveId};
use crate::model::{
    Card, CardPatch, CardRole, CoveKind, FolderConflict, FolderConflictKind, NewCard, NewOverlay,
    NewWave, RequestTheme, Wave, WaveDetail, WavePatch, new_id,
};
use crate::operation::spec_harness_start_adapter::SpecHarnessStartOperationPayload;
use crate::operation::workspace_lease::{
    release_workspace_leases_for_wave_tx, sweep_workspace_worktrees_for_waves_repo,
    wave_has_active_forge_action,
};
use crate::operation::{OperationKey, OperationOutcome};
use crate::plugin_host::manifest::WorkflowDescriptor;
use crate::plugin_host::workflow_input::validate_workflow_input;
use crate::report_backlinks;
use crate::routes::cards::interrupt_shared_card_active_turn;
use crate::routes::cove_folders::{find_owner, is_descendant_of, normalize_path};
use crate::routes::terminal_cards::stable_payload_hash;
use crate::session_projection_lookup::project_runtime_into_cards_payload;
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};
use crate::terminal_sweeper::reap_terminal_artifacts_with_renderer;
use crate::validation::CODEX_PAYLOAD_SCHEMA_VERSION;
use crate::wave_fs_view::{WaveFsContent, WaveFsEntry, WaveFsView};
use crate::wave_lifecycle::{validate_transition, wave_get_tx};
use crate::wave_report::{
    ReportBlock, WaveReportPayload, persist_report, report_blocks_snapshot_tx,
    resolve_report_for_wave, tasks_rebuild_tree_tx, tasks_rebuild_tx,
};
use crate::wave_report_doc::ReportDoc;
use crate::wave_report_read::load_report_read_snapshot;
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
    cove_id: &str,
    barrier: std::sync::Arc<tokio::sync::Barrier>,
) {
    chat_wave_ensure_barriers()
        .lock()
        .expect("chat-wave ensure barrier lock poisoned")
        .insert(cove_id.to_string(), barrier);
}

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn remove_chat_wave_ensure_barrier_for_test(cove_id: &str) {
    chat_wave_ensure_barriers()
        .lock()
        .expect("chat-wave ensure barrier lock poisoned")
        .remove(cove_id);
}

#[cfg(feature = "fixtures")]
async fn wait_at_chat_wave_ensure_barrier(cove_id: &str) {
    let barrier = chat_wave_ensure_barriers()
        .lock()
        .expect("chat-wave ensure barrier lock poisoned")
        .get(cove_id)
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
    cove_id: crate::ids::CoveId,
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
    pub cove_id: crate::ids::CoveId,
    pub title: String,
    pub sort: Option<f64>,
    pub cwd: String,
    #[serde(default)]
    pub workflow_id: Option<String>,
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub workflow_input: Option<serde_json::Value>,
    #[serde(default)]
    pub attach_folder: bool,
    pub theme: RequestTheme,
    /// One-time creation instruction: copy this wave's report snapshot into
    /// the new report inside the wave-create transaction.
    #[serde(default)]
    pub fork_report_from: Option<String>,
}

impl CreateWaveRequest {
    fn into_parts(self) -> (NewWave, Option<String>) {
        (
            NewWave {
                cove_id: self.cove_id,
                title: self.title,
                sort: self.sort,
                cwd: self.cwd,
                workflow_id: self.workflow_id,
                workflow_input: self.workflow_input,
                attach_folder: self.attach_folder,
                theme: self.theme,
            },
            self.fork_report_from,
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
        // unchanged; both paths funnel through `wave_report::persist_report`
        // so the dual-event invariant + CRDT write stays one boundary.
        .route(
            "/api/waves/{id}/report",
            get(get_wave_report).post(update_wave_report),
        )
        .route("/api/waves/{id}/backlinks", get(get_wave_backlinks))
        .route("/api/waves/{id}/files/ls", get(list_wave_files))
        .route("/api/waves/{id}/files/cat", get(cat_wave_file))
        .route("/api/coves/{cove_id}/waves", get(list_waves_by_cove))
        .route(
            "/api/coves/{cove_id}/chat-wave/ensure",
            axum::routing::post(ensure_cove_chat_wave),
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
    path = "/api/coves/{cove_id}/waves",
    tag = "waves",
    params(("cove_id" = String, Path, description = "Cove id")),
    responses(
        (status = 200, description = "Waves under cove", body = Vec<Wave>),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn list_waves_by_cove(
    State(s): State<RouteState>,
    Path(cove_id): Path<String>,
) -> Result<Json<Vec<Wave>>> {
    let mut waves = s.repo.waves_by_cove(&cove_id).await?;
    waves.retain(user_visible_wave);
    Ok(Json(waves))
}

/// Public wave lists hide only the cove conversation container. Keep this at
/// the route boundary: repository readers such as cove deletion and backlink
/// resolution require the complete set.
///
/// The `match` is spelled out rather than written `!= Some(COVE_CHAT_PURPOSE)`
/// purely for readability — both forms already keep NULL-purpose waves
/// visible, because Rust comparison against `Option` is total. The three-valued
/// logic trap this must not be confused with lives in SQL, where
/// `purpose <> 'cove-chat'` drops NULL rows; the two hand-written predicates
/// that must spell out `purpose IS NULL OR ...` are in `session_repo_impl.rs`.
fn user_visible_wave(wave: &Wave) -> bool {
    match wave.purpose.as_deref() {
        None => true,
        Some(purpose) => purpose != COVE_CHAT_PURPOSE,
    }
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
    /// Optional per-cove filter. Mirrors `list_waves_by_cove` for
    /// callers that want one cove's window in a single endpoint.
    pub cove_id: Option<String>,
}

/// Issue #250 PR 2 — calendar / dashboard window query.
///
/// `GET /api/waves?since=<ms>&until=<ms>&cove_id=<id>` — every
/// parameter is optional. Returns the full wave row (so the frontend
/// can render lifecycle / cove / terminal-at without an N+1 detail
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
        .waves_window(q.cove_id.as_deref(), q.since, q.until)
        .await?;
    waves.retain(user_visible_wave);
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
    let (mut p, fork_report_from) = request.into_parts();
    // PR6 (#136) — wave create now atomically mints a `CardRole::Spec`
    // codex card alongside the wave row. Both rows commit in one tx
    // and both `Event::WaveUpdated` + `Event::CardAdded` envelopes
    // emit from the same commit, each tagged with its own scope so
    // per-wave and per-card subscribers each see the relevant frame
    // without re-routing through ancestors.
    //
    // Issue #250 PR 2 — the body now carries `cwd` (the wave's working
    // directory) and `attach_folder`. The wave's cwd is the source of
    // truth for the spec daemon's working directory (replacing the
    // pre-#250 `routes::codex_cards::default_cwd()` = `$HOME`). The
    // cwd must either resolve to the body's `cove_id` via the existing
    // folder claims, or — when `attach_folder = true` — get atomically
    // claimed as a new folder under that cove inside the same tx that
    // mints the wave row.

    // 0. Validate cwd up front before opening the tx. The route owns
    //    every cross-cove correctness check so the inner writer
    //    (`wave_create_tx`) stays a pure mechanical row insert. Order:
    //    absolute-path shape → normalize → existing-claim resolution
    //    → optional folder attach. All branches that surface a 4xx
    //    short-circuit before any DB write.
    let workflow_descriptor = match p.workflow_id.as_deref() {
        Some(workflow_id) => {
            let unknown_workflow = || {
                CalmError::BadRequest(format!(
                    "wave create: `workflow_id` must reference a registered trusted workflow; got `{workflow_id}`"
                ))
            };
            // Whitespace-only ids short-circuit before the registry lookup
            // (pre-#891 local-guard shape) and share the unknown-id 400.
            if workflow_id.trim().is_empty() {
                return Err(unknown_workflow());
            }
            let descriptor = resolve_trusted_workflow(&s, workflow_id)
                .await
                .ok_or_else(unknown_workflow)?;
            Some(descriptor)
        }
        None => None,
    };
    // #891 — `workflow_input` is only accepted against a bound descriptor
    // that declares an `input_schema`; validated here, before any DB write,
    // so the inner writer persists the blob verbatim.
    validate_workflow_input_binding(workflow_descriptor.as_ref(), p.workflow_input.as_ref())?;

    if !p.cwd.starts_with('/') {
        return Err(CalmError::BadRequest(format!(
            "wave create: `cwd` must be absolute (start with `/`); got `{}`",
            p.cwd
        )));
    }
    let normalized_cwd = normalize_path(&p.cwd);
    // Stamp the normalized cwd back onto the body before the wave row
    // is minted — the `cove_folder.path` we may attach below is also
    // the normalized form, so storing them in the same shape keeps
    // future "resolve by exact cwd" lookups simple.
    p.cwd = normalized_cwd.clone();

    // Issue #250 PR 2 fix — system cove (kernel-internal scaffolding,
    // hosts the default Today terminal's wave) is exempt from the
    // cove_folders claim namespace. The user can't reach it through
    // any user-facing surface, and claiming a path under it (e.g. the
    // initial `/` placeholder useTodayTerminal used) would poison
    // every real cove's descendant check. Look up the kind once here;
    // if System, skip both the pre-tx folder validation and the
    // in-tx attach. The cwd is still recorded on the wave row (the
    // spec daemon chdirs into it) but no `cove_folders` row is minted.
    let cove = s
        .repo
        .cove_get(p.cove_id.as_str())
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("cove `{}`", p.cove_id)))?;
    let is_system_cove = cove.kind == CoveKind::System;
    if is_system_cove {
        p.attach_folder = false;
    }

    let attach_folder = p.attach_folder;
    let body_cove_id = p.cove_id.as_str().to_string();

    // Issue #275 — the whole cwd-vs-claim decision (covering-folder scan,
    // reverse-overlap check, and the INSERT when `attach_folder`) now runs
    // inside the wave-create transaction. It used to be a pre-tx scan on a
    // separate pooled connection, which let two concurrent creates for
    // `/a` and `/a/b` both pass an empty-table scan and commit overlapping
    // claims — `UNIQUE(cove_folders.path)` only rejects *equal* paths.
    // Overlapping rows made the two resolvers disagree, and the wave
    // create then 409'd on a cove the UI had just been told to use.
    //
    // The tx is already `BEGIN IMMEDIATE` (see
    // `SqlxRepo::write_with_actor_events`), so this adds one SELECT under
    // a writer lock the create was taking anyway — it does not widen the
    // lock window with any new I/O.
    let conflict = FolderConflictSlot::default();
    let folder_claim = if is_system_cove {
        FolderClaim::Skip
    } else {
        FolderClaim::Enforce {
            attach: attach_folder,
            conflict: conflict.clone(),
        }
    };

    let created = create_wave_with_spec_harness(
        s,
        actor,
        p,
        CreateWaveOptions {
            folder_claim,
            body_cove_id,
            normalized_cwd,
            fork_report_from,
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

/// Resolve `workflow_id` to its descriptor iff a running **trusted** plugin
/// registers it — same filter as `bound_workflow_descriptor` on the spec
/// harness side. `None` covers unknown, stopped, and untrusted workflows
/// alike (the route deliberately does not distinguish them in the 400).
async fn resolve_trusted_workflow(s: &RouteState, workflow_id: &str) -> Option<WorkflowDescriptor> {
    let running_plugin_ids = s.plugin.running_plugin_ids().await;
    s.plugin
        .registry()
        .list()
        .into_iter()
        .filter(|manifest| {
            running_plugin_ids.contains(&manifest.id) && trusted_forge_plugin(&manifest.id)
        })
        .flat_map(|manifest| manifest.workflows)
        .find(|workflow| workflow.id == workflow_id)
}

/// #891 — create-time `workflow_input` validation matrix (design §1.4).
/// Fail-closed: input is only accepted when the bound descriptor declares an
/// `input_schema`, and a schema with required fields makes input mandatory.
/// The kernel never applies schema `default`s — the value persists exactly
/// as the caller sent it.
fn validate_workflow_input_binding(
    descriptor: Option<&WorkflowDescriptor>,
    input: Option<&serde_json::Value>,
) -> Result<()> {
    let Some(descriptor) = descriptor else {
        if input.is_some() {
            return Err(CalmError::BadRequest(
                "wave create: `workflow_input` requires `workflow_id`".into(),
            ));
        }
        return Ok(());
    };
    let workflow_id = &descriptor.id;
    match (descriptor.input_schema.as_ref(), input) {
        (None, None) => Ok(()),
        (None, Some(_)) => Err(CalmError::BadRequest(format!(
            "wave create: workflow `{workflow_id}` does not declare an input_schema; \
             `workflow_input` is not accepted"
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
                    "wave create: workflow `{workflow_id}` requires `workflow_input` \
                     (required: {required:?})"
                )))
            }
        }
        (Some(schema), Some(input)) => validate_workflow_input(schema, input)
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
            "wave create: cwd conflicts with folder claim `{}` (cove `{}`)",
            body.conflict_path, body.cove_id
        );
        *self.0.lock().expect("folder conflict slot poisoned") = Some(body);
        CalmError::Conflict(message)
    }

    fn take(&self) -> Option<FolderConflict> {
        self.0.lock().expect("folder conflict slot poisoned").take()
    }
}

/// Issue #275 — what the wave-create transaction does about `cove_folders`.
enum FolderClaim {
    /// Don't scan, don't insert. The system cove is exempt from the claim
    /// namespace entirely, and `ensure_cove_chat_wave_inner` derives its
    /// cwd from claims that already exist.
    Skip,
    /// Scan inside the wave tx (`BEGIN IMMEDIATE`, so scan and insert are
    /// atomic against a concurrent claim) and act on the result:
    /// `attach` mints the claim when nothing covers the cwd; without it a
    /// cwd no cove claims is refused rather than making a homeless wave.
    Enforce {
        attach: bool,
        conflict: FolderConflictSlot,
    },
}

struct CreateWaveOptions {
    folder_claim: FolderClaim,
    body_cove_id: String,
    normalized_cwd: String,
    fork_report_from: Option<String>,
}

#[allow(deprecated)]
async fn create_wave_with_spec_harness(
    s: RouteState,
    actor: Actor,
    p: NewWave,
    options: CreateWaveOptions,
) -> Result<Response> {
    let (wave, _, spec_card_id, report_card_id) =
        create_wave_structure(s.clone(), actor.clone(), p, options, None).await?;
    start_spec_harness(&s, &actor, &wave, spec_card_id, report_card_id).await?;
    Ok((StatusCode::CREATED, Json(wave)).into_response())
}

/// Ensure the cove's single chat wave exists.
///
/// The cwd is selected only while creating the wave: it is the claimed path
/// with the fewest path components, breaking ties lexicographically. Cove
/// folder claims cannot be equal, ancestors, or descendants of one another,
/// so "closest to the cove root" is defined here as this deterministic shallow
/// path ordering rather than containment. Once created, later folder claims or
/// changes deliberately do not update the wave cwd, so an existing conversation
/// cannot drift between working directories from one message to the next.
#[utoipa::path(
    post,
    path = "/api/coves/{cove_id}/chat-wave/ensure",
    tag = "waves",
    params(("cove_id" = String, Path, description = "Cove id")),
    responses(
        (status = 200, description = "Existing chat wave", body = Wave),
        (status = 201, description = "Chat wave created", body = Wave),
        (status = 409, description = "Cove has no claimed folder", body = ErrorBody),
        (status = 404, description = "Cove not found", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn ensure_cove_chat_wave(
    State(s): State<RouteState>,
    actor: Actor,
    Path(cove_id): Path<String>,
) -> Result<Response> {
    let (wave, created) = ensure_cove_chat_wave_inner(&s, actor, &cove_id).await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(wave)).into_response())
}

/// The body of [`ensure_cove_chat_wave`], shared with
/// `POST /api/coves/{cove_id}/conversations` (#1098 slice 3). Returns the
/// wave and whether this call is the one that created it.
///
/// Both callers must agree byte-for-byte on the cwd rule and the concurrent
/// ensure race resolution, so there is exactly one implementation of them.
#[allow(deprecated)]
pub(crate) async fn ensure_cove_chat_wave_inner(
    s: &RouteState,
    actor: Actor,
    cove_id: &str,
) -> Result<(Wave, bool)> {
    let cove_id = cove_id.to_string();
    if s.repo.cove_get(&cove_id).await?.is_none() {
        return Err(CalmError::NotFound(format!("cove {cove_id}")));
    }
    // Preserve the existing wave (and therefore its cwd) before consulting
    // current folder claims. The partial unique index closes the concurrent
    // ensure race; the loser reads and returns the winner below.
    if let Some(wave) = s
        .repo
        .waves_by_cove(&cove_id)
        .await?
        .into_iter()
        .find(|wave| wave.purpose.as_deref() == Some(COVE_CHAT_PURPOSE))
    {
        return Ok((wave, false));
    }

    #[cfg(feature = "fixtures")]
    wait_at_chat_wave_ensure_barrier(&cove_id).await;

    let cwd = s
        .repo
        .cove_folders_by_cove(&cove_id)
        .await?
        .into_iter()
        .min_by(|left, right| {
            std::path::Path::new(&left.path)
                .components()
                .count()
                .cmp(&std::path::Path::new(&right.path).components().count())
                .then_with(|| left.path.cmp(&right.path))
        })
        .map(|folder| folder.path)
        .ok_or_else(|| {
            CalmError::Conflict(format!(
                "chat wave ensure: cove `{cove_id}` has no claimed folder; claim a folder for this cove before starting a conversation"
            ))
        })?;
    let p = NewWave {
        cove_id: cove_id.clone().into(),
        title: "Cove chat".into(),
        sort: None,
        cwd: cwd.clone(),
        workflow_id: None,
        workflow_input: None,
        attach_folder: false,
        theme: RequestTheme::default_dark(),
    };
    let attempt = create_wave_structure(
        s.clone(),
        actor,
        p,
        CreateWaveOptions {
            // The cwd was just picked from this cove's own existing
            // claims, so there is nothing to scan and nothing to mint.
            folder_claim: FolderClaim::Skip,
            body_cove_id: cove_id.clone(),
            normalized_cwd: cwd,
            fork_report_from: None,
        },
        Some(COVE_CHAT_PURPOSE),
    )
    .await;
    let (wave, created) = match attempt {
        Ok((wave, _, _, _)) => (wave, true),
        Err(error) if is_unique_constraint(&error, "waves.cove_id") => {
            let wave = s
                .repo
                .waves_by_cove(&cove_id)
                .await?
                .into_iter()
                .find(|wave| wave.purpose.as_deref() == Some(COVE_CHAT_PURPOSE))
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
        body_cove_id,
        normalized_cwd,
        fork_report_from,
    } = options;
    let spec_card_id = new_id();
    let report_card_id = new_id();
    let actor_id = actor.to_actor_id();
    let actor_id_for_tx = actor_id.clone();
    let write_for_tx = s.write.clone();
    let spec_card_id_for_tx = spec_card_id.clone();
    let report_card_id_for_tx = report_card_id.clone();
    let cove_id_for_attach = body_cove_id;
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
                if let FolderClaim::Enforce { attach, conflict } = &folder_claim {
                    let existing = cove_folders_list_all_tx(tx).await?;
                    match find_owner(&existing, &normalized_cwd_for_tx) {
                        // Some other cove already covers this cwd.
                        // `Descendant` is the right label from the cwd's
                        // point of view: the cwd is a descendant of an
                        // existing folder owned by another cove.
                        Some(f) if f.cove_id.as_str() != cove_id_for_attach => {
                            return Err(conflict.park(FolderConflict {
                                folder_id: f.id,
                                cove_id: f.cove_id.clone(),
                                conflict_path: f.path.clone(),
                                conflict_kind: FolderConflictKind::Descendant,
                            }));
                        }
                        // Same cove already covers it — `attach_folder` is a
                        // no-op, create the wave only.
                        //
                        // #275 behavior change. Before this fix the insert
                        // ran unconditionally on the scan result, so this
                        // arm fell through into `cove_folder_create_tx`:
                        //   - cwd == the existing claim → UNIQUE(path) →
                        //     409 for re-claiming your own folder;
                        //   - cwd under the existing claim → a second,
                        //     overlapping row, minted from plain HTTP with
                        //     no concurrency at all.
                        // The latter is the larger hole in the "at most one
                        // claim covers any path" invariant — bigger and far
                        // easier to reach than the scan/insert TOCTOU.
                        // Pinned by `post_api_waves_attach_folder_*` in
                        // `tests/cases/wave_cwd_terminal_at.rs`.
                        Some(_) => {}
                        None if *attach => {
                            // No claim covers the cwd and the caller wants
                            // to mint one. Check the *reverse* overlap
                            // first: an existing folder that is a
                            // descendant of the proposed cwd (`/a/b`
                            // exists, claim `/a`). Refused for the same
                            // reason the cove_folders route refuses it —
                            // silently widening a narrower claim would
                            // make resolution ambiguous.
                            if let Some(f) = existing
                                .iter()
                                .find(|f| is_descendant_of(&normalized_cwd_for_tx, &f.path))
                            {
                                return Err(conflict.park(FolderConflict {
                                    folder_id: f.id,
                                    cove_id: f.cove_id.clone(),
                                    conflict_path: f.path.clone(),
                                    conflict_kind: FolderConflictKind::Ancestor,
                                }));
                            }
                            cove_folder_create_tx(
                                tx,
                                &cove_id_for_attach,
                                &normalized_cwd_for_tx,
                            )
                            .await?;
                        }
                        // Nothing covers the cwd and the caller didn't opt
                        // in to attach. Refuse so accidentally typing a
                        // stray path doesn't create a "homeless" wave.
                        None => {
                            return Err(CalmError::Conflict(format!(
                                "wave create: cwd `{normalized_cwd_for_tx}` is not claimed by \
                                 any cove. Set `attach_folder: true` to claim it for cove \
                                 `{cove_id_for_attach}`."
                            )));
                        }
                    }
                }

                let wave = wave_create_tx(tx, p, purpose, write_for_tx.cove_cache()).await?;
                let wave_id = wave.id.clone();
                let cove_id = wave.cove_id.clone();
                let goal = wave.title.trim().to_string();

                let fork_snapshot = if let Some(source_wave_id) = fork_report_from.as_deref() {
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
                    let source_cove_kind: String =
                        sqlx::query_scalar("SELECT kind FROM coves WHERE id=?1")
                            .bind(source_wave.cove_id.as_str())
                            .fetch_one(&mut **tx)
                            .await?;
                    if source_wave.cove_id != cove_id
                        && source_cove_kind != CoveKind::System.as_db_str()
                    {
                        return Err(CalmError::BadRequest(format!(
                            "wave create: fork source wave `{source_wave_id}` must be in the target cove or the system cove"
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
                } else {
                    None
                };

                let spec_card = card_create_with_id_tx(
                    tx,
                    spec_card_id_for_tx.clone(),
                    NewCard {
                        title: None,
                        wave_id: wave_id.clone(),
                        kind: "codex".into(),
                        sort: None,
                        payload: spec_harness_card_payload((!goal.is_empty()).then_some(goal)),
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

                let mut fork_projection = None;
                if let Some((payload, mut doc, declarations, diagnostics)) = fork_snapshot {
                    let payload = serde_json::to_value(payload).map_err(|error| {
                        CalmError::Internal(format!(
                            "wave_create: serialize forked wave-report payload: {error}"
                        ))
                    })?;
                    let (persisted_report, projection) =
                        persist_fork_report_and_project_tasks_tx(
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
                    fork_projection = Some(projection);
                }

                let wave_scope = EventScope::Wave {
                    wave: wave_id.clone(),
                    cove: cove_id.clone(),
                };
                let spec_card_scope = EventScope::Card {
                    card: spec_card.id.clone(),
                    wave: wave_id.clone(),
                    cove: cove_id.clone(),
                };
                let report_card_scope = EventScope::Card {
                    card: report_card.id.clone(),
                    wave: wave_id.clone(),
                    cove: cove_id.clone(),
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
                if let Some(projection) = fork_projection {
                    if !projection.changed_keys.is_empty() {
                        events.push((
                            actor_id_for_tx.clone(),
                            EventScope::Wave {
                                wave: wave_id.clone(),
                                cove: cove_id.clone(),
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

    Ok((wave, created, spec_card_id, report_card_id))
}

async fn start_spec_harness(
    s: &RouteState,
    actor: &Actor,
    wave: &Wave,
    spec_card_id: String,
    report_card_id: String,
) -> Result<()> {
    let goal = wave.title.trim().to_string();
    let request = SpecHarnessStartOperationPayload {
        actor: actor.to_actor_id(),
        wave_id: wave.id.to_string(),
        spec_card_id: CardId::from(spec_card_id.clone()),
        report_card_id: Some(report_card_id),
        sort: None,
        cwd: wave.cwd.clone(),
        goal: (!goal.is_empty()).then_some(goal),
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

type ForkReportSnapshot = (
    WaveReportPayload,
    ReportDoc,
    Vec<calm_types::report_blocks::tasks::TaskDeclaration>,
    Vec<Vec<calm_types::report_blocks::tasks::Diagnostic>>,
);

async fn persist_fork_report_and_project_tasks_tx(
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
) -> Result<ForkReportSnapshot> {
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

pub(crate) fn spec_harness_card_payload(goal: Option<String>) -> serde_json::Value {
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

#[utoipa::path(
    patch,
    path = "/api/waves/{id}",
    tag = "waves",
    params(("id" = String, Path, description = "Wave id")),
    request_body = WavePatch,
    responses(
        (status = 200, description = "Wave updated", body = Wave),
        (status = 404, description = "Wave not found", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
pub(crate) async fn update_wave(
    State(s): State<RouteState>,
    actor: Actor,
    Path(id): Path<String>,
    Json(p): Json<WavePatch>,
) -> Result<Json<Wave>> {
    // Need cove_id for the scope. Wave rows are immutable wrt their
    // parent cove, so reading outside the txn is safe (same rationale as
    // the delete path below).
    let existing = s
        .repo
        .wave_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;
    // The guard fires on *mentioning* `lifecycle`, not on changing it: a PATCH
    // that re-sends the wave's current lifecycle is 403 too. That is
    // deliberate — the chat wave has no lifecycle the user may drive, so
    // accepting a no-op write would advertise an editable field, and the FSM
    // would then have to be trusted to keep every such write a no-op forever.
    if existing.purpose.as_deref() == Some(COVE_CHAT_PURPOSE) && p.lifecycle.is_some() {
        return Err(CalmError::Forbidden(
            "cove chat wave lifecycle cannot be changed".into(),
        ));
    }
    let scope = EventScope::Wave {
        wave: existing.id.clone(),
        cove: existing.cove_id.clone(),
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
        return Ok(Json(existing));
    }

    // When a lifecycle change is part of the patch we emit *two*
    // events from the same txn: a `WaveLifecycleChanged` so dedicated
    // subscribers don't have to inspect every `WaveUpdated`, plus the
    // usual `WaveUpdated` so cache invalidation still sees the new
    // row shape. Both share scope + actor; both land or neither does.
    let cove_id_for_event = existing.cove_id.clone();
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
                            cove_id: cove_id_for_event.clone(),
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
                                cove: projected_wave.cove_id.clone(),
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
    Ok(Json(wave))
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
        cove_id: wave.cove_id.clone(),
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
    let cove_id = plan.cove_id.clone();
    let terminals = plan.terminals;
    let scope = EventScope::Wave {
        wave: wave_id.clone(),
        cove: cove_id.clone(),
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
                wave_delete_tx(tx, wave_id.as_str(), write_for_tx.cove_cache()).await?;
                events.push((
                    actor,
                    scope,
                    Event::WaveDeleted {
                        id: wave_id,
                        cove_id,
                    },
                ));
                Ok((release.sweep.into_iter().collect::<Vec<_>>(), events))
            })
        })
        .await?;
    sweep_workspace_worktrees_for_waves_repo(s.repo.as_ref(), &s.events, sweeps).await?;
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
        (status = 200, description = "Report links from waves in the same cove", body = WaveBacklinksResponse),
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
/// both paths funnel through [`crate::wave_report::persist_report`]
/// so the dual-event invariant (`CardUpdated` + `WaveReportEdited`)
/// and the CRDT write happen identically regardless of who's editing.
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
    // the persist call. Today the handler hardcodes
    // `EditAuthor::User` in the `persist_report` invocation below
    // regardless, so no audit-log corruption is possible — but the
    // OpenAPI / handler doc both claim "any non-user actor → 403" and
    // we want that to be true by construction, not "true because the
    // hardcoded author downstream covers for the gate." The raw
    // string check makes the gate honest: the *only* declared actor
    // that reaches `persist_report` here is exactly `"user"`. Every
    // other validated header value (`ai:codex`, `ai:claude`,
    // `ai:gpt5`, future `ai:*`) is 403.
    super::wave_report_blocks::require_rest_user_actor(&actor)?;

    // Resolve the wave + report card + current payload. 404 on missing
    // wave; 500 (Internal) on missing report card (invariant; PR1
    // backfill plus the partial unique index on `cards.kind =
    // 'wave-report'` guarantee one report row per wave).
    let (wave, report_card, current_payload) =
        resolve_report_for_wave(s.repo.as_ref(), &id).await?;

    // Build the next payload from the request body. `schemaVersion` is
    // always the current constant — the field is not on the wire shape
    // (see `UpdateWaveReportBody` doc) so we stamp it here.
    let if_doc_rev = body.if_doc_rev;
    let next = WaveReportPayload::new(body.summary, body.body);

    // Persist + emit. `EditAuthor::User` is the load-bearing
    // attribution — the wire shape doesn't accept `author` (see the
    // request-body doc), so this is the only place User can be
    // recorded. PR5's spec system prompt will wake on
    // `WaveReportEdited { author: User }` specifically.
    let updated = persist_report(
        s.repo.as_ref(),
        &s.events,
        &s.write,
        ActorId::User,
        EditAuthor::User,
        wave,
        report_card,
        current_payload,
        next,
        if_doc_rev,
        None,
        None,
        false,
    )
    .await?;

    // Project the persisted payload out of the updated card row. This
    // is the CRDT-projected shape (`wave_report::persist_report`
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
        persist_fork_report_and_project_tasks_tx, prepare_fork_report, spec_harness_layout_payload,
    };
    use crate::db::prelude::*;
    use crate::db::sqlite::SqlxRepo;
    use crate::model::{NewCard, NewCove, NewWave};
    use crate::routes::theme::RequestTheme;
    use crate::wave_report::{ReportBlock, WaveReportPayload};
    use crate::wave_report_doc::ReportDoc;
    use serde_json::json;

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

    #[tokio::test]
    async fn fork_persist_helper_writes_cache_crdt_and_projection_together() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let cove = repo
            .cove_create(NewCove {
                name: "fork-helper".into(),
                color: "#000".into(),
                sort: None,
            })
            .await
            .unwrap();
        let wave = repo
            .wave_create(NewWave {
                cove_id: cove.id,
                title: "fork helper".into(),
                sort: None,
                cwd: "/tmp/fork-helper".into(),
                workflow_id: None,
                workflow_input: None,
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
        let (updated, projection) = persist_fork_report_and_project_tasks_tx(
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

    /// #891 — the create-time `workflow_input` validation matrix (design
    /// §1.4). Schema-conformance details are pinned in
    /// `plugin_host::workflow_input`; this covers the binding combinations.
    mod workflow_input_binding {
        use super::super::validate_workflow_input_binding;
        use crate::error::CalmError;
        use crate::plugin_host::manifest::WorkflowDescriptor;
        use serde_json::{Value, json};

        fn descriptor(input_schema: Option<Value>) -> WorkflowDescriptor {
            WorkflowDescriptor {
                id: "issue-development".into(),
                plan_template: vec![],
                gates: vec![],
                spec_instructions: String::new(),
                card_kinds: vec![],
                input_schema,
            }
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

        fn expect_bad_request(
            descriptor: Option<&WorkflowDescriptor>,
            input: Option<&Value>,
            needle: &str,
        ) {
            match validate_workflow_input_binding(descriptor, input) {
                Err(CalmError::BadRequest(message)) => {
                    assert!(message.contains(needle), "message `{message}` ∌ `{needle}`");
                }
                other => panic!("expected BadRequest containing `{needle}`, got {other:?}"),
            }
        }

        #[test]
        fn input_without_workflow_id_is_rejected() {
            expect_bad_request(None, Some(&json!({ "x": 1 })), "requires `workflow_id`");
        }

        #[test]
        fn no_workflow_no_input_is_ok() {
            validate_workflow_input_binding(None, None).expect("plain wave create unchanged");
        }

        #[test]
        fn input_against_schema_less_descriptor_is_rejected_fail_closed() {
            let d = descriptor(None);
            expect_bad_request(Some(&d), Some(&json!({ "x": 1 })), "does not declare");
        }

        #[test]
        fn schema_less_binding_without_input_stays_valid() {
            // Today's git-forge binding (no input_schema yet) — slice ① must
            // not change its behavior.
            let d = descriptor(None);
            validate_workflow_input_binding(Some(&d), None).expect("bound create unchanged");
        }

        #[test]
        fn missing_input_with_required_schema_is_rejected() {
            let d = descriptor(Some(schema(json!(["issue_url"]))));
            expect_bad_request(Some(&d), None, "requires `workflow_input`");
            expect_bad_request(Some(&d), None, "issue_url");
        }

        #[test]
        fn missing_input_with_no_required_fields_is_ok() {
            let d = descriptor(Some(schema(json!([]))));
            validate_workflow_input_binding(Some(&d), None).expect("optional input omitted");
        }

        #[test]
        fn input_is_validated_against_the_schema() {
            let d = descriptor(Some(schema(json!(["issue_url"]))));
            validate_workflow_input_binding(
                Some(&d),
                Some(&json!({ "issue_url": "u", "merge_policy": "auto-merge" })),
            )
            .expect("conforming input accepted");
            // Failure names the offending field.
            expect_bad_request(
                Some(&d),
                Some(&json!({ "issue_url": "u", "merge_policy": "yolo" })),
                "workflow_input.merge_policy",
            );
        }
    }
}
