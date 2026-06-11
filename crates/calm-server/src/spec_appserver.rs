//! Process and socket utilities retained for shared codex app-server
//! supervision and boot recovery.

use std::path::Path;
use std::time::Duration;

use crate::codex_appserver::{ClientInfo, CodexAppServer};

#[derive(Debug)]
pub enum SockDirCleanupOutcome {
    Removed,
    NotPresent,
    Error(std::io::Error),
}

/// Remove the listen socket and its now-empty per-card dir
/// (`<data_dir>/appserver/<card_id>/`). Best-effort: a missing socket /
/// non-empty dir is fine. Mirrors the PTY `remove_file(sock)` cleanup in
/// [`crate::terminal_sweeper::reap_terminal_artifacts`].
pub fn cleanup_sock_dir(sock: &Path) -> SockDirCleanupOutcome {
    let outcome = match std::fs::remove_file(sock) {
        Ok(()) => SockDirCleanupOutcome::Removed,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SockDirCleanupOutcome::NotPresent,
        Err(e) => SockDirCleanupOutcome::Error(e),
    };
    if let Some(dir) = sock.parent() {
        // `remove_dir` only succeeds when empty — exactly what we want
        // (don't nuke a dir that unexpectedly holds other files).
        let _ = std::fs::remove_dir(dir);
    }
    outcome
}

/// #313 problem #1 round-3 (B1) + #335 PR2 — verify that the shared codex
/// app-server socket at `sock` has a live listener BEFORE the caller signals
/// the process group.
///
/// **Why this exists.** After a host reboot a stale process group id could
/// belong to an unrelated process (PIDs/PGIDs are recycled), so a
/// `kill(-pgid, SIGTERM/SIGKILL)` could target arbitrary user processes.
/// Connect alone is not enough: a different listener on a stale path could
/// otherwise authorize a kill. We require both WebSocket connect and a JSON-RPC
/// `initialize` round-trip.
///
/// Returns `true` when the kill is **safe** (initialize succeeded — caller
/// should proceed with `signal_process_group`), `false` when the caller
/// should **skip** the kill (socket missing/refused, non-WS listener,
/// initialize failure/timeout — caller should still `cleanup_sock_dir` to
/// wipe the stale path before respawn).
///
/// Any probe failure is conservative-skip. A false-negative (we skip a kill
/// we could have done) is harmless because boot recovery's `cleanup_sock_dir`
/// plus respawn still works; a false-positive (we kill the wrong process) is
/// the bug we're guarding against.
pub async fn socket_owned_by_appserver(sock: &Path) -> bool {
    match tokio::time::timeout(Duration::from_secs(3), CodexAppServer::connect(sock)).await {
        Err(_) => {
            tracing::warn!(
                sock = %sock.display(),
                "takeover ownership probe: websocket connect timed out — skipping kill"
            );
            false
        }
        Ok(Ok((client, _notifs))) => {
            // Connect + WebSocket upgrade succeeded. Finish the ownership
            // probe with a JSON-RPC initialize round-trip so a random
            // non-codex listener on the same stale path cannot authorize a
            // process-group kill.
            let client = client.with_request_timeout(Duration::from_secs(2));
            match tokio::time::timeout(
                Duration::from_secs(3),
                client.initialize(ClientInfo {
                    name: "neige-calm-takeover-probe".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                }),
            )
            .await
            {
                Ok(Ok(_)) => {
                    tracing::debug!(
                        sock = %sock.display(),
                        "takeover ownership probe: initialize OK — socket is a codex app-server"
                    );
                    true
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        sock = %sock.display(),
                        error = %e,
                        "takeover ownership probe: initialize failed — skipping kill"
                    );
                    false
                }
                Err(_) => {
                    tracing::warn!(
                        sock = %sock.display(),
                        "takeover ownership probe: initialize timed out — skipping kill"
                    );
                    false
                }
            }
        }
        Ok(Err(e)) => {
            let msg = e.to_string();
            if msg.contains("No such file")
                || msg.contains("os error 2")
                || msg.contains("Connection refused")
                || msg.contains("os error 111")
            {
                // ENOENT — socket file gone (graceful teardown / host
                // wipe) → no listener exists, nothing to kill.
                // ECONNREFUSED — socket path exists, no listener bound
                // (stale dirent from a crashed process) → likewise
                // nothing of ours to kill.
                tracing::info!(
                    sock = %sock.display(),
                    error = %e,
                    "takeover ownership probe: socket has no live listener — \
                     skipping kill of persisted pgid (post-reboot PID may be unrelated); \
                     caller should still cleanup_sock_dir before respawn"
                );
                false
            } else {
                // Any other error (EACCES, EAGAIN, WS handshake failure,
                // non-JSON-RPC listener, …): we can't prove ownership.
                // Default to skipping the kill — safety over reaping a
                // leaked group (the respawn path can retry, but reviving a
                // SIGKILLed user process can't).
                //
                // #315 round-4 (N3) — the conservative-skip-kill on
                // unrecognized errors trades a worst-case "stale socket
                // file leaks forever" for the worst-case "we SIGTERM/
                // SIGKILL an unrelated process group whose pid was
                // recycled into our persisted pgid slot post-reboot".
                tracing::warn!(
                    sock = %sock.display(),
                    error = %e,
                    "takeover ownership probe: app-server probe failed — skipping kill \
                     to avoid signaling unrelated process group"
                );
                false
            }
        }
    }
}
