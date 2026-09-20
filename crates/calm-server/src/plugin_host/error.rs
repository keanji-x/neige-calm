//! Plugin-host error surface: process I/O, MCP framing and supervisor state, kept apart from the HTTP-shaped `CalmError`.

use std::io;

use thiserror::Error;

use super::version::KernelTooOld;

#[derive(Debug, Error)]
pub enum ProcessError {
    /// Most common cause is a bad `entrypoint.command` path or a missing executable bit.
    #[error("plugin process spawn failed: {0}")]
    Spawn(#[source] io::Error),

    #[error("plugin process wait failed: {0}")]
    Wait(#[source] io::Error),

    /// Callers treat this as benign (the desired post-state is "not running" either way), so it is its own variant rather than an `Io`.
    #[error("plugin process already exited")]
    AlreadyDead,

    /// Fires only if even SIGKILL + grace exhausts without the child reaping; in practice a zombie.
    #[error("plugin process did not exit within kill timeout")]
    KillTimeout,
}

/// Wire-level failures from the JSON-RPC framer or the actor, distinct from `RpcError` (JSON-RPC's own `error` object).
#[derive(Debug, Error)]
pub enum McpError {
    /// Once an `McpClient` returns this, every future call also will; the supervisor treats it as "the process is gone".
    #[error("mcp transport closed: {0}")]
    TransportClosed(String),

    #[error("mcp framing error: {0}")]
    Framing(String),

    /// The actor task panicked or was cancelled before responding, so `call()` does not hang forever.
    #[error("mcp client dropped before response arrived")]
    ClientDropped,

    /// Outbound channel buffer (~64 deep) is full; the caller should back off and retry.
    #[error("mcp client outbound buffer full")]
    BufferFull,

    /// Exact-equal match against `KERNEL_PROTOCOL_VERSION` is enforced; the handshake fails and the process is reaped.
    #[error("mcp protocol version mismatch: kernel={kernel}, plugin={plugin}")]
    ProtocolVersionMismatch { kernel: String, plugin: String },
}

#[derive(Debug, Error)]
pub enum HostError {
    #[error("plugin `{0}` not found in registry")]
    NotFound(String),

    #[error("plugin `{0}` is already running")]
    AlreadyRunning(String),

    /// `config.plugins_disabled` lists this id.
    #[error("plugin `{0}` is disabled by config")]
    Disabled(String),

    /// The plugin's DB row says `enabled = false`. Deliberately NOT [`Self::Disabled`] (the config kill switch): two stores, two remedies. Raised before any spawn or token mint; an absent row is not this error.
    #[error("plugin `{0}` is disabled: its row says `enabled = false`")]
    OperatorDisabled(String),

    #[error(transparent)]
    Spawn(#[from] ProcessError),

    #[error(transparent)]
    Mcp(#[from] McpError),

    /// `initialize` succeeded as a wire round-trip but the response was malformed or claimed an incompatible protocol version.
    #[error("plugin initialize rejected: {0}")]
    InitializeRejected(String),

    /// State-machine guard, e.g. stopping a plugin that is `Spawning`.
    #[error("plugin in bad state: {0}")]
    BadState(String),

    /// Treated as a security failure: the supervisor does NOT respawn and reports `Crashed { reason: "auth handshake failed" }`.
    #[error("plugin auth handshake failed: {0}")]
    AuthMismatch(String),

    /// Fires before any process spawn, so no half-spawned plugin is left behind; routes map it to a 4xx.
    #[error(transparent)]
    KernelTooOld(#[from] KernelTooOld),

    /// A trusted plugin declares a template id another running trusted plugin already registers, which would make the binding resolvers ambiguous. Fires before any spawn or token mint.
    #[error(
        "plugin `{plugin_id}` declares template `{template_id}`, which running trusted plugin `{held_by}` already registers"
    )]
    TemplateConflict {
        plugin_id: String,
        /// The id a plugin declared in its manifest's `templates[]` array, naming which kernel template it binds to.
        template_id: String,
        held_by: String,
    },

    /// A non-`app` connector could not be brought up. No child process and no supervisor behind it: terminal until an operator re-enables, and `reason` is the only diagnostic (it lands in `PluginRuntimeStatus::Unavailable`).
    #[error("connector `{plugin_id}` is unavailable: {reason}")]
    ConnectorUnavailable { plugin_id: String, reason: String },

    /// `config_schema.required` keys that neither `user_config` nor a manifest `default` supplies. Raised before any spawn or token mint, but a live `Unavailable` entry is published so `reason` survives to `GET /api/plugins/{id}`.
    #[error("plugin `{plugin_id}` cannot start: {reason}")]
    MissingRequiredConfig { plugin_id: String, reason: String },

    /// The spawn path could not read the stored `user_config` at all. Reading `{}` instead would let manifest defaults silently paper over the operator's real configuration, so the spawn is refused and a live `Unavailable` entry is published first.
    #[error("plugin `{plugin_id}` cannot start: {reason}")]
    ConfigUnreadable { plugin_id: String, reason: String },

    /// The per-id lifecycle lock was held by another operation. Nothing happened (every entry point takes the lock first), so the caller can simply retry; routes render a 409 `plugin_busy`, which clears on its own, unlike `plugin_conflict`.
    #[error("plugin `{0}` is busy: another lifecycle operation holds it")]
    LifecycleBusy(String),

    /// An operation that only makes sense for a process-backed `app` plugin (e.g. token rotation) was attempted on a connector.
    #[error("plugin `{plugin_id}` has kind `{kind}`, which does not support {operation}")]
    UnsupportedForKind {
        plugin_id: String,
        kind: &'static str,
        operation: &'static str,
    },
}
