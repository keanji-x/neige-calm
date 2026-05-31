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
//! the WS terminal revive path in `ws::terminal::resolve_live_renderer`:
//! the frontend would open the spec card's WS in the ~400 ms window
//! between commit and the background task running, see
//! `renderer entry = None`, and respawn the daemon from the **baked
//! terminal-row env**, which is missing `NEIGE_MCP_SOCKET` /
//! `NEIGE_MCP_TOKEN` (those vars are folded in only on the post-commit
//! env clone, never persisted to the terminal row). Two daemons would
//! then race on the same `--sock` and the WS would attach to the
//! no-MCP one, breaking the codex MCP handshake.
//!
//! The fix awaits the seed + spawn inline before returning 201, so the
//! response payload never reflects a daemon-less spec card when the spawn
//! succeeds. Spawning the spec agent is **non-fatal** (issue #293 / PR
//! #311): if the codex app-server can't boot (missing/broken binary, auth
//! hiccup, or the overall boot deadline) the handler still returns 201
//! with the created wave — the spec card just has no `codex_thread_id`
//! and no running daemon (inert / not-running, recoverable by retry or
//! delete). The DB tx is never rolled back: the wave row stays so the
//! user keeps their typed title. Persisted rows + the two events survive;
//! the orphan-terminal sweeper reaps the dangling terminal row (~60 s
//! grace) if no daemon is attached.
//!
//! The earlier rationale for the `tokio::spawn` form was the old
//! readiness wait in `spawn_terminal_for`. That latency affected one
//! specific test path (`web/e2e/a11y-keyboard.spec.ts`'s 5 s navigation
//! budget when running without a real codex). The tradeoff was wrong:
//! the WS race is a correctness bug for every production user, the
//! a11y timeout is a CI-only ergonomic concern. The a11y test stack is
//! expected to carry a navigation budget that accommodates synchronous
//! daemon spawn failure.
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
    card_create_with_id_tx, card_update_tx, card_with_codex_create_tx, cove_folder_create_tx,
    overlay_upsert_tx, terminal_delete_tx, wave_create_tx, wave_delete_tx, wave_update_tx,
};
use crate::db::{write_with_event_typed, write_with_events_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{EditAuthor, Event, EventScope};
use crate::ids::ActorId;
use crate::model::{
    CardPatch, CardRole, CoveKind, FolderConflict, FolderConflictKind, NewCard, NewOverlay,
    NewWave, Wave, WaveDetail, WavePatch, new_id,
};
use crate::routes::cove_folders::{is_descendant_of, normalize_path};
use crate::routes::settings::{Settings, load_settings};
use crate::spec_appserver::{
    TurnWatchdogConfig, spawn_spec_appserver_with_watchdog_config_and_recovery_for_wave,
};
use crate::spec_card::{SpecPushDaemonArgs, build_codex_env_map, seed_and_spawn_spec_daemon};
use crate::state::AppState;
use crate::terminal_sweeper::{reap_spec_push, reap_terminal_artifacts};
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
    // #177 — capture the host browser's theme RGB BEFORE `p` is moved
    // into `wave_create_tx`. The value lands on the spec card's
    // terminal row inside the tx; the synchronous spec-card spawn
    // below reads it back from that row via `spawn_terminal_for`, so
    // the daemon argv and the row stay agreement-by-construction.
    let theme_for_tx = p.theme;
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
                // The system prompt itself is sent through the app-server
                // `thread/start` call; the prompt arg is the user-facing
                // goal that the spec agent's loop ("Read the wave's goal…")
                // reads.
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
                    None,               // icon_bg: default frontend logo color
                    None,               // icon_fg: default frontend logo color
                    CardRole::Spec,     // <— the PR6 binding
                    // Issue #229 PR A — the spec card is kernel-owned.
                    // Migration 0013 already backfilled `deletable = 0`
                    // for legacy spec rows; new spec cards minted here
                    // get the same treatment so `DELETE /api/cards/:id`
                    // and `neige.card.delete` refuse to drop them.
                    // Wave delete still cascades via the FK chain.
                    false,
                    &cache_for_tx,
                    // #177 — host browser's theme RGB taken from the
                    // wave-create request body (required on `NewWave`).
                    // Persisted onto the spec card's terminal row so
                    // the codex daemon's argv carries `--terminal-fg/
                    // -bg` even on the first boot (closing the bug
                    // where a cold-mounted spec card painted in the
                    // daemon's default colors).
                    theme_for_tx,
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
    //    observe `renderer entry = None` in
    //    `ws::terminal::resolve_live_renderer`, and trigger that handler's
    //    respawn branch using the terminal row's baked env — which
    //    omits `NEIGE_MCP_SOCKET` / `NEIGE_MCP_TOKEN`. Two daemons
    //    would then race on the same `--sock` path and the WS would
    //    attach to the no-MCP one. Awaiting the spawn inline closes
    //    the race by guaranteeing `renderer entry` is `Some` by the
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
    //    baked env" branch in `ws::terminal::resolve_live_renderer`
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
    //
    // #293/#419 cutover — push is the ONLY path. Non-empty waves drive
    // DECISION A's blocking sequence: boot the kernel-owned `codex
    // app-server`, run turn #1, await its initial lifecycle notification,
    // persist `codex_thread_id` + `appserver_sock`, park the handle, then
    // spawn the PTY daemon in resume mode. Empty-title waves boot only the
    // app-server, persist runtime fields without `codex_thread_id`, park a
    // pending handle, and spawn `codex --remote <sock>` so the TUI
    // fresh-starts the thread. There is no legacy bare-`codex '<title>'`
    // path anymore.
    //
    // S2 (#293, #311) — SPEC BOOT IS NON-FATAL TO WAVE CREATION.
    // The wave + spec card + report card rows are already committed (and
    // their `CardAdded`/`WaveUpdated` events already broadcast) by the time
    // we boot the app-server here. The app-server boot must therefore be
    // NON-FATAL: if it fails — a missing/broken codex binary (every
    // codex-free environment: CI's web a11y job, the chromium docker stack),
    // a transient model/auth hiccup, or the S1 layer-3 init/boot wedge
    // backstop firing across socket connect, WS handshake, initialize,
    // turn setup, or the initial lifecycle wait — we DO NOT return 500.
    // `spawn_push_appserver`'s internal `SpawnRollback` guard has already
    // torn down the failed app-server process group + socket dir (no
    // orphan), so on the error arm we simply `warn!` that the spec agent
    // couldn't start, SKIP the
    // `codex_thread_id` persist + registry insert + `--remote` TUI spawn
    // (all of which live in `spawn_push_appserver` / the
    // `seed_and_spawn_spec_daemon` call below), and return **201 with the
    // created wave**. The wave/spec-card/report/terminal rows already
    // committed stay; the spec card simply has no `codex_thread_id` and no
    // running daemon (inert / not-running, recoverable by retry or delete).
    // The dispatcher's missing-handle path already warns (no crash), so an
    // inert wave is safe. Pre-cutover the PTY path tolerated codex failing
    // (it only 500'd if the daemon BINARY was missing); this restores that
    // tolerance for the push path so codex-free UI jobs get a 201.
    let push_args = match spawn_push_appserver(
        &s,
        &spec_card_id,
        &wave,
        &env_for_spawn,
        &settings,
        mcp_token.as_deref(),
    )
    .await
    {
        Ok(args) => Some(args),
        Err(e) => {
            // Non-fatal: the app-server's `SpawnRollback` guard already
            // reaped its process group + socket dir on the way out (no
            // orphan). Log the wave as created-but-inert and fall through
            // to return 201 with the wave; we skip the daemon spawn below.
            tracing::warn!(
                card_id = %spec_card_id,
                wave_id = %wave.id,
                error = %e,
                "spec push app-server failed to boot on wave create; wave created but the spec agent is NOT running (inert wave, recoverable via retry/delete) — returning 201",
            );
            None
        }
    };

    // Only spawn the `--remote` TUI daemon when the app-server actually
    // booted (non-empty: turn #1 started and thread persisted; empty:
    // initialized and pending TUI fresh-start handle parked).
    // If the boot failed above, `push_args` is `None` and the wave is inert
    // — there is no socket to attach to, so we skip the daemon spawn entirely
    // and return the created wave.
    if let Some(push_args) = push_args {
        if let Err(e) = seed_and_spawn_spec_daemon(
            s.clone(),
            spec_card_id.clone(),
            wave.id.as_str().to_string(),
            wave.cwd.clone(),
            env_for_spawn,
            mcp_token.clone(),
            push_args,
        )
        .await
        {
            // Non-fatal, mirroring the app-server boot path: the wave +
            // rows are committed and the app-server already booted (its
            // handle is parked in `state.spec_push`). A failed PTY daemon
            // spawn leaves the wave inert/not-running rather than 500'ing;
            // the sweeper reaps the orphan terminal.
            tracing::warn!(
                card_id = %spec_card_id,
                wave_id = %wave.id,
                error = %e,
                "spec daemon spawn failed on wave create; wave created but the spec TUI daemon is NOT running (inert wave) — returning 201",
            );
        } else {
            tracing::info!(
                card_id = %spec_card_id,
                wave_id = %wave.id,
                "spec card persisted + daemon spawned inline",
            );
        }
    } else {
        tracing::info!(
            card_id = %spec_card_id,
            wave_id = %wave.id,
            "wave created without a running spec agent (app-server boot failed; inert wave)",
        );
    }

    Ok((StatusCode::CREATED, Json(wave)).into_response())
}

/// PR3a (#293) — DECISION A's create-wave blocking sequence for the push
/// path. Boots the kernel-owned `codex app-server`. Non-empty titles start
/// a codex thread, run turn #1 with that title, and await `turn/started` so
/// a rollout exists on disk for the `--remote` TUI to resume. Empty titles
/// skip `thread/start`; the TUI fresh-starts and the notification consumer
/// backfills `codex_thread_id` from the first turn lifecycle. The function
/// persists app-server runtime fields on the spec card payload (eventized —
/// same audited `write_with_event_typed` path every other write uses), parks
/// the handle in `state.spec_push`, and returns the [`SpecPushDaemonArgs`]
/// the PTY daemon spawn needs.
///
/// On any failure the `SpawnRollback` guard in `spawn_spec_appserver`
/// reaps the failed app-server's process group + socket dir, and this
/// function's `?` propagates the error. The caller (`create_wave`) treats
/// that error as **non-fatal** (issue #293 / PR #311): it logs a warning,
/// skips the daemon spawn, and returns 201 with an inert wave — the
/// persisted rows survive and the sweeper reaps the orphan terminal.
async fn spawn_push_appserver(
    s: &AppState,
    spec_card_id: &str,
    wave: &Wave,
    env_for_spawn: &serde_json::Value,
    settings: &Settings,
    mcp_token: Option<&str>,
) -> Result<SpecPushDaemonArgs> {
    // Seed the spec card's `$CODEX_HOME` FIRST. The kernel-owned
    // `app-server` is spawned with `CODEX_HOME = <codex_homes_dir>/<card_id>`
    // (in `env_for_spawn`); codex exits at boot with "CODEX_HOME … does not
    // exist" if the dir (with its seeded auth.json + config.toml) isn't
    // there yet. The legacy path seeds inside `seed_and_spawn_spec_daemon`,
    // but in push mode the app-server boots BEFORE that call, so we seed
    // here. The later `seed_and_spawn_spec_daemon` re-seed is idempotent
    // (the copy is skipped on the now-existing dir; config.toml is
    // rewritten identically).
    crate::spec_card::seed_codex_home_for_card(
        s,
        spec_card_id,
        &wave.cwd,
        wave.id.as_str(),
        crate::spec_card::SeededCardRole::Spec,
        mcp_token,
    )?;

    // Per-card listen socket under the user-owned data dir (the server
    // 0700-chmods the socket's parent dir; `appserver_sock_dir` resolves
    // it to `<data_dir>/appserver/<card_id>/`, never the shared root).
    // Create the dir now so `spawn_spec_appserver` can bind into it.
    let sock = s.daemon.appserver_sock_path(spec_card_id);
    let sock_dir = s.daemon.appserver_sock_dir(spec_card_id);
    std::fs::create_dir_all(&sock_dir).map_err(|e| {
        CalmError::Internal(format!(
            "mkdir appserver sock dir {}: {e}",
            sock_dir.display()
        ))
    })?;

    // DECISION A's boot sequence for non-empty goals remains unchanged
    // (boot → connect → initialize → thread/start → turn/start(goal) →
    // initial lifecycle wait). Empty goals intentionally stop after
    // initialize; the remote TUI fresh-starts the thread and this handle
    // observes the first turn lifecycle before activating push delivery.
    let recovery_signal =
        crate::wire_spec_push_recovery_supervisor(s, settings, spec_card_id, wave.id.clone());
    let developer_instructions = crate::spec_card::render_system_prompt(
        crate::spec_card::SeededCardRole::Spec.prompt_template(),
        wave.id.as_str(),
    );
    let handle = spawn_spec_appserver_with_watchdog_config_and_recovery_for_wave(
        &s.codex.codex_bin,
        env_for_spawn,
        &wave.title,
        &sock,
        Some(&developer_instructions),
        TurnWatchdogConfig::default(),
        Some(recovery_signal),
        Some(&wave.id),
    )
    .await?;
    let thread_id = handle.thread_id.clone();
    let sock_for_args = handle.sock.clone();
    let pgid = handle.pgid;
    // #318 INV-5 (R3-B1) — identity stamp captured by
    // `spawn_spec_appserver` from `/proc/<pgid>/stat` field 22 (start_time)
    // and `/proc/sys/kernel/random/boot_id` (boot_id) at spawn. Both
    // persisted alongside `appserver_pgid` so boot-recovery can verify
    // the pid hasn't been recycled (same-boot stamp mismatch OR
    // cross-reboot boot_id mismatch) before signaling it. `None` on
    // non-Linux / ENOENT-at-spawn → field is JSON null below; boot-
    // recovery treats an absent stamp as "skip the kill" (conservative).
    let start_time = handle.start_time;
    let boot_id = handle.boot_id.clone();
    let needs_initial_prompt = wave.title.trim().is_empty();

    // Persist app-server runtime fields on
    // the spec card payload (merge into the existing payload —
    // `card_update_tx` replaces the whole `payload` column, so we
    // read-modify-write). For non-empty goals, `codex_thread_id` is also
    // the `codex_auto_submit` skip signal: the kernel already drove turn
    // #1, so no `\r` is injected into the resumed TUI. Empty goals do NOT
    // write `codex_thread_id` yet; the notification consumer backfills the
    // TUI-created id after the first `turn/started`. `appserver_pgid` is the
    // crash-recovery reuse target — `takeover_spec_appservers_on_boot`
    // reads it to `kill(-pgid, …)` a leaked group after a hard crash.
    //
    // NOTE: this persists AFTER the handle is built but BEFORE it is parked
    // in the registry. If this write fails (`?`), `handle` is dropped on
    // the early return → its `Drop` fires a group SIGTERM (B1), so the
    // freshly-booted app-server is reaped rather than leaked, and nothing
    // is parked in the registry. The socket dir is left for the sweeper /
    // next boot recovery (the pgid was not persisted, so boot-recovery
    // can't see it; but the Drop already killed the group, so there is no
    // live server to leak — only an empty stale socket file, harmless).
    // The wave is in hand, so the card scope needs no extra DB round-trip.
    let scope = EventScope::Card {
        card: spec_card_id.into(),
        wave: wave.id.clone(),
        cove: wave.cove_id.clone(),
    };
    let card_id_for_tx = spec_card_id.to_string();
    let thread_id_for_tx = if needs_initial_prompt {
        None
    } else {
        Some(thread_id.clone())
    };
    let sock_str = sock_for_args.to_string_lossy().to_string();
    let (_card, _id) = write_with_event_typed(
        s.repo.as_ref(),
        ActorId::Kernel,
        scope,
        None,
        &s.events,
        &s.card_role_cache,
        &s.wave_cove_cache,
        move |tx| {
            Box::pin(async move {
                // Read current payload, merge the push fields, write back.
                // N1: a spec card's payload must be a JSON object — the
                // push fields can only be inserted into one. If it isn't
                // (corruption / unexpected shape), fail loudly rather than
                // silently dropping `codex_thread_id` (which would hand the
                // `--remote` TUI a `codex resume` argv with no persisted
                // thread id and leave the booted app-server parked but
                // unreferenceable).
                let mut payload = s_repo_card_get(tx, &card_id_for_tx).await?;
                let Some(map) = payload.as_object_mut() else {
                    return Err(CalmError::Internal(format!(
                        "spec card {card_id_for_tx} payload is not a JSON object; \
                         cannot persist codex_thread_id/appserver_sock/appserver_pgid",
                    )));
                };
                if let Some(thread_id) = thread_id_for_tx {
                    map.insert(
                        "codex_thread_id".into(),
                        serde_json::Value::String(thread_id),
                    );
                } else {
                    map.remove("codex_thread_id");
                }
                if needs_initial_prompt {
                    map.insert(
                        "appserver_needs_initial_prompt".into(),
                        serde_json::Value::Bool(true),
                    );
                } else {
                    map.remove("appserver_needs_initial_prompt");
                }
                map.insert("appserver_sock".into(), serde_json::Value::String(sock_str));
                map.insert(
                    "appserver_pgid".into(),
                    serde_json::Value::Number(pgid.into()),
                );
                // #318 INV-5 (R3-B1) — pid identity stamp. Persist as a
                // JSON number when available; on non-Linux / `/proc`
                // read failure (`start_time == None`) we EXPLICITLY
                // write JSON null so a future boot reads it as absent
                // and conservatively skips the kill rather than
                // matching against a missing key with undefined
                // semantics. `serde_json::Number::from(u64)` is
                // infallible for u64 (JSON numbers losslessly hold u64).
                map.insert(
                    "appserver_start_time".into(),
                    start_time
                        .map(|v| serde_json::Value::Number(serde_json::Number::from(v)))
                        .unwrap_or(serde_json::Value::Null),
                );
                // #318 INV-5 (R3-B1) — kernel boot UUID companion. The
                // start_time stamp is jiffies-since-boot, so a reboot
                // resets it; the boot_id distinguishes "same kernel,
                // pid recycled" from "host rebooted entirely". Same
                // explicit-null posture on absence (conservative
                // skip-the-kill).
                map.insert(
                    "appserver_boot_id".into(),
                    boot_id
                        .map(serde_json::Value::String)
                        .unwrap_or(serde_json::Value::Null),
                );
                // #313 problem #1 — initialize the persisted push watermark
                // to 0 (the "no events pushed yet" sentinel). The dispatcher
                // bumps this field atomically alongside `push_cursor.bump`
                // every time it pushes (see `dispatcher::push_to_spec`); on
                // boot recovery, [`crate::takeover_spec_appservers_on_boot`]
                // seeds the in-memory `EventCursorCache` from this field and
                // replays every event with `id > watermark` so a kernel
                // restart can't silently drop already-acked-by-codex catch-up
                // events. 0 is correct on first persist because no push has
                // happened yet — the very first push will bump it to the
                // envelope's id.
                map.insert(
                    "push_watermark".into(),
                    serde_json::Value::Number(0i64.into()),
                );
                let card = card_update_tx(
                    tx,
                    &card_id_for_tx,
                    CardPatch {
                        kind: None,
                        sort: None,
                        payload: Some(payload),
                        deletable: None,
                    },
                )
                .await?;
                Ok((card.clone(), Event::CardUpdated(card)))
            })
        },
    )
    .await?;

    // #313 problem #1 round-3 (B2) — install the watermark sink on the
    // freshly-created handle BEFORE parking it in the registry. The
    // round-2 boot-takeover path (`lib.rs::register_and_catch_up`) wired
    // this for resumed handles, but the symmetric create-wave path was
    // missed: a push landing while the freshly-created spec turn is
    // active would hit `Enqueue` (the spec is mid-turn-1, the
    // dispatcher's `Inner::push_to_spec` cannot issue a second
    // `turn/start`), the consumer's later `flush_push_queue` would
    // deliver the queued items, and — with no sink installed — the
    // durable `push_watermark` would NEVER advance for those flushed
    // ids. A subsequent restart would replay them all (boot catch-up
    // uses `id > watermark` strictly), causing the spec thread to see
    // already-delivered events twice.
    //
    // `Dispatcher::watermark_sink_for` is the single source of truth for
    // the sink closure shape (it captures repo + the push cursor cache
    // for in-process dedup symmetry); both this site and
    // `register_and_catch_up` go through it.
    let card_key: crate::ids::CardId = spec_card_id.to_string().into();
    let sink = s.dispatcher.watermark_sink_for(card_key.clone());
    handle.install_watermark_sink(sink).await;
    debug_assert!(
        handle.has_watermark_sink().await,
        "spawn_push_appserver: install_watermark_sink did not take effect — \
         a future refactor split the install from the assert; queued-then-\
         flushed envelopes would silently fail to persist their watermark"
    );
    if needs_initial_prompt {
        let initial_prompt_ready = s.dispatcher.initial_prompt_ready_sink_for(
            card_key.clone(),
            wave.id.clone(),
            wave.cove_id.clone(),
        );
        handle
            .install_initial_prompt_ready_sink(initial_prompt_ready)
            .await;
    }

    // #318 INV-3 (R2-B1) — install the durable queue-persist callbacks
    // BEFORE parking the handle in the registry, symmetric with the
    // watermark sink above. A push landing immediately after registration
    // hits `SpecPusher::push_observation`, whose `Enqueue` arm persists to
    // `spec_push_queue` BEFORE the in-memory `push_back` — so a kernel
    // crash between persist and the consumer task's flush leaves a
    // recoverable row that boot-takeover's `rehydrate_queue_from_persist`
    // re-delivers on the next process.
    //
    // The sister install lives in `lib.rs::register_and_catch_up` for the
    // boot-takeover path (INV-6 — symmetric startup hooks across both
    // entry points; #318).
    let persist = s.dispatcher.queue_persist_for(card_key);
    handle.install_queue_persist(persist).await;
    debug_assert!(
        handle.has_queue_persist().await,
        "spawn_push_appserver: install_queue_persist did not take effect — \
         enqueued-but-not-yet-flushed observations would not be durable, \
         silently re-introducing the INV-3 (#318) regression"
    );

    // Park the handle so PR3b's dispatcher can resolve the wave's
    // app-server client + thread, and so wave-delete / sweeper teardown
    // kills the child via `kill_on_drop`.
    //
    // #322 — `park` (not the bare `insert`) runs the aspect framework's
    // `BeforeHandleParkInRegistry` checks first; INV-6
    // (`WatermarkSinkInstalledAspect`) panics in release if a future
    // refactor drops the `install_watermark_sink` call above. The
    // `debug_assert!` above is the local fast-fail at the install site;
    // the aspect is the framework-level enforcement at the park site
    // (belt + suspenders, both pointing at INV-6).
    s.spec_push
        .park(wave.id.clone(), handle, s.aspects.as_ref())
        .await;

    let thread_id_log = if thread_id.is_empty() {
        "<pending>"
    } else {
        thread_id.as_str()
    };
    tracing::info!(
        card_id = %spec_card_id,
        wave_id = %wave.id,
        thread_id = %thread_id_log,
        sock = %sock_for_args.display(),
        needs_initial_prompt,
        "spec push: app-server booted, runtime fields persisted",
    );

    Ok(SpecPushDaemonArgs {
        thread_id: if needs_initial_prompt {
            None
        } else {
            Some(thread_id)
        },
        sock: sock_for_args,
    })
}

/// Fetch a card's current `payload` JSON inside a transaction (read step
/// of the read-modify-write payload merge in [`spawn_push_appserver`]).
async fn s_repo_card_get(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card_id: &str,
) -> Result<serde_json::Value> {
    let row: Option<(String,)> = sqlx::query_as("SELECT payload FROM cards WHERE id = ?1")
        .bind(card_id)
        .fetch_optional(&mut **tx)
        .await?;
    let payload_text = row
        .ok_or_else(|| CalmError::NotFound(format!("card {card_id}")))?
        .0;
    // N1: surface a malformed payload as an explicit error rather than
    // masking it as `Value::Null` (the prior `unwrap_or(Null)`), which
    // would then silently drop the merged push fields downstream. The
    // caller additionally rejects a well-formed-but-non-object payload.
    serde_json::from_str(&payload_text)
        .map_err(|e| CalmError::Internal(format!("card {card_id} payload is not valid JSON: {e}")))
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

    // PR3a (#293) — eager teardown of the wave's spec-push app-server
    // handle (if any): kills the kernel-owned `codex app-server` *process
    // group* (SIGTERM→SIGKILL, reaping both the node launcher and the
    // native child) and removes the listen socket + per-card dir. No-op
    // when the flag is off or no handle exists. Done alongside the
    // PTY-daemon reaping below so both processes are torn down before the
    // rows drop.
    reap_spec_push(&s, &wave_id).await;

    let cards = s.repo.cards_by_wave(wave_id.as_str()).await?;
    let mut terminal_ids: Vec<String> = Vec::new();
    for card in &cards {
        if let Some(t) = s.repo.terminal_get_by_card(card.id.as_str()).await? {
            reap_terminal_artifacts(&s, &t).await;
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
    State(s): State<AppState>,
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
        &s.card_role_cache,
        &s.wave_cove_cache,
        ActorId::User,
        EditAuthor::User,
        wave,
        report_card,
        current_payload,
        next,
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
