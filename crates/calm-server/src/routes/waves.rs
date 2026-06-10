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
//! `delete_wave` enumerates every card under the wave (including the
//! spec card), reaps each terminal's daemon + socket via
//! `terminal_sweeper::reap_terminal_artifacts`, then drops the terminal
//! rows and the wave row in one transaction. The
//! `terminals.card_id` FK is `ON DELETE RESTRICT` (migration 0011),
//! so a missed cleanup surfaces as a transaction-level error rather
//! than a silent daemon-process leak.

use crate::actor::Actor;
use crate::auth::Principal;
use crate::db::sqlite::{
    card_create_with_id_tx, cove_folder_create_tx, overlay_delete_by_entity_tx,
    overlay_delete_card_overlays_by_wave_tx, overlay_upsert_tx, terminal_delete_tx, wave_create_tx,
    wave_delete_tx, wave_update_tx,
};
use crate::db::{write_with_event_typed, write_with_events_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{EditAuthor, Event, EventScope};
use crate::ids::{ActorId, CardId};
use crate::model::{
    CardRole, CoveKind, FolderConflict, FolderConflictKind, NewCard, NewOverlay, NewWave, Wave,
    WaveDetail, WavePatch, new_id,
};
use crate::operation::spec_harness_start_adapter::SpecHarnessStartOperationPayload;
use crate::operation::{OperationKey, OperationOutcome};
use crate::routes::cards::interrupt_shared_card_active_turn;
use crate::routes::cove_folders::{is_descendant_of, normalize_path};
use crate::routes::terminal_cards::stable_payload_hash;
use crate::runtime_lookup::project_runtime_into_cards_payload;
use crate::state::{AppState, CodexShellState, RouteState, WorkerState};
use crate::terminal_sweeper::reap_terminal_artifacts_with_renderer;
use crate::validation::CODEX_PAYLOAD_SCHEMA_VERSION;
use crate::wave_fs_view::{WaveFsContent, WaveFsEntry, WaveFsView};
use crate::wave_lifecycle::validate_transition;
use crate::wave_report::{WaveReportPayload, persist_report, resolve_report_for_wave};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

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
        .route("/api/waves/{id}/report", post(update_wave_report))
        .route("/api/waves/{id}/files/ls", get(list_wave_files))
        .route("/api/waves/{id}/files/cat", get(cat_wave_file))
        .route("/api/coves/{cove_id}/waves", get(list_waves_by_cove))
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
    let waves = s.repo.waves_by_cove(&cove_id).await?;
    Ok(Json(waves))
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
    let waves = state
        .repo
        .waves_window(q.cove_id.as_deref(), q.since, q.until)
        .await?;
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
    request_body = NewWave,
    responses(
        (status = 201, description = "Wave created", body = Wave),
        (status = 500, description = "Internal error", body = ErrorBody),
    ),
)]
#[allow(deprecated)]
pub(crate) async fn create_wave(
    State(s): State<RouteState>,
    actor: Actor,
    Json(mut p): Json<NewWave>,
) -> Result<Response> {
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

    if !is_system_cove {
        // Pre-tx claim scan. The route runs every cwd-vs-folder check
        // outside the tx so the structured 409 (`FolderConflict`) can be
        // returned without a custom in-tx error variant. The UNIQUE
        // constraint on `cove_folders.path` provides a concurrent-insert
        // backstop inside the tx; concurrent `attach_folder = true`
        // requests for the same cwd surface as a generic 409 from the
        // sqlite layer.
        let existing_folders = s.repo.cove_folders_list_all().await?;

        // Step 1 — find a covering folder (cwd is descendant of or equal
        // to some claim). At most one row qualifies as the *longest*
        // prefix; ancestor/equal claims under different coves are a
        // hard conflict, under the same cove are a silent no-op.
        let owner = existing_folders
            .iter()
            .filter(|f| is_descendant_of(&f.path, &normalized_cwd))
            .max_by_key(|f| f.path.len());
        if let Some(f) = owner {
            if f.cove_id.as_str() != body_cove_id {
                let body = FolderConflict {
                    folder_id: f.id,
                    cove_id: f.cove_id.clone(),
                    conflict_path: f.path.clone(),
                    // `Descendant` is the right label from the cwd's
                    // point of view: the cwd is a descendant of an
                    // existing folder owned by another cove.
                    conflict_kind: FolderConflictKind::Descendant,
                };
                return Ok((StatusCode::CONFLICT, Json(body)).into_response());
            }
            // Same cove already covers it — silently ignore
            // `attach_folder`. Fall through to wave-only create.
        } else if attach_folder {
            // Step 2 — no claim covers the cwd, but the caller wants to
            // mint one. Re-check for the *reverse* overlap: any existing
            // folder that is a descendant of the proposed cwd. This is
            // the `/a/b exists, claim /a` case that the cove_folders
            // route refuses with `FolderConflictKind::Ancestor`. We
            // refuse here for the same reason — silently widening an
            // existing narrower claim would make resolution ambiguous.
            if let Some(f) = existing_folders
                .iter()
                .find(|f| is_descendant_of(&normalized_cwd, &f.path))
            {
                let body = FolderConflict {
                    folder_id: f.id,
                    cove_id: f.cove_id.clone(),
                    conflict_path: f.path.clone(),
                    conflict_kind: FolderConflictKind::Ancestor,
                };
                return Ok((StatusCode::CONFLICT, Json(body)).into_response());
            }
            // Cwd is fully unclaimed (no ancestor, no descendant) — the
            // in-tx `cove_folder_create_tx` will insert the row.
        } else {
            // No claim covers the cwd and the caller didn't opt in to
            // attach. Refuse so accidentally typing a stray path doesn't
            // create a "homeless" wave.
            return Err(CalmError::Conflict(format!(
                "wave create: cwd `{normalized_cwd}` is not claimed by any cove. \
                 Set `attach_folder: true` to claim it for cove `{body_cove_id}`."
            )));
        }
    }

    create_wave_with_spec_harness(s, actor, p, attach_folder, body_cove_id, normalized_cwd).await
}

#[allow(deprecated)]
async fn create_wave_with_spec_harness(
    s: RouteState,
    actor: Actor,
    p: NewWave,
    attach_folder: bool,
    body_cove_id: String,
    normalized_cwd: String,
) -> Result<Response> {
    let spec_card_id = new_id();
    let report_card_id = new_id();
    let actor_for_hash = actor.as_str().to_string();
    let actor_id = actor.to_actor_id();
    let write_for_tx = s.write.clone();
    let spec_card_id_for_tx = spec_card_id.clone();
    let report_card_id_for_tx = report_card_id.clone();
    let cove_id_for_attach = body_cove_id;
    let normalized_cwd_for_tx = normalized_cwd;
    let ((wave,), _event_ids) = write_with_events_typed(
        s.repo.as_ref(),
        actor_id.clone(),
        None,
        &s.events,
        &s.write,
        move |tx| {
            Box::pin(async move {
                if attach_folder {
                    cove_folder_create_tx(tx, &cove_id_for_attach, &normalized_cwd_for_tx).await?;
                }

                let wave = wave_create_tx(tx, p, write_for_tx.cove_cache()).await?;
                let wave_id = wave.id.clone();
                let cove_id = wave.cove_id.clone();
                let goal = wave.title.trim().to_string();
                let spec_card = card_create_with_id_tx(
                    tx,
                    spec_card_id_for_tx.clone(),
                    NewCard {
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
                let report_card = card_create_with_id_tx(
                    tx,
                    report_card_id_for_tx.clone(),
                    NewCard {
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
                let events = vec![
                    (
                        wave_scope.clone(),
                        Event::WaveUpdated(crate::event::WaveUpdatedPayload::new(
                            wave.clone(),
                            None,
                        )),
                    ),
                    (spec_card_scope, Event::CardAdded(spec_card)),
                    (report_card_scope, Event::CardAdded(report_card)),
                    (wave_scope, Event::OverlaySet(layout_overlay)),
                ];
                Ok(((wave,), events))
            })
        },
    )
    .await?;

    let goal = wave.title.trim().to_string();
    let request = SpecHarnessStartOperationPayload {
        actor: actor_id,
        wave_id: wave.id.to_string(),
        spec_card_id: CardId::from(spec_card_id.clone()),
        report_card_id: Some(report_card_id),
        sort: None,
        cwd: wave.cwd.clone(),
        goal: (!goal.is_empty()).then_some(goal),
        reset_harness_items: false,
        force_new_thread: false,
    };
    let op_payload = serde_json::to_value(&request)?;
    let payload_hash = stable_payload_hash(&serde_json::json!({
        "actor": actor_for_hash,
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

    Ok((StatusCode::CREATED, Json(wave)).into_response())
}

fn spec_harness_card_payload(goal: Option<String>) -> serde_json::Value {
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

fn spec_harness_layout_payload(spec_card_id: &str, report_card_id: &str) -> serde_json::Value {
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
    let scope = EventScope::Wave {
        wave: existing.id.clone(),
        cove: existing.cove_id.clone(),
    };
    let actor_id = actor.to_actor_id();

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

    // If the patch is now entirely empty (lifecycle was a no-op and
    // no other field was supplied) there's nothing to write and
    // nothing to emit — return the wave as-is. This is the
    // idempotent retry path for "spec re-sends the current state."
    let patch_has_other_changes =
        p.title.is_some() || p.sort.is_some() || p.archived_at.is_some() || p.pinned_at.is_some();
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
    let p_for_tx = p.clone();
    let (wave, _ids) = write_with_events_typed(
        s.repo.as_ref(),
        actor_id,
        None,
        &s.events,
        &s.write,
        move |tx| {
            let scope = scope.clone();
            Box::pin(async move {
                let wave = wave_update_tx(tx, &id, p_for_tx).await?;
                let mut events: Vec<(EventScope, Event)> = Vec::new();
                if let Some((from, to)) = lifecycle_change {
                    events.push((
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
                    scope,
                    Event::WaveUpdated(crate::event::WaveUpdatedPayload::new(wave.clone(), None)),
                ));
                Ok((wave, events))
            })
        },
    )
    .await?;
    Ok(Json(wave))
}

#[utoipa::path(
    delete,
    path = "/api/waves/{id}",
    tag = "waves",
    params(("id" = String, Path, description = "Wave id")),
    responses(
        (status = 204, description = "Wave deleted"),
        (status = 404, description = "Wave not found", body = ErrorBody),
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
    //   1. Enumerate every card under the wave (`cards_by_wave`).
    //   2. Resolve each card's terminal row (if any) via
    //      `terminal_get_by_card`.
    //   3. Call `reap_terminal_artifacts` for each — kills the daemon
    //      + unlinks the socket. Spec cards (CardRole::Spec) take this
    //      same path; the spec card daemon TODO from PR6 is now
    //      handled here.
    //   4. Inside the write txn, drop each terminal row first
    //      (`terminal_delete_tx`), then drop the wave row. The cards
    //      cascade away from the wave; the FK to terminals is honored
    //      because we've already drained the table for this subtree.
    //
    // The outside-txn card walk is only for terminal process reaping.
    // Overlay cleanup happens inside the delete transaction via a DB
    // subquery, so a card+overlay created after this snapshot but before
    // the wave delete commits is still swept before the FK cascade drops
    // the card.
    let wave = s
        .repo
        .wave_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;
    let cove_id = wave.cove_id.clone();
    let wave_id = wave.id.clone();

    let mut terminal_ids: Vec<String> = Vec::new();
    let mut active_runtime_ids: Vec<String> = Vec::new();
    let cards = s.repo.cards_by_wave(wave_id.as_str()).await?;
    for card in &cards {
        interrupt_shared_card_active_turn(s.repo.as_ref(), &cs, card).await;
        if let Some(runtime) = s
            .repo
            .runtime_get_active_for_card(&card.id.to_string())
            .await?
        {
            active_runtime_ids.push(runtime.id);
        }
        if let Some(t) = s.repo.terminal_get_by_card(card.id.as_str()).await? {
            reap_terminal_artifacts_with_renderer(Some(w.terminal_renderer.as_ref()), &t).await;
            terminal_ids.push(t.id);
        }
    }
    for runtime_id in active_runtime_ids {
        if let Some(harness) = w.harness.remove(&runtime_id) {
            harness.shutdown().await?;
        }
    }

    let scope = EventScope::Wave {
        wave: wave_id.clone(),
        cove: cove_id.clone(),
    };
    let write_for_tx = s.write.clone();
    let (_unit, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.write,
        move |tx| {
            Box::pin(async move {
                // Drop terminal rows first so the RESTRICT FK lets the
                // wave delete cascade through cards cleanly.
                // Idempotent: tolerate NotFound on each row in case a
                // racing sweeper tick beat us to it.
                for tid in &terminal_ids {
                    match terminal_delete_tx(tx, tid).await {
                        Ok(()) => {}
                        Err(CalmError::NotFound(_)) => {}
                        Err(e) => return Err(e),
                    }
                }
                overlay_delete_card_overlays_by_wave_tx(tx, wave_id.as_str()).await?;
                overlay_delete_by_entity_tx(tx, "wave", wave_id.as_str()).await?;
                overlay_delete_by_entity_tx(tx, "view", wave_id.as_str()).await?;
                wave_delete_tx(tx, wave_id.as_ref(), write_for_tx.cove_cache()).await?;
                Ok((
                    (),
                    Event::WaveDeleted {
                        id: wave_id,
                        cove_id,
                    },
                ))
            })
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Issue #247 PR3 — user-facing wave-report edit endpoint
// ---------------------------------------------------------------------------

/// Request body for `POST /api/waves/:id/report`.
///
/// Both fields are required `String`s (per `WaveReportPayload`'s
/// [[required-over-option]] rule). An empty `summary` is a valid
/// value; the caller must commit to *some* string.
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
    /// One-line summary the wave-list sidebars surface. Empty string
    /// is a valid value; the caller must commit.
    pub summary: String,
    /// Markdown source. Sections are derived at render time by
    /// splitting at H1 (`^# `) headings; the kernel does not interpret
    /// the structure.
    pub body: String,
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
    if actor.as_str() != "user" {
        return Err(CalmError::Forbidden(format!(
            "wave-report edit: only `X-Calm-Actor: user` is allowed via REST; \
             got `{}`. MCP write paths use `calm.report.*` tools.",
            actor.as_str()
        )));
    }

    // Resolve the wave + report card + current payload. 404 on missing
    // wave; 500 (Internal) on missing report card (invariant; PR1
    // backfill plus the partial unique index on `cards.kind =
    // 'wave-report'` guarantee one report row per wave).
    let (wave, report_card, current_payload) =
        resolve_report_for_wave(s.repo.as_ref(), &id).await?;

    // Build the next payload from the request body. `schemaVersion` is
    // always the current constant — the field is not on the wire shape
    // (see `UpdateWaveReportBody` doc) so we stamp it here.
    let next = WaveReportPayload {
        schema_version: WaveReportPayload::SCHEMA_VERSION,
        summary: body.summary,
        body: body.body,
    };

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
