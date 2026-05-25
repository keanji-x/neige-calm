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

/// #293 PR3a (B1 crash recovery) — reap any **leaked `codex app-server`
/// process group** left by a previous kernel hard-crash.
///
/// The graceful teardown path
/// ([`terminal_sweeper::reap_spec_push`](crate::terminal_sweeper::reap_spec_push))
/// kills the app-server's process group while the kernel is alive. But if
/// the kernel is `SIGKILL`ed (or the box loses power), the in-process
/// reap never runs and the native `codex app-server` child survives,
/// reparented under `systemd --user`, still bound to its per-card listen
/// socket. The kernel persists the launcher's pgid on the spec-card
/// payload (`appserver_pgid`) precisely so this boot-time sweep can find
/// and reap that orphan — the spec-push parallel to the PTY sweeper's
/// `terminal_set_pid` + SIGTERM recovery, extended to the process group.
///
/// Pid-recycling guard: a persisted pgid could, after a reboot, name an
/// unrelated process. We only `kill(-pgid, …)` a group whose **per-card
/// listen socket still accepts a connection** — i.e. a `codex app-server`
/// is genuinely still bound there. A recycled, unrelated pid would not be
/// listening on our socket, so it is never signaled. After the kill we
/// clean the socket dir. This runs at boot before the registry holds any
/// handle, so there is no live owner to race.
pub async fn reap_orphan_appserver_groups_on_boot(state: &state::AppState) {
    let cards = match state.repo.spec_cards_with_appserver_pgid().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "reap_orphan_appserver_groups_on_boot: query failed; skipping"
            );
            return;
        }
    };
    if cards.is_empty() {
        return;
    }
    let mut reaped = 0usize;
    for (card_id, pgid, sock) in cards {
        // Only reap a group that is genuinely a live, leaked app-server:
        // its listen socket must still accept a connection. This both
        // confirms the server survived the crash AND guards against pid
        // recycling (an unrelated recycled pgid isn't bound to our socket).
        let sock_path = std::path::Path::new(&sock);
        let connectable = tokio::net::UnixStream::connect(sock_path).await.is_ok();
        if !connectable {
            // Either the server already died (socket stale) or never came
            // up. Clean any stale socket file/dir and move on — nothing to
            // kill. (Killing a recycled pgid here would be unsafe.)
            tracing::debug!(
                card_id = %card_id,
                pgid,
                sock = %sock,
                "reap_orphan_appserver_groups_on_boot: socket not connectable; not signaling pgid (stale/recycled)"
            );
            spec_appserver::cleanup_sock_dir(sock_path);
            continue;
        }
        tracing::warn!(
            card_id = %card_id,
            pgid,
            sock = %sock,
            "reap_orphan_appserver_groups_on_boot: leaked codex app-server group found (socket live) — killing group"
        );
        if spec_appserver::signal_process_group(pgid, libc::SIGTERM) {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            spec_appserver::signal_process_group(pgid, libc::SIGKILL);
            reaped += 1;
        }
        spec_appserver::cleanup_sock_dir(sock_path);
    }
    tracing::info!(reaped, "reap_orphan_appserver_groups_on_boot: complete");
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
