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
//! [`Event::CardAdded`] (scope = Card). The spec card's backing
//! `$CODEX_HOME` seed + daemon spawn happen **off the response hot
//! path** — they're scheduled through [`tokio::spawn`] so the
//! handler returns 201 the instant the tx commits, regardless of how
//! long the daemon takes to come up (or fail to). All failures inside
//! the background task log at `warn!` and are swallowed; the orphan-
//! terminal sweeper reaps any dangling terminal row (~60s grace; see
//! PR7+ for spec card cleanup).
//!
//! Why two iterations: the first PR6 fix downgraded daemon-spawn
//! failure from 500 → warn + 201 but kept the `spawn_daemon_for`
//! await on the hot path. In CI (no `codex` binary) the busy-poll
//! wait-until-socket-ready loop inside `spawn_daemon_for` held the
//! response open for ~3s, which combined with the front-end's create
//! → wait-for-201 → router-navigate sequence blew past the web a11y
//! test's 5s navigation budget (`web/e2e/a11y-keyboard.spec.ts`).
//! The fix here moves the entire seed + spawn pipeline behind
//! `tokio::spawn`, restoring the "201 returns on commit" contract.
//!
//! The wave-delete path does **not** yet cascade-clean the spec card's
//! daemon — see TODO in [`delete_wave`].

use crate::actor::Actor;
use crate::db::sqlite::{
    card_with_codex_create_tx, wave_create_tx, wave_delete_tx, wave_update_tx,
};
use crate::db::{write_with_event_typed, write_with_events_typed};
use crate::error::{CalmError, ErrorBody, Result};
use crate::event::{Event, EventScope};
use crate::model::{CardRole, NewWave, Wave, WaveDetail, WavePatch, new_id};
use crate::routes::settings::load_settings;
use crate::spec_card::{build_codex_env_map, seed_and_spawn_spec_daemon};
use crate::state::AppState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/waves", axum::routing::post(create_wave))
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
    let detail = s
        .repo
        .wave_detail(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;
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
    Json(p): Json<NewWave>,
) -> Result<(StatusCode, Json<Wave>)> {
    // PR6 (#136) — wave create now atomically mints a `CardRole::Spec`
    // codex card alongside the wave row. Both rows commit in one tx
    // and both `Event::WaveUpdated` + `Event::CardAdded` envelopes
    // emit from the same commit, each tagged with its own scope so
    // per-wave and per-card subscribers each see the relevant frame
    // without re-routing through ancestors.

    tracing::info!(
        debug = "pr182",
        phase = "entered",
        cove_id = %p.cove_id,
        "waves::create_wave entered"
    );

    // 1. Pre-mint the spec card id BEFORE the tx opens — we need it to
    //    derive `CODEX_HOME = <codex_homes_dir>/<card_id>/` for the
    //    env map we hand the daemon spawn post-commit. The wave id is
    //    minted inside `wave_create_tx` (PR2 stopgap precedent in
    //    `routes/coves.rs`); we read it back from the closure result.
    let spec_card_id = new_id();

    // 2. Resolve cwd + assemble env up front — these go into the
    //    terminal row written inside the tx. Mirror of
    //    `routes::codex_cards::create_codex_card` minus the user-
    //    supplied cwd: the spec card's cwd defaults to `$HOME` (the
    //    spec agent has no project-specific working directory; PR7+
    //    may wire a wave-level cwd field).
    let cwd = crate::routes::codex_cards::default_cwd();
    tracing::info!(
        debug = "pr182",
        phase = "before_load_settings",
        spec_card_id = %spec_card_id,
        "about to load settings"
    );
    let settings = load_settings(s.repo.as_ref()).await?;
    tracing::info!(
        debug = "pr182",
        phase = "after_load_settings",
        "settings loaded; building env map"
    );
    let env = build_codex_env_map(
        s.codex.as_ref(),
        &spec_card_id,
        settings.http_proxy.as_deref(),
        settings.https_proxy.as_deref(),
    );
    tracing::info!(
        debug = "pr182",
        phase = "after_build_env",
        "env map built; entering atomic tx"
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
    let env_for_tx = env.clone();
    let cwd_for_tx = cwd.clone();
    let spec_card_id_for_tx = spec_card_id.clone();
    tracing::info!(
        debug = "pr182",
        phase = "before_write_with_events",
        spec_card_id = %spec_card_id,
        "calling write_with_events_typed"
    );
    let (wave, _event_ids) = write_with_events_typed(
        s.repo.as_ref(),
        actor_id,
        None,
        &s.events,
        &s.card_role_cache,
        move |tx| {
            Box::pin(async move {
                // 3a. Wave row.
                let wave = wave_create_tx(tx, p).await?;
                let wave_id = wave.id.clone();
                let cove_id = wave.cove_id.clone();

                // 3b. Spec card + terminal row. The helper's
                // `card_create_with_id_tx` writes through into the
                // role cache (`Spec` for this call) so the next
                // `enforce_role` step sees it.
                let (spec_card, _term) = card_with_codex_create_tx(
                    tx,
                    spec_card_id_for_tx,
                    wave_id.clone(),
                    None,       // sort: append to end
                    cwd_for_tx, // codex's cwd
                    env_for_tx, // terminal env
                    None,       // prompt: spec cards don't use the
                    // hands-free composer auto-submit
                    // path — the system prompt lives in
                    // $CODEX_HOME/config.toml instead
                    CardRole::Spec, // <— the PR6 binding
                    &cache_for_tx,
                )
                .await?;

                // 3c. Per-event scopes — we now have the real ids.
                let wave_scope = EventScope::Wave {
                    wave: wave_id.clone(),
                    cove: cove_id.clone(),
                };
                let card_scope = EventScope::Card {
                    card: spec_card.id.clone(),
                    wave: wave_id,
                    cove: cove_id,
                };
                let events = vec![
                    (wave_scope, Event::WaveUpdated(wave.clone())),
                    (card_scope, Event::CardAdded(spec_card)),
                ];
                tracing::info!(
                    debug = "pr182",
                    phase = "closure_returning",
                    wave_id = %wave.id,
                    "tx closure built (wave, events); returning to write_with_events_typed"
                );
                Ok((wave, events))
            })
        },
    )
    .await?;
    tracing::info!(
        debug = "pr182",
        phase = "after_write_with_events",
        wave_id = %wave.id,
        "tx committed; events broadcast; about to queue bg spawn"
    );

    // 4. Post-commit: hand off the seed + daemon spawn to a background
    //    `tokio::spawn` task and return 201 immediately. This is a
    //    PR6 second-fix iteration on top of the first warn-and-201
    //    fix: even with the response status downgraded to 201, the
    //    handler was still awaiting `spawn_daemon_for`, whose busy-
    //    poll wait-until-socket-ready loop can hold the response open
    //    for ~3s when the daemon binary is missing (CI shape) — that
    //    delay blew past the web a11y test's 5s navigation timeout
    //    after the route+frontend round-trip overhead.
    //
    //    Architectural contract: persisted rows (the wave + spec
    //    card + spec terminal) and the two broadcast events are the
    //    sync side of `create_wave`. The codex daemon is best-effort
    //    async; the orphan-terminal sweeper reaps a row whose daemon
    //    never came up (~60s grace) and PR7+ adds structured
    //    tombstone events for spec-card-level cleanup automation.
    //
    //    `tokio::spawn` is synchronous: it returns a `JoinHandle`
    //    without awaiting the future, so the 201 response leaves the
    //    handler before the background task even acquires the
    //    runtime. The discarded `JoinHandle` is the standard
    //    fire-and-forget shape — failures inside the task log at
    //    `warn!` and are swallowed; the helper itself takes care of
    //    the logging.
    {
        let state_for_task = s.clone();
        let spec_card_id_for_task = spec_card_id.clone();
        let wave_id_for_task = wave.id.as_str().to_string();
        tracing::info!(
            debug = "pr182",
            phase = "before_spawn",
            spec_card_id = %spec_card_id,
            wave_id = %wave.id,
            "queueing background daemon spawn"
        );
        let bg_card_id = spec_card_id.clone();
        let bg_wave_id = wave.id.as_str().to_string();
        let _bg = tokio::spawn(async move {
            tracing::info!(
                debug = "pr182_bg",
                phase = "bg_start",
                card_id = %bg_card_id,
                wave_id = %bg_wave_id,
                "background daemon spawn task entered"
            );
            seed_and_spawn_spec_daemon(
                state_for_task,
                spec_card_id_for_task,
                wave_id_for_task,
                cwd,
                env,
            )
            .await;
            tracing::info!(
                debug = "pr182_bg",
                phase = "bg_end",
                card_id = %bg_card_id,
                wave_id = %bg_wave_id,
                "background daemon spawn task finished"
            );
        });
        tracing::info!(
            debug = "pr182",
            phase = "after_spawn",
            "background task queued (JoinHandle dropped)"
        );
    }

    tracing::info!(
        card_id = %spec_card_id,
        wave_id = %wave.id,
        "spec card persisted; daemon spawn handed off to background task"
    );
    tracing::info!(
        debug = "pr182",
        phase = "before_response",
        wave_id = %wave.id,
        "returning 201 to client"
    );
    Ok((StatusCode::CREATED, Json(wave)))
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
    let (wave, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.card_role_cache,
        move |tx| {
            Box::pin(async move {
                let wave = wave_update_tx(tx, &id, p).await?;
                Ok((wave.clone(), Event::WaveUpdated(wave)))
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
    // TODO(#136 PR7+): Spec card daemon cleanup on wave delete.
    // Today the spec card row + its codex daemon are orphaned when a
    // wave is deleted: the FK cascade nukes the cards/terminals rows
    // (the schema has ON DELETE CASCADE), and the orphan-terminal
    // sweeper reaps the now-card-less terminal row + sends SIGTERM
    // to the persisted pid. But the codex daemon itself may linger
    // between cascade-delete and sweeper sweep (worst case ~60s), and
    // there is no `WaveDeleted` listener that proactively tears down
    // the spec card's session. PR7+ will either (a) emit a
    // `SpecCardEvicted` tombstone event the supervisor consumes, or
    // (b) wire a direct daemon-kill in this handler before the FK
    // cascade fires. PR6 ships the orphan-sweeper path as the MVP.
    //
    // Look up first (outside the txn) so we know the cove_id for the
    // delete event. Reading outside the txn is fine — there's no
    // concurrent write that could change `wave.cove_id` between the
    // read and the write_with_event start (wave rows are immutable
    // wrt their parent cove).
    let wave = s
        .repo
        .wave_get(&id)
        .await?
        .ok_or_else(|| CalmError::NotFound(format!("wave {id}")))?;
    let cove_id = wave.cove_id.clone();
    let wave_id = wave.id.clone();
    let scope = EventScope::Wave {
        wave: wave_id.clone(),
        cove: cove_id.clone(),
    };
    let (_unit, _id) = write_with_event_typed(
        s.repo.as_ref(),
        actor.to_actor_id(),
        scope,
        None,
        &s.events,
        &s.card_role_cache,
        move |tx| {
            Box::pin(async move {
                wave_delete_tx(tx, wave_id.as_ref()).await?;
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
