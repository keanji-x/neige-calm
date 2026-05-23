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
//! ## Issue #236 (closes) — synchronous spec daemon spawn
//!
//! Earlier iterations of this handler ran the post-commit
//! `seed_and_spawn_spec_daemon` call inside `tokio::spawn` so the route
//! could return 201 instantly. That introduced a TOCTOU race against
//! the WS terminal revive path in `ws::terminal::resolve_live_sock`:
//! the frontend would open the spec card's WS in the ~400 ms window
//! between commit and the background task running, see
//! `daemon_handle = None`, and respawn the daemon from the **baked
//! terminal-row env**, which is missing `NEIGE_MCP_SOCKET` /
//! `NEIGE_MCP_TOKEN` (those vars are folded in only on the post-commit
//! env clone, never persisted to the terminal row). Two daemons would
//! then race on the same `--sock` and the WS would attach to the
//! no-MCP one, breaking the codex MCP handshake.
//!
//! The fix awaits the seed + spawn inline before returning 201, so the
//! response payload never reflects a daemon-less spec card. If the
//! daemon spawn fails the handler returns 500 — the wave is unusable
//! without its daemon — but the DB tx is **not** rolled back: the wave
//! row stays so the user can retry the daemon spawn out-of-band rather
//! than losing their typed title. Persisted rows + the two events
//! still survive on the 500 path; the orphan-terminal sweeper reaps
//! the dangling terminal row (~60 s grace) if the user doesn't retry.
//!
//! The earlier rationale for the `tokio::spawn` form was the
//! `spawn_daemon_for` busy-poll wait-until-socket-ready loop (~3 s
//! worst case when the daemon binary is missing). That latency
//! affected one specific test path (`web/e2e/a11y-keyboard.spec.ts`'s
//! 5 s navigation budget when running without a real codex). The
//! tradeoff was wrong: the WS race is a correctness bug for every
//! production user, the a11y timeout is a CI-only ergonomic concern.
//! The a11y test stack is expected to bump its navigation budget to
//! accommodate.
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
use crate::db::sqlite::{
    card_create_with_id_tx, card_with_codex_create_tx, cove_folder_create_tx, overlay_upsert_tx,
    terminal_delete_tx, wave_create_tx, wave_delete_tx, wave_update_tx,
};
use crate::db::{write_with_event_typed, write_with_events_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::model::{
    CardRole, FolderConflict, FolderConflictKind, NewCard, NewOverlay, NewWave, Wave, WaveDetail,
    WavePatch, new_id,
};
use crate::routes::cove_folders::{is_descendant_of, normalize_path};
use crate::routes::settings::load_settings;
use crate::spec_card::{build_codex_env_map, seed_and_spawn_spec_daemon};
use crate::state::AppState;
use crate::terminal_sweeper::reap_terminal_artifacts;
use crate::wave_lifecycle::validate_transition;
use crate::wave_report::WaveReportPayload;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
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
        .route("/api/coves/{cove_id}/waves", get(list_waves_by_cove))
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
    State(s): State<AppState>,
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
    State(state): State<AppState>,
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
    State(s): State<AppState>,
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
pub(crate) async fn create_wave(
    State(s): State<AppState>,
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

    // Pre-tx claim scan. The route runs every cwd-vs-folder check
    // outside the tx so the structured 409 (`FolderConflict`) can be
    // returned without a custom in-tx error variant. The UNIQUE
    // constraint on `cove_folders.path` provides a concurrent-insert
    // backstop inside the tx; concurrent `attach_folder = true`
    // requests for the same cwd surface as a generic 409 from the
    // sqlite layer.
    let existing_folders = s.repo.cove_folders_list_all().await?;
    let attach_folder = p.attach_folder;
    let body_cove_id = p.cove_id.as_str().to_string();

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

    // 1. Pre-mint the spec card id BEFORE the tx opens — we need it to
    //    derive `CODEX_HOME = <codex_homes_dir>/<card_id>/` for the
    //    env map we hand the daemon spawn post-commit. The wave id is
    //    minted inside `wave_create_tx` (PR2 stopgap precedent in
    //    `routes/coves.rs`); we read it back from the closure result.
    let spec_card_id = new_id();
    // Issue #229 PR B — wave-report card id, pre-minted alongside the
    // spec id so the layout overlay seeded later in the tx can
    // reference it without needing the closure's return value. Same
    // generator (`uuid-v4 simple`) the rest of the kernel uses; the
    // partial unique index `idx_cards_one_report_per_wave` from
    // migration 0013 backstops the "one report card per wave"
    // invariant if a future code path races itself.
    let report_card_id = new_id();

    // 2. Issue #250 PR 2 — the spec daemon's cwd is the wave's cwd
    //    (validated above). Pre-#250 this defaulted to `$HOME` because
    //    waves had no cwd field; the new model is one cwd per wave,
    //    inherited by the spec daemon and any future cwd-anchored
    //    surfaces. The same string lands on three rows in the tx:
    //    `waves.cwd`, the spec card's terminal row, and (when
    //    `attach_folder = true`) a fresh `cove_folders.path` claim.
    let cwd = normalized_cwd.clone();
    let settings = load_settings(s.repo.as_ref()).await?;
    // PR7a (#136) — env baked into the terminal row is the pre-MCP
    // shape (no `NEIGE_MCP_TOKEN` / `NEIGE_MCP_SOCKET` yet). The token
    // is minted inside the tx below; the env handed to the codex
    // daemon spawn is augmented post-commit just before
    // `seed_and_spawn_spec_daemon`. Restarting from the terminal-row
    // env on a future cold-start path will need to re-derive these
    // from `card_mcp_tokens` + `mcp_server.shim_config`, but that's
    // not exercised today (PR8 followup).
    let env = build_codex_env_map(
        s.codex.as_ref(),
        &spec_card_id,
        settings.http_proxy.as_deref(),
        settings.https_proxy.as_deref(),
        None,
        None,
    );

    // 3. Run the atomic tx: wave row + spec card row + spec terminal
    //    row + two events in one commit.
    //
    //    Order of operations inside the closure matters:
    //      a. `wave_create_tx` first (mints wave_id, validates cove)
    //      b. `card_with_codex_create_tx` with `CardRole::Spec` second
    //         (the write-through into `card_role_cache` makes the spec
    //         card immediately visible to `enforce_role`, which the
    //         outer plural-events writer calls per emitted event before
    //         persisting)
    //      c. Build the scopes from the actual minted wave + cove ids
    //      d. Return `(Wave, Vec<(EventScope, Event)>)`
    //
    //    No `EventScope::Cove`-fallback dance: by the time the closure
    //    runs, we know wave_id, so each event gets its tightest scope.
    let actor_id = actor.to_actor_id();
    let cache_for_tx = s.card_role_cache.clone();
    let wcc_for_tx = s.wave_cove_cache.clone();
    let env_for_tx = env.clone();
    let cwd_for_tx = cwd.clone();
    let spec_card_id_for_tx = spec_card_id.clone();
    let report_card_id_for_tx = report_card_id.clone();
    let cove_id_for_attach = body_cove_id.clone();
    let normalized_cwd_for_tx = normalized_cwd.clone();
    let ((wave, mcp_token), _event_ids) = write_with_events_typed(
        s.repo.as_ref(),
        actor_id,
        None,
        &s.events,
        &s.card_role_cache,
        &s.wave_cove_cache,
        move |tx| {
            Box::pin(async move {
                // 3.0. Issue #250 PR 2 — optional folder attach.
                // Pre-tx the route ran the full ancestor/descendant
                // scan and rejected every structured conflict with a
                // typed `FolderConflict` body. Here we only need the
                // insert; a concurrent claim from another connection
                // between the pre-scan and now surfaces as the
                // UNIQUE-constraint violation in `cove_folder_create_tx`
                // (which maps it to `CalmError::Conflict`). That
                // hands the caller a generic 409 rather than a
                // structured body — acceptable for a race window
                // measured in milliseconds; the typed shape is
                // reserved for the deterministic prior-state case
                // covered by the pre-scan.
                if attach_folder {
                    cove_folder_create_tx(tx, &cove_id_for_attach, &normalized_cwd_for_tx).await?;
                }

                // 3a. Wave row.
                let wave = wave_create_tx(tx, p, &wcc_for_tx).await?;
                let wave_id = wave.id.clone();
                let cove_id = wave.cove_id.clone();

                // 3b. Spec card + terminal row. The helper's
                // `card_create_with_id_tx` writes through into the
                // role cache (`Spec` for this call) so the next
                // `enforce_role` step sees it. PR7a adds the third
                // return slot — the raw per-card MCP token. For the
                // spec card it'll be `Some`; we capture and re-emit
                // it as the row return so the post-commit closure
                // can fold it into the env map handed to the codex
                // daemon.
                // Issue #251 — the wave title is the user's prompt /
                // goal for the spec agent. Stamping it as the codex
                // card's `payload.prompt` does two things:
                //   1. surfaces it to `codex_auto_submit`, which gates
                //      its `\r` injection on a non-empty payload prompt
                //      (so the composer-pre-filled goal is submitted
                //      the moment the codex TUI is ready); and
                //   2. gives `seed_and_spawn_spec_daemon` a stable
                //      input to append as codex's positional `[PROMPT]`
                //      arg so the TUI mounts with the goal already in
                //      the composer (the same hands-free shape plain
                //      codex cards use).
                // The system prompt itself still lives in
                // `$CODEX_HOME/config.toml`'s `instructions` field
                // (seeded by `seed_codex_home_for_card`); the prompt
                // arg is the user-facing goal that the spec agent's
                // loop ("Read the wave's goal…") reads.
                let spec_prompt = wave.title.trim().to_string();
                let spec_prompt_for_tx = if spec_prompt.is_empty() {
                    None
                } else {
                    Some(spec_prompt.clone())
                };
                let (spec_card, _term, mcp_token) = card_with_codex_create_tx(
                    tx,
                    spec_card_id_for_tx.clone(),
                    wave_id.clone(),
                    None,               // sort: append to end
                    cwd_for_tx,         // codex's cwd
                    env_for_tx,         // terminal env
                    spec_prompt_for_tx, // prompt = wave title (#251)
                    CardRole::Spec,     // <— the PR6 binding
                    // Issue #229 PR A — the spec card is kernel-owned.
                    // Migration 0013 already backfilled `deletable = 0`
                    // for legacy spec rows; new spec cards minted here
                    // get the same treatment so `DELETE /api/cards/:id`
                    // and `neige.card.delete` refuse to drop them.
                    // Wave delete still cascades via the FK chain.
                    false,
                    &cache_for_tx,
                )
                .await?;

                // 3c. Issue #229 PR B — mint the wave-report card in
                // the same tx. Kernel-owned (`deletable = false`), role
                // = `ReportCard`, payload = the v1 initial shape from
                // `WaveReportPayload::initial()`. The placeholder body
                // (`"# Goal\n\n_The spec agent will fill this in._\n"`)
                // mirrors the literal string in migration 0014's
                // backfill INSERT — both paths land an identical row so
                // a wave never observes a "no report card" state, and
                // freshly-minted waves render the same placeholder as
                // legacy ones until the spec agent rewrites the body.
                // `sort: -1.0` puts the report card ahead of every
                // user/dispatcher card in list-mode order (existing
                // sorts are non-negative; `next_sort_scoped_in_tx`
                // never mints negative values).
                let report_payload =
                    serde_json::to_value(WaveReportPayload::initial()).map_err(|e| {
                        CalmError::Internal(format!(
                            "wave_create: serialize wave-report payload: {e}"
                        ))
                    })?;
                let report_new = NewCard {
                    wave_id: wave_id.clone(),
                    kind: "wave-report".to_string(),
                    sort: Some(-1.0),
                    payload: report_payload,
                };
                let report_card = card_create_with_id_tx(
                    tx,
                    report_card_id_for_tx.clone(),
                    report_new,
                    CardRole::ReportCard,
                    // Kernel-owned: refuses REST / plugin-callback
                    // delete (the parent wave's delete still cascades
                    // via FK).
                    false,
                    &cache_for_tx,
                )
                .await?;

                // 3d. Seed the layout overlay so the WaveGrid renders
                // a side-by-side layout: spec agent on the left half
                // (x=0, w=6) and wave report on the right half (x=6,
                // w=6), both full height (h=12). Stamping both
                // positions explicitly here means a user who never
                // opens the wave still gets the canonical two-column
                // layout from their first view.
                //
                // `plugin_id = "kernel"`, `entity_kind = "view"`,
                // `entity_id = wave_id`, `kind = "layout"` — same
                // tuple `useOverlayState({entity_kind: 'view',
                // kind: 'layout'})` reads/writes from the frontend.
                let layout_payload = serde_json::json!({
                    "schemaVersion": 1,
                    "positions": {
                        spec_card_id_for_tx.as_str(): {
                            "x": 0, "y": 0, "w": 6, "h": 12
                        },
                        report_card_id_for_tx.as_str(): {
                            "x": 6, "y": 0, "w": 6, "h": 12
                        }
                    }
                });
                let layout_overlay = overlay_upsert_tx(
                    tx,
                    NewOverlay {
                        plugin_id: "kernel".to_string(),
                        entity_kind: "view".to_string(),
                        entity_id: wave_id.as_str().to_string(),
                        kind: "layout".to_string(),
                        payload: layout_payload,
                    },
                )
                .await?;

                // 3e. Per-event scopes — we now have the real ids.
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
                let events = vec![
                    (wave_scope.clone(), Event::WaveUpdated(wave.clone())),
                    (spec_card_scope, Event::CardAdded(spec_card)),
                    (report_card_scope, Event::CardAdded(report_card)),
                    // Layout overlay broadcasts so any open WaveGrid
                    // subscriber on this wave picks up the seeded
                    // positions without an extra GET round-trip.
                    (wave_scope, Event::OverlaySet(layout_overlay)),
                ];
                Ok(((wave, mcp_token), events))
            })
        },
    )
    .await?;

    // 4. Post-commit: seed the spec card's `$CODEX_HOME` + spawn its
    //    codex daemon **inline** before returning 201. Issue #236:
    //    handing this off via `tokio::spawn` opened a TOCTOU race
    //    where the frontend could open the spec card's WS in the
    //    window between commit and the background task running,
    //    observe `daemon_handle = None` in
    //    `ws::terminal::resolve_live_sock`, and trigger that handler's
    //    respawn branch using the terminal row's baked env — which
    //    omits `NEIGE_MCP_SOCKET` / `NEIGE_MCP_TOKEN`. Two daemons
    //    would then race on the same `--sock` path and the WS would
    //    attach to the no-MCP one. Awaiting the spawn inline closes
    //    the race by guaranteeing `daemon_handle` is `Some` by the
    //    time the 201 reaches the client.
    //
    //    PR7a (#136) — fold the freshly minted per-card MCP token +
    //    kernel socket path into the env handed to the codex daemon
    //    spawn. The shim_config lives on `AppState::mcp_server`; when
    //    both the token and the shim are present we add
    //    `NEIGE_MCP_TOKEN` + `NEIGE_MCP_SOCKET` so the codex daemon
    //    forwards them to `neige-mcp-stdio-shim` per its
    //    `[mcp_servers.calm].env` block for the handshake step.
    //    Missing either side is a soft-fail: the daemon still starts,
    //    but the spec agent can't reach the kernel-as-MCP-server.
    //
    //    NOTE: this env augmentation is **not** persisted to the
    //    terminal row (only the pre-MCP env was written inside the
    //    tx; we don't have a hash→raw lookup for `card_mcp_tokens`
    //    today — see `mcp_server::auth::hash_token`). A long-tail
    //    daemon death + WS revive will hit the buggy "respawn from
    //    baked env" branch in `ws::terminal::resolve_live_sock`
    //    today; that branch now emits a defensive warn log when
    //    MCP env is absent on a Spec card. The proper revive-side
    //    fix (re-mint token + update hash) is deferred to an issue
    //    #236 follow-up.
    let mut env_for_spawn = env;
    if let (Some(token), Some(server)) = (mcp_token.as_deref(), s.mcp_server.as_ref())
        && let Some(map) = env_for_spawn.as_object_mut()
    {
        map.insert(
            "NEIGE_MCP_TOKEN".into(),
            serde_json::Value::String(token.to_string()),
        );
        map.insert(
            "NEIGE_MCP_SOCKET".into(),
            serde_json::Value::String(server.shim_config.socket_path.to_string_lossy().to_string()),
        );
    }

    // Issue #236: synchronous spawn. On failure return 5xx; persisted
    // rows + already-broadcast events stay (no rollback). The wave is
    // recoverable out-of-band; rolling back would silently discard
    // the user's typed title which is worse UX than a retriable error.
    // #236 followup — thread `mcp_token` through to the seed step so
    // it lands in the per-card config.toml's `[mcp_servers.calm].env`
    // block. The pre-followup code passed the token only via the
    // daemon spawn env (above) on the assumption codex would forward
    // it to the shim subprocess; codex CLI 0.132 doesn't, so the shim
    // exited with `missing NEIGE_MCP_SOCKET`. The daemon-env path
    // stays as defense-in-depth.
    // Issue #251 — thread the wave title through as codex's positional
    // `[PROMPT]` arg. The same value lives in the spec card's
    // `payload.prompt` (stamped inside the tx above) so the
    // `codex_auto_submit` subscriber's gate (non-empty payload.prompt
    // → inject `\r`) fires for spec cards just like it does for plain
    // hands-free cards. Together: composer mounts pre-filled with the
    // goal, `\r` is injected on `hook.codex.session_start`, spec
    // agent's loop starts.
    // Issue #250 PR 2 — the spec daemon now spawns in `wave.cwd`,
    // not `default_cwd()`. The committed wave's `cwd` is the
    // authoritative source even though the route had its own
    // `normalized_cwd` in scope: routing through `wave.cwd` keeps the
    // contract narrow ("whatever ended up on the row is what the
    // daemon runs under") and matches the same path the spec card's
    // terminal-row write recorded.
    if let Err(e) = seed_and_spawn_spec_daemon(
        s.clone(),
        spec_card_id.clone(),
        wave.id.as_str().to_string(),
        wave.cwd.clone(),
        env_for_spawn,
        mcp_token.clone(),
        Some(wave.title.clone()),
    )
    .await
    {
        tracing::error!(
            card_id = %spec_card_id,
            wave_id = %wave.id,
            error = %e,
            "spec daemon spawn failed on wave create; wave row persisted, terminal will be reaped by sweeper",
        );
        return Err(CalmError::Internal(format!(
            "spec daemon spawn failed: {e}",
        )));
    }

    tracing::info!(
        card_id = %spec_card_id,
        wave_id = %wave.id,
        "spec card persisted + daemon spawned inline",
    );
    Ok((StatusCode::CREATED, Json(wave)).into_response())
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
    State(s): State<AppState>,
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
    let patch_has_other_changes = p.title.is_some() || p.sort.is_some() || p.archived_at.is_some();
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
        &s.card_role_cache,
        &s.wave_cove_cache,
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
                        },
                    ));
                }
                events.push((scope, Event::WaveUpdated(wave.clone())));
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
pub(crate) async fn delete_wave(
    State(s): State<AppState>,
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
    // Reading outside the txn is fine — there's no concurrent write
    // that could change `wave.cove_id` or grow the card list under us
    // (the FK locks established by the wave-delete txn would serialize
    // any racing create against the same wave). Worst case a racing
    // `POST /api/cards` lands a new card on the wave between our read
    // and the write — that card won't have a terminal row yet (the
    // 3-step create takes another HTTP round trip), so the wave-delete
    // FK cascade handles it.
    let wave = s
        .repo
        .wave_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;
    let cove_id = wave.cove_id.clone();
    let wave_id = wave.id.clone();

    let cards = s.repo.cards_by_wave(wave_id.as_str()).await?;
    let mut terminal_ids: Vec<String> = Vec::new();
    for card in &cards {
        if let Some(t) = s.repo.terminal_get_by_card(card.id.as_str()).await? {
            reap_terminal_artifacts(&t).await;
            terminal_ids.push(t.id);
        }
    }

    let scope = EventScope::Wave {
        wave: wave_id.clone(),
        cove: cove_id.clone(),
    };
    let wcc_for_tx = s.wave_cove_cache.clone();
    let (_unit, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.card_role_cache,
        &s.wave_cove_cache,
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
                wave_delete_tx(tx, wave_id.as_ref(), &wcc_for_tx).await?;
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
