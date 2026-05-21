//! Plugin-host error surface.
//!
//! Split out from `crate::error::CalmError` because the host stack has a
//! distinct vocabulary (process I/O, MCP framing, supervisor state machine)
//! that doesn't map cleanly to the HTTP-shaped `CalmError` variants. Slice D
//! will glue these into `CalmError` at the route layer; for now we keep them
//! local so the host can be exercised in isolation.

use std::io;

use thiserror::Error;

use super::version::KernelTooOld;

// ---------------------------------------------------------------------------
// Process-layer errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ProcessError {
    /// `tokio::process::Command::spawn` failed. Most common cause is a bad
    /// `entrypoint.command` path in the manifest or a missing executable
    /// permission bit; the wrapped `io::Error` carries the kernel detail.
    #[error("plugin process spawn failed: {0}")]
    Spawn(#[source] io::Error),

    /// `child.wait()` failed mid-supervision. Rare — typically only seen if
    /// the process exits but the waitpid call races with another consumer.
    #[error("plugin process wait failed: {0}")]
    Wait(#[source] io::Error),

    /// Stop / kill was called on a process that already exited. Slice C and D
    /// callers treat this as benign (the desired post-state is "not running"
    /// either way), so it's its own variant rather than an `Io`.
    #[error("plugin process already exited")]
    AlreadyDead,

    /// SIGTERM was sent but the child did not exit within the grace window.
    /// We follow up with SIGKILL; this error fires only if even SIGKILL +
    /// grace exhausts without the child reaping. In practice means a zombie.
    #[error("plugin process did not exit within kill timeout")]
    KillTimeout,
}

// ---------------------------------------------------------------------------
// MCP-layer errors
// ---------------------------------------------------------------------------

/// Wire-level failures from the JSON-RPC framer or the actor. These are
/// distinct from `RpcError` (which models JSON-RPC's own `error` object
/// per §5.1 of the JSON-RPC 2.0 spec).
#[derive(Debug, Error)]
pub enum McpError {
    /// stdin/stdout I/O died — child closed its end, or we got an `EPIPE`.
    /// Once an `McpClient` returns this, every future call also will. The
    /// supervisor treats it as "the process is gone" and restarts.
    #[error("mcp transport closed: {0}")]
    TransportClosed(String),

    /// We received bytes that didn't parse as a JSON-RPC frame.
    #[error("mcp framing error: {0}")]
    Framing(String),

    /// The actor task panicked or was cancelled before responding. Used as a
    /// last-resort marker so call() doesn't hang forever.
    #[error("mcp client dropped before response arrived")]
    ClientDropped,

    /// `tokio::sync::mpsc` capacity exhausted — happens if the outbound
    /// channel buffer fills (~64 deep). The caller should back off and retry.
    #[error("mcp client outbound buffer full")]
    BufferFull,
}

// ---------------------------------------------------------------------------
// Host-layer errors (what `PluginHost::spawn`/`stop`/etc. return)
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum HostError {
    /// No manifest registered for `id`. Caller probably forgot to install /
    /// the registry didn't pick up the directory.
    #[error("plugin `{0}` not found in registry")]
    NotFound(String),

    /// `spawn` was called on a plugin that's already running. Caller can
    /// `stop` first or call `restart` instead.
    #[error("plugin `{0}` is already running")]
    AlreadyRunning(String),

    /// `config.plugins_disabled` lists this id; the host refuses to spawn it.
    /// Slice D's enable endpoint can override by removing from the list.
    #[error("plugin `{0}` is disabled by config")]
    Disabled(String),

    /// Process-level failure (spawn/wait/kill). The wrapped `ProcessError`
    /// carries the syscall detail.
    #[error(transparent)]
    Spawn(#[from] ProcessError),

    /// MCP wire failure. Most commonly seen during `initialize` handshake
    /// failures or mid-flight transport drops.
    #[error(transparent)]
    Mcp(#[from] McpError),

    /// `initialize` succeeded as a wire round-trip but the plugin's response
    /// was malformed or claimed an incompatible protocol version. We carry
    /// the rejection reason so the supervisor's `last_error` can show it.
    #[error("plugin initialize rejected: {0}")]
    InitializeRejected(String),

    /// State-machine guard: e.g. trying to stop a plugin that's `Spawning`.
    /// The string carries the offending state for diagnostics.
    #[error("plugin in bad state: {0}")]
    BadState(String),

    /// Slice H: plugin's echoed token didn't match what the kernel issued on
    /// spawn. Treated as a security failure — supervisor does NOT respawn,
    /// state event reports `Crashed { reason: "auth handshake failed" }`.
    #[error("plugin auth handshake failed: {0}")]
    AuthMismatch(String),

    /// Issue #45: the manifest's `min_kernel_version` exceeds the running
    /// kernel's version. The check fires before any process spawn, so this
    /// variant never leaves a half-spawned plugin behind. Routes map it to a
    /// 4xx so callers see the typed reason rather than a generic 500.
    #[error(transparent)]
    KernelTooOld(#[from] KernelTooOld),
}
