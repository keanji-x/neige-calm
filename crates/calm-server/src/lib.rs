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

/// #177 root-cause refactor (commit 3) — replace the WS handler's
/// auto-revive with a single boot-time sweep that re-spawns the
/// `calm-session-daemon` for every terminal row whose persisted
/// socket is unreachable. This is the **only** kernel-internal
/// auto-revive seam: the WS upgrade path is now probe-only and
/// surfaces a 500 / browser-reconnect on a dead daemon (see
/// `ws::terminal::resolve_live_sock`).
///
/// Why a boot-time sweep is enough: production daemons live as
/// child processes of the kernel. The two cases that produced an
/// orphan row pre-#177 are
///   * **kernel restart while daemons were running** — the daemon
///     `prctl(PR_SET_PDEATHSIG)` (TODO) kills them when the kernel
///     exits, but their `daemon_handle` lingers on the row until
///     this sweep clears it. We probe + respawn.
///   * **daemon crash mid-session** — the row still points at a
///     stale socket; the next WS upgrade returns 500, the browser's
///     "Reconnect" UI calls into the wave detail re-fetch path, and
///     a future spawn (or the operator restarting the kernel) brings
///     it back. We deliberately *don't* auto-revive crashes here
///     because that path can't carry the per-card MCP token or any
///     env that was generated post-create — keeping the crash recovery
///     opt-in (operator restart) is safer than a partial respawn.
///
/// The sweep walks `terminals` rows whose `daemon_handle IS NOT NULL`,
/// probes the socket, and on `ECONNREFUSED` clears the handle and
/// calls `spawn_daemon_with_parts` with the row's existing
/// program/cwd/env. The row's `theme_fg/bg` (NOT NULL via migration
/// 0013) end up on the new daemon's argv just like any other spawn.
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
    for term in rows {
        let Some(handle) = term.daemon_handle.as_deref() else {
            continue;
        };
        // Probe — if the socket accepts a connect the daemon is
        // already alive (kernel restarted but daemons survived); no
        // action needed.
        if tokio::net::UnixStream::connect(handle).await.is_ok() {
            continue;
        }
        tracing::info!(
            terminal_id = %term.id,
            sock = %handle,
            "revive_orphans_on_boot: socket unreachable — respawning"
        );
        // Clear the stale handle before respawn — the helper writes
        // a fresh one on success.
        let _ = state.repo.terminal_set_handle(&term.id, None).await;
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
                "revive_orphans_on_boot: respawn failed; row will need operator action"
            );
        }
    }
}

pub mod card_fsm;
pub mod card_role_cache;
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
pub mod spec_card;
pub mod state;
pub mod terminal_sweeper;
pub mod validation;
pub mod ws;
