//! Calm kernel — minimal container/PTY core. Business semantics (tasks,
//! calendar, plans, git, ...) live in out-of-process plugins reached via MCP.
//!
//! Module map:
//! ```text
//! model         entity types + DTOs (Cove/Wave/Card/Overlay/Terminal/Plugin)
//! error         CalmError + Result alias + IntoResponse
//! event         Event enum + EventBus (broadcast fan-out)
//! db            Repo trait
//!   ├ mod.rs    `Repo` trait + helper free fns
//!   └ sqlite.rs SqlxRepo (production + in-memory dev/test default via
//!               `sqlite::memory:`)
//! routes        HTTP API
//!   ├ coves.rs       (track B)
//!   ├ waves.rs       (track B)
//!   ├ cards.rs       (track B)
//!   ├ overlays.rs    (track B)
//!   ├ plugins.rs     (M2 stub)
//!   └ terminal.rs    (track D, REST half)
//! ws            WebSocket endpoints
//!   ├ events.rs      (track C)
//!   └ terminal.rs    (track D, WS half)
//! plugin_host   M2 placeholder
//! state         AppState (Arc<Repo>, EventBus, DaemonClient, PluginHost)
//! config        Config (CLI / env)
//! ```

pub mod actor;
pub mod auth;

/// #177 root-cause refactor — replace the WS handler's auto-revive with
/// a single boot-time sweep that re-spawns the `calm-session-daemon`
/// for every terminal row whose persisted socket is unreachable. This
/// is the **only** kernel-internal auto-revive seam: the WS upgrade
/// path is now probe-only and surfaces a 500 / browser-reconnect on a
/// dead daemon (see [`ws::terminal::resolve_live_sock`]).
///
/// Why a boot-time sweep is enough: production daemons live as child
/// processes of the kernel.
///   * **kernel restart while daemons were running** — when the kernel
///     exits, its children may survive (no `prctl(PR_SET_PDEATHSIG)`
///     today). Their `daemon_handle` lingers on the row but the
///     socket file path may be stale. We probe + respawn unreachable
///     ones, no-op the live ones.
///   * **daemon crash mid-session** — the row still points at a stale
///     socket; the next WS upgrade returns 500 (probe-only resolve),
///     the browser's "Reconnect" UI calls into the wave detail re-
///     fetch path, and a future spawn (or the operator restarting the
///     kernel) brings it back. We deliberately *don't* auto-revive
///     crashes on the WS hot path because that path can't carry the
///     per-card MCP token or any env that was generated post-create
///     — keeping the crash recovery opt-in is safer than a partial
///     respawn.
///
/// The sweep walks `terminals` rows whose `daemon_handle IS NOT NULL`,
/// probes the socket, and on connect-failure clears the handle and
/// calls `spawn_daemon_with_parts` with the row's existing program /
/// cwd / env. The row's `theme_fg / _bg` (NOT NULL post-migration
/// 0017) flow through to the new daemon argv automatically — every
/// spawn reads theme from the row.
pub async fn revive_orphans_on_boot(state: &state::AppState) {
    let rows = match state.repo.terminals_with_daemon_handle().await {
        Ok(rs) => rs,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "revive_orphans_on_boot: list-orphans query failed; skipping sweep"
            );
            return;
        }
    };
    let mut respawned = 0usize;
    let mut alive = 0usize;
    for term in rows {
        let Some(handle) = term.daemon_handle.clone() else {
            continue;
        };
        // Probe — if the socket accepts a connect the daemon is already
        // alive (kernel restarted but daemons survived); no action.
        if tokio::net::UnixStream::connect(&handle).await.is_ok() {
            alive += 1;
            continue;
        }
        tracing::info!(
            terminal_id = %term.id,
            sock = %handle,
            "revive_orphans_on_boot: socket unreachable — respawning",
        );
        // Clear the stale handle before respawn — the helper writes a
        // fresh one on success.
        let _ = db::RepoOutOfDomain::terminal_set_handle(state.repo.as_ref(), &term.id, None).await;
        let env = term.env.clone();
        if let Err(e) = routes::terminal::spawn_daemon_with_parts(
            state.daemon.as_ref(),
            state.repo.as_ref(),
            &term,
            &term.program,
            &term.cwd,
            &env,
        )
        .await
        {
            tracing::warn!(
                terminal_id = %term.id,
                error = %e,
                "revive_orphans_on_boot: respawn failed; row stays orphaned and the next WS attach returns 500",
            );
        } else {
            respawned += 1;
        }
    }
    tracing::info!(respawned, alive, "revive_orphans_on_boot: complete",);
}

/// #313 problem #1 — boot-time **takeover** of in-flight spec waves.
///
/// Replaces the previous "boot-time kill" sweep
/// (`reap_orphan_appserver_groups_on_boot` — see the parent design in
/// issue #313). Today's posture is: every spec card whose payload carries
/// a `codex_thread_id` AND whose wave is **not terminal** gets:
///
///   1. A live `codex app-server` re-established for it — reused if the
///      previous process is still bound to its persisted socket (kernel
///      hard-crash → systemd reparenting), else freshly respawned (graceful
///      teardown / `kill_on_drop` reaped it on the way down).
///   2. `initialize` + `thread/resume(<codex_thread_id>)` on that server —
///      based on the on-disk rollout (so the first round-trip from the
///      original boot has to have completed; otherwise resume returns
///      `-32600 "no rollout found"` and we leave the wave inert, see below).
///   3. A fresh [`SpecPushHandle`] registered in [`SpecPushRegistry`]
///      keyed by [`crate::ids::WaveId`], identical to what
///      [`crate::routes::waves::create_wave`] would have inserted.
///   4. **Catch-up replay** of every persisted event with `id >
///      push_watermark` for that wave, routed through the dispatcher's
///      normal push path ([`crate::dispatcher::Dispatcher::catch_up_push`])
///      so dedup, queue, and turn-phase semantics are byte-identical to
///      steady-state delivery. The in-memory
///      [`crate::event_cursor::EventCursorCache`] is seeded from the
///      persisted `push_watermark` BEFORE the replay starts.
///
/// Every failure mode is **non-fatal at boot** (boot stays best-effort,
/// matching `create_wave`'s 201-when-spec-fails posture):
///
///   * `thread/resume` returns `-32600 "no rollout found"` (the prior boot
///     persisted `codex_thread_id` but the wave never completed turn #1) →
///     log warn, clear the stale push fields (`codex_thread_id`, sock,
///     pgid, watermark) so the next boot doesn't retry, leave the wave
///     inert. Matches the "lazy wave" state from issue #313 problem #2
///     (out of scope for this PR — just don't crash).
///   * App-server fails to spawn or the socket never becomes ready → log
///     warn, leave the wave inert. The next boot will retry.
///   * The wave's lifecycle is terminal — SQL `WHERE` already filtered it
///     out; this path never sees it.
///   * `codex_thread_id` is absent — SQL `WHERE` filtered it out; this
///     path never sees it either.
///   * Any individual wave's takeover failing must NOT fail the kernel
///     boot.
///
/// Preserves the #293/#311 push-path invariants: no pull fallback, dedup
/// via `envelope_id > push_watermark`, mid-turn queue semantics on
/// resumed handles (the `SpecPushHandle` produced by resume goes through
/// the same `consume_notifications` task as one produced by spawn).
pub async fn takeover_spec_appservers_on_boot(state: &state::AppState) {
    let cards = match state.repo.spec_cards_for_boot_takeover().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "takeover_spec_appservers_on_boot: query failed; skipping"
            );
            return;
        }
    };
    if cards.is_empty() {
        tracing::info!("takeover_spec_appservers_on_boot: no in-flight spec waves to take over");
        return;
    }
    tracing::info!(
        candidates = cards.len(),
        "takeover_spec_appservers_on_boot: starting boot takeover"
    );
    // Settings drive the proxy env handed to a respawned app-server (same
    // shape `create_wave` builds via `build_codex_env_map`).
    let settings = match crate::routes::settings::load_settings(state.repo.as_ref()).await {
        Ok(s) => s,
        Err(e) => {
            // A settings load failure shouldn't block takeover — fall back
            // to empty proxies (the app-server still boots, just without
            // an override). `create_wave` would surface this as a 500 on
            // the hot path, but at boot we prefer "best-effort proceed".
            tracing::warn!(
                error = %e,
                "takeover_spec_appservers_on_boot: load_settings failed; proceeding with no proxy override"
            );
            crate::routes::settings::Settings::default()
        }
    };

    let mut reused = 0usize;
    let mut respawned = 0usize;
    let mut inert = 0usize;
    for (card_id, wave_id, thread_id, persisted_pgid, persisted_sock, watermark) in cards {
        let wave_key: crate::ids::WaveId = wave_id.clone().into();
        // Per-wave best-effort: failures inside this block are logged and
        // we move on to the next wave (the kernel boot proceeds regardless).
        let outcome = try_takeover_one_wave(
            state,
            &settings,
            &card_id,
            &wave_key,
            &thread_id,
            persisted_pgid,
            persisted_sock.as_deref(),
            watermark,
        )
        .await;
        match outcome {
            TakeoverOutcome::Reused => reused += 1,
            TakeoverOutcome::Respawned => respawned += 1,
            TakeoverOutcome::Inert => inert += 1,
        }
    }
    tracing::info!(
        reused,
        respawned,
        inert,
        "takeover_spec_appservers_on_boot: complete"
    );
}

/// Per-wave outcome of [`takeover_spec_appservers_on_boot`].
#[derive(Debug, Clone, Copy)]
enum TakeoverOutcome {
    /// The persisted app-server was still alive and accepting connections;
    /// we adopted it via `initialize` + `thread/resume`. Existing process
    /// group is now owned by the registered [`SpecPushHandle`].
    Reused,
    /// The persisted app-server was gone (or never alive); we spawned a
    /// fresh one and ran `initialize` + `thread/resume` against it.
    Respawned,
    /// The wave is left without a live push channel. Either resume
    /// returned `-32600` (no rollout — payload cleared) or the
    /// spawn/connect/handshake errored. The dispatcher's missing-handle
    /// path will warn on the next live event and move on.
    Inert,
}

#[allow(clippy::too_many_arguments)]
async fn try_takeover_one_wave(
    state: &state::AppState,
    settings: &crate::routes::settings::Settings,
    card_id: &str,
    wave_id: &crate::ids::WaveId,
    thread_id: &str,
    persisted_pgid: Option<i32>,
    persisted_sock: Option<&str>,
    watermark: i64,
) -> TakeoverOutcome {
    // 1. Try to adopt a live, persisted app-server first. The pid-recycling
    //    guard from the old reap sweep applies here too: only treat the
    //    persisted pgid as "alive" if the persisted socket actually accepts
    //    a connection AND the pgid still exists. A recycled, unrelated pgid
    //    won't be listening on our socket, so it never reaches the adopt
    //    path. (The probe is non-destructive: a failed adoption falls
    //    through to respawn.)
    if let (Some(pgid), Some(sock)) = (persisted_pgid, persisted_sock) {
        let sock_path = std::path::Path::new(sock);
        let connectable = tokio::net::UnixStream::connect(sock_path).await.is_ok();
        let alive = pgid > 1 && unsafe { libc::kill(pgid, 0) } == 0;
        if connectable && alive {
            tracing::info!(
                card_id, wave_id = %wave_id, thread_id, pgid, sock,
                "takeover: persisted app-server is alive; adopting (no respawn)",
            );
            match spec_appserver::adopt_live_appserver(pgid, thread_id, sock_path).await {
                Ok(handle) => {
                    register_and_catch_up(state, card_id, wave_id, watermark, handle).await;
                    return TakeoverOutcome::Reused;
                }
                Err(e) => {
                    // Adoption handshake failed — likely `-32600 "no rollout
                    // found"` against a half-broken server. Fall through to
                    // respawn rather than give up: a fresh server may answer
                    // the resume (the rollout is on disk; the existing
                    // server may be wedged for some other reason). If the
                    // respawn ALSO fails we land in the inert path.
                    tracing::warn!(
                        card_id, wave_id = %wave_id, error = %e,
                        "takeover: adopt failed against persisted live app-server; trying respawn",
                    );
                    // Kill the wedged server so the respawn can rebind the
                    // socket. (The old reap sweep's pid-recycling guard ran
                    // BEFORE this fallback, so we know the pgid really
                    // names our server.)
                    spec_appserver::signal_process_group(pgid, libc::SIGTERM);
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    spec_appserver::signal_process_group(pgid, libc::SIGKILL);
                    spec_appserver::cleanup_sock_dir(sock_path);
                }
            }
        } else {
            // Either the persisted process is gone (graceful teardown
            // already killed it on the way down) or it was alive but
            // unreachable. Clean any stale socket and move to respawn.
            tracing::debug!(
                card_id, wave_id = %wave_id, pgid, sock,
                connectable, alive,
                "takeover: persisted app-server not adoptable; will respawn",
            );
            spec_appserver::cleanup_sock_dir(sock_path);
        }
    }

    // 2. Respawn path: build the env the way `create_wave` does, point at
    //    the per-card socket path (same resolver as the route), and run
    //    `resume_spec_appserver` (the create-wave shape, swapping
    //    thread/start + turn/start + await turn/started for thread/resume).
    let env_map = crate::spec_card::build_codex_env_map(
        state.codex.as_ref(),
        card_id,
        settings.http_proxy.as_deref(),
        settings.https_proxy.as_deref(),
        // No MCP env on respawn: the per-card `$CODEX_HOME/config.toml`
        // already bakes `NEIGE_MCP_TOKEN`/`NEIGE_MCP_SOCKET` into the
        // `[mcp_servers.calm].env` block, and we don't have the raw token
        // (only the hash is persisted). Codex picks the values up from
        // the config block when it spawns its MCP transport — same path
        // a live respawned server takes today.
        None,
        None,
    );
    let sock = state.daemon.appserver_sock_path(card_id);
    let sock_dir = state.daemon.appserver_sock_dir(card_id);
    if let Err(e) = std::fs::create_dir_all(&sock_dir) {
        tracing::warn!(
            card_id, wave_id = %wave_id, error = %e,
            "takeover: mkdir appserver sock dir failed; leaving wave inert",
        );
        return TakeoverOutcome::Inert;
    }
    match spec_appserver::resume_spec_appserver(&state.codex.codex_bin, &env_map, thread_id, &sock)
        .await
    {
        Ok(handle) => {
            tracing::info!(
                card_id, wave_id = %wave_id, thread_id,
                "takeover: respawned codex app-server + thread/resume succeeded",
            );
            // Persist the fresh pgid + sock for the NEXT boot cycle so a
            // hard-crash between this point and the next graceful
            // teardown can probe the persisted pgid against the new
            // process. (Same write `create_wave` does post-spawn, minus
            // the codex_thread_id which is already persisted.)
            persist_post_respawn_fields(
                state,
                card_id,
                handle.pgid,
                &handle.sock.to_string_lossy(),
            )
            .await;
            register_and_catch_up(state, card_id, wave_id, watermark, handle).await;
            TakeoverOutcome::Respawned
        }
        Err(e) => {
            // Classify the failure: `-32600 "no rollout found"` means the
            // wave never completed turn #1 last boot, so the rollout file
            // doesn't exist on disk and no respawn can ever resume it.
            // Clear the stale push fields so the next boot stops retrying
            // — the wave is inert until issue #313 problem #2 wires up a
            // re-run path (out of scope).
            let msg = e.to_string();
            let no_rollout = msg.contains("no rollout") || msg.contains("-32600");
            if no_rollout {
                tracing::warn!(
                    card_id, wave_id = %wave_id, thread_id, error = %msg,
                    "takeover: thread/resume returned -32600 no rollout; clearing stale push state — wave inert until manual restart (#313 problem #2)",
                );
                if let Err(e2) = state.repo.spec_card_clear_push_state(card_id).await {
                    tracing::warn!(
                        card_id, error = %e2,
                        "takeover: spec_card_clear_push_state failed (best-effort)",
                    );
                }
            } else {
                tracing::warn!(
                    card_id, wave_id = %wave_id, thread_id, error = %msg,
                    "takeover: respawn app-server / resume failed; leaving wave inert (next boot retries)",
                );
            }
            TakeoverOutcome::Inert
        }
    }
}

/// Persist the freshly-respawned app-server's pgid + sock back onto the
/// spec card's payload via a small JSON-merge UPDATE. Identical shape to
/// the create-wave persist, minus `codex_thread_id` (already on the row)
/// and `push_watermark` (already on the row — we MUST NOT clobber it).
async fn persist_post_respawn_fields(
    state: &state::AppState,
    card_id: &str,
    pgid: i32,
    sock: &str,
) {
    // Same pattern as `spec_card_set_push_watermark`: a single-statement
    // JSON-merge UPDATE that touches only the named keys. Going through
    // `write_with_event_typed` would emit a `CardUpdated` event for what
    // is purely kernel-internal bookkeeping — same reason terminal PIDs /
    // handles go through `RepoOutOfDomain` instead.
    if let Err(e) = state
        .repo
        .spec_card_set_appserver_after_takeover(card_id, pgid, sock)
        .await
    {
        tracing::warn!(
            card_id, error = %e,
            "takeover: persist post-respawn pgid+sock failed; in-memory handle is parked, next boot will probe stale fields",
        );
    }
}

/// Register the resumed/adopted [`SpecPushHandle`] in the registry and
/// catch the spec thread up with every event `id > watermark` for this
/// wave via the dispatcher's normal push path.
async fn register_and_catch_up(
    state: &state::AppState,
    card_id: &str,
    wave_id: &crate::ids::WaveId,
    watermark: i64,
    handle: spec_appserver::SpecPushHandle,
) {
    // Seed the in-memory push cursor from the persisted watermark BEFORE
    // we register the handle, so a concurrent live event landing right
    // after we register still dedups against the correct floor (the live
    // event would `bump` to its own id; that's monotonic, so seeding to
    // `watermark` only ever raises the floor from 0 to watermark).
    let card_key: crate::ids::CardId = card_id.to_string().into();
    state.dispatcher.seed_push_cursor(card_key, watermark);

    // Register the handle — dispatcher.push_to_spec resolves on this.
    state.spec_push.insert(wave_id.clone(), handle);

    // Catch-up replay: read every `id > watermark` event from the log and
    // re-route the wave-scoped push kinds through the dispatcher.
    let kinds_match = |e: &event::Event| {
        matches!(
            e,
            event::Event::TaskCompleted { .. }
                | event::Event::TaskFailed { .. }
                | event::Event::WaveReportEdited { .. }
        )
    };
    let rows = match state.repo.events_since(watermark, None).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                card_id, wave_id = %wave_id, watermark, error = %e,
                "takeover: events_since(catch-up) failed; spec thread will only see new live events from here",
            );
            return;
        }
    };
    let mut replayed = 0usize;
    for (id, _ver, scope, ev) in rows {
        // Only events scoped to (or under) this wave count; only the three
        // push kinds the dispatcher routes; only the user-authored
        // `wave.report_edited` (matches the dispatcher's live filter).
        let Some(ev_wave) = scope.wave_id() else {
            continue;
        };
        if ev_wave != wave_id {
            continue;
        }
        if !kinds_match(&ev) {
            continue;
        }
        if let event::Event::WaveReportEdited { author, .. } = &ev
            && *author != event::EditAuthor::User
        {
            continue;
        }
        // Push through the dispatcher so dedup (watermark check), per-wave
        // serialization, and the turn-phase decision all run identically to
        // steady state. The dedup will skip any rows that were re-broadcast
        // by the bus between persist and now (rare; the kernel just booted).
        state
            .dispatcher
            .catch_up_push(wave_id.clone(), ev, id)
            .await;
        replayed += 1;
    }
    if replayed > 0 {
        tracing::info!(
            card_id, wave_id = %wave_id, replayed, watermark,
            "takeover: catch-up replay pushed events to resumed spec thread",
        );
    } else {
        tracing::debug!(
            card_id, wave_id = %wave_id, watermark,
            "takeover: no catch-up events above watermark",
        );
    }
}

pub mod card_fsm;
pub mod card_role_cache;
pub mod codex_appserver;
pub mod codex_auto_submit;
pub mod config;
pub mod db;
pub mod dispatcher;
pub mod error;
pub mod event;
pub mod event_cursor;
pub mod ids;
pub mod mcp_server;
pub mod model;
pub mod openapi;
pub mod plugin_host;
pub mod replay;
pub mod role_gate;
pub mod routes;
pub mod spec_appserver;
pub mod spec_card;
pub mod state;
pub mod terminal_sweeper;
pub mod validation;
pub mod wave_cove_cache;
pub mod wave_lifecycle;
pub mod wave_report;
pub mod wave_report_doc;
pub mod ws;
