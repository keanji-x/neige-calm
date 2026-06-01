//! Kernel-as-MCP-server. PR7a (#136) of the Wave-as-Actor cut.
//!
//! ## Architecture
//!
//! The codex daemons spawned for Spec / Worker cards need a write path
//! back into the kernel for `dispatch_request`, `task_completed`,
//! `task_failed` (PR7a), `wave_state.{update,get}` (PR7b), and the
//! `report.*` tools (#229). (The old `wait_for_events` pull tool was
//! removed in the #293 cutover.) The chosen transport is **MCP over a
//! Unix domain socket** so the per-card identity binding is
//! cryptographic (per-card token in `card_mcp_tokens`) and the wire
//! shape is the same JSON-RPC the plugin host already speaks.
//!
//! ```text
//!   codex daemon ── stdio ──> neige-mcp-stdio-shim ── UDS ──> kernel
//!     (initialize with                                          mcp_server
//!      `_meta["dev.neige/auth"].token = $NEIGE_MCP_TOKEN`)        │
//!                                                                  ▼
//!                                                            ToolRegistry
//!                                                                  │
//!                                                                  ▼
//!                                                          write_with_event
//! ```
//!
//! ## Submodules
//!
//!   * [`auth`]      — token mint / hash / constant-time verify.
//!     The DB schema (migration 0010, `card_mcp_tokens`)
//!     pairs with this module.
//!   * [`framing`]   — re-exports JSON-RPC line framing helpers from
//!     `plugin_host::mcp`, keeping the layering one-way.
//!   * [`handshake`] — `initialize` request handler: token lookup +
//!     card-identity binding.
//!   * [`registry`]  — `ToolRegistry` + `AppContext` + per-call identity.
//!   * [`transport`] — UDS listener, per-connection task, JSON-RPC
//!     message pump, `McpServer` + `McpShimConfig`.
//!   * [`tools`]     — the three PR7a emit tools (`calm.dispatch_request`,
//!     `calm.task_completed`, `calm.task_failed`).
//!
//! ## Constructing a registry + server at boot
//!
//! `AppState::new` calls [`build_default_registry`] once, hands the
//! result to [`transport::McpServer::spawn`], and stashes the returned
//! `Arc<McpServer>` on the state for the rest of the process's life.

pub mod auth;
pub mod framing;
pub mod handshake;
pub mod registry;
pub mod tools;
pub mod transport;

pub use registry::{AppContext, CardIdentity, ToolCallIdentity, ToolRegistry};
pub use transport::{McpServer, McpShimConfig};

use std::sync::Arc;

/// Build the default `ToolRegistry` populated with the emit, wave-state,
/// and wave-report tools (see [`tools::register_default_tools`]).
pub fn build_default_registry() -> Arc<ToolRegistry> {
    let mut r = ToolRegistry::new();
    tools::register_default_tools(&mut r);
    Arc::new(r)
}
