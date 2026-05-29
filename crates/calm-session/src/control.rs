//! Control-plane messages between calm-server and calm-proc-supervisor.
//!
//! Phase 1 keeps this narrow: the supervisor is only a stable fork broker for
//! session daemons. The daemon/browser protocol remains in `crate::ClientMsg`
//! and `crate::DaemonMsg`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlMsg {
    EnsureProc(EnsureProcRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsureProcRequest {
    pub proc_id: String,
    pub program: String,
    pub args: Vec<String>,
    pub envs: Vec<(String, String)>,
    pub cwd: String,
    pub ready_timeout_ms: u64,
}

/// Two-phase reply: supervisor emits `Spawned` immediately after fork
/// (so the client can persist pid + handle before readiness — the
/// pre-#388 ordering the dispatcher's fast-exit-preserve discriminator
/// depends on), then emits `Ready` or `Failed` after the ready-fd
/// handshake or child-exit drains. A `SpawnFailed` short-circuits when
/// the fork itself fails — no `Spawned` arrives in that case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlReply {
    /// Process forked; pid is final. Client persists pid + handle now.
    Spawned { pid: u32 },
    /// Daemon wrote its ready signal. Spawn fully succeeded.
    Ready,
    /// Readiness failed after spawn: child exited early without ready,
    /// or the ready-fd backstop timed out. `pid` is still valid for
    /// rollback reap.
    ReadyFailed {
        error: String,
        child_already_reaped: bool,
    },
    /// Fork itself failed (e.g. ENOENT on program path). No pid; the
    /// stream closes after this frame.
    SpawnFailed {
        error: String,
        child_already_reaped: bool,
    },
}
