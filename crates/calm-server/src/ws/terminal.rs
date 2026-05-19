//! `GET /api/terminals/:id` (WebSocket upgrade). **Owned by Track D.**
//!
//! ## Protocol
//!
//! Frames carry the `calm_session::ClientMsg` / `DaemonMsg` enums encoded as
//! JSON text. Each WS text frame is exactly one serde-JSON `ClientMsg` (going
//! up) or `DaemonMsg` (coming down). Binary WS frames are not used in this
//! bridge — the wave's own xterm.js client handles VT replay on top of
//! `DaemonMsg::Hello.replay` / `DaemonMsg::Stdout` byte arrays delivered as
//! JSON byte-arrays.
//!
//! This is intentionally a *thin* bridge: history, replay, seq numbering,
//! reconnect epochs etc. all live in the daemon (Hello.replay) or are handled
//! at the daemon attach layer. Calm-server just shuttles frames.

use crate::error::Result;
use crate::routes::terminal::spawn_daemon_for;
use crate::state::AppState;
use axum::{
    Router,
    extract::{
        Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    routing::get,
};
use futures::{SinkExt, StreamExt};
use calm_session::{ClientMsg, DaemonMsg, read_frame, write_frame};
use std::path::PathBuf;
use tokio::net::UnixStream;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/terminals/{id}", get(upgrade))
}

async fn upgrade(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(s): State<AppState>,
) -> impl IntoResponse {
    // Resolve the socket path *before* the upgrade so a missing terminal
    // returns a proper HTTP error instead of a 101 + immediate close.
    // If the daemon for an existing row has died (shell exited, OS killed
    // it, calm-server restart unhooked it, …), respawn here so the client
    // re-attach feels seamless.
    let sock = match resolve_live_sock(&s, &id).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    ws.on_upgrade(move |socket| handle(socket, sock))
        .into_response()
}

/// Resolve the socket path for a terminal row, **revive a dead daemon if
/// necessary**. The revive path:
///   1. Read the terminal row from the repo.
///   2. If `daemon_handle` is set, probe the socket (`UnixStream::connect`).
///      If it connects, the daemon is alive — return the path.
///   3. Otherwise re-spawn the daemon with the row's original program /
///      cwd / env, wait for it to be ready, persist the new handle, and
///      return that path.
async fn resolve_live_sock(s: &AppState, id: &str) -> Result<PathBuf> {
    let term = s
        .repo
        .terminal_get(id)
        .await?
        .ok_or_else(|| crate::error::CalmError::NotFound(format!("terminal {id}")))?;

    if let Some(handle) = term.daemon_handle.as_ref() {
        if let Ok(_probe) = UnixStream::connect(handle).await {
            // Live daemon — fast path.
            return Ok(PathBuf::from(handle));
        }
        tracing::info!(
            terminal_id = %term.id,
            sock = %handle,
            "daemon socket unreachable — respawning"
        );
    } else {
        tracing::info!(terminal_id = %term.id, "terminal has no daemon_handle — spawning");
    }

    // Cold path: respawn. `spawn_daemon_for` updates `daemon_handle`
    // when it succeeds, so we re-read to get the canonical path.
    let env = term.env.clone();
    spawn_daemon_for(s, &term, &term.program, &term.cwd, &env).await?;
    let refreshed = s
        .repo
        .terminal_get(id)
        .await?
        .ok_or_else(|| crate::error::CalmError::Internal("terminal vanished after respawn".into()))?;
    let handle = refreshed.daemon_handle.ok_or_else(|| {
        crate::error::CalmError::Internal(format!(
            "terminal {id}: daemon_handle still missing after respawn"
        ))
    })?;
    Ok(PathBuf::from(handle))
}

async fn handle(socket: WebSocket, sock: PathBuf) {
    let stream = match UnixStream::connect(&sock).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, sock = ?sock, "connect daemon socket failed");
            return;
        }
    };
    let (mut rd, mut wr) = stream.into_split();
    let (mut ws_tx, mut ws_rx) = socket.split();

    // WS → daemon: parse each text frame as ClientMsg, write to socket.
    let up = async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                Message::Text(text) => {
                    let parsed: ClientMsg = match serde_json::from_str(&text) {
                        Ok(m) => m,
                        Err(e) => {
                            tracing::warn!(error = %e, "unparseable ClientMsg JSON; dropping");
                            continue;
                        }
                    };
                    if write_frame(&mut wr, &parsed).await.is_err() {
                        break;
                    }
                }
                // Binary frames could be used as an optimization for Stdin
                // (skip JSON wrapping). Not part of the documented contract
                // — drop for now, surface if the frontend wants it.
                Message::Binary(_) => {}
                Message::Close(_) => break,
                _ => {}
            }
        }
    };

    // Daemon → WS: read framed bincode DaemonMsg, ship as JSON text.
    let down = async move {
        loop {
            let msg: DaemonMsg = match read_frame(&mut rd).await {
                Ok(m) => m,
                Err(_) => break,
            };
            let exit = matches!(msg, DaemonMsg::ChildExited { .. });
            let text = match serde_json::to_string(&msg) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "serialize DaemonMsg failed");
                    continue;
                }
            };
            if ws_tx.send(Message::Text(text.into())).await.is_err() {
                break;
            }
            if exit {
                break;
            }
        }
        let _ = ws_tx.send(Message::Close(None)).await;
    };

    tokio::select! {
        _ = up => {}
        _ = down => {}
    }
}
