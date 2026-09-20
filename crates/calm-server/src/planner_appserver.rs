//! Process and socket utilities for shared codex app-server supervision and boot recovery.

use std::path::Path;
use std::time::Duration;

use crate::codex_appserver::{ClientInfo, CodexAppServer};

#[derive(Debug)]
pub enum SockDirCleanupOutcome {
    Removed,
    NotPresent,
    Error(std::io::Error),
}

/// Best-effort: a missing socket or a non-empty dir is fine.
pub fn cleanup_sock_dir(sock: &Path) -> SockDirCleanupOutcome {
    let outcome = match std::fs::remove_file(sock) {
        Ok(()) => SockDirCleanupOutcome::Removed,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SockDirCleanupOutcome::NotPresent,
        Err(e) => SockDirCleanupOutcome::Error(e),
    };
    if let Some(dir) = sock.parent() {
        // `remove_dir` only succeeds when empty; don't nuke a dir that unexpectedly holds other files.
        let _ = std::fs::remove_dir(dir);
    }
    outcome
}

/// Verify the socket has a live listener BEFORE the caller signals the process group: after a reboot the persisted pgid may be recycled to an unrelated process, so both WebSocket connect and a JSON-RPC `initialize` round-trip are required.
/// Returns `true` when the kill is safe; any probe failure is a conservative skip (the caller should still `cleanup_sock_dir`).
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
                // ENOENT / ECONNREFUSED: no listener exists, nothing to kill.
                tracing::info!(
                    sock = %sock.display(),
                    error = %e,
                    "takeover ownership probe: socket has no live listener — \
                     skipping kill of persisted pgid (post-reboot PID may be unrelated); \
                     caller should still cleanup_sock_dir before respawn"
                );
                false
            } else {
                // Any other error: ownership unproven, skip the kill — the respawn path can retry, but reviving a SIGKILLed user process can't.
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
