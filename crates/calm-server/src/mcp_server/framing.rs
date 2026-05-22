//! JSON-RPC framing for the kernel-as-MCP-server transport.
//!
//! PR7a (#136) — this is a thin shim over the line-delimited JSON helpers
//! the plugin host already uses (`crate::plugin_host::mcp::parse_frame`
//! and friends). The wire format is identical; only the direction
//! flips: in `plugin_host`, the kernel is the *client* talking to a
//! plugin server. Here, the kernel is the *server* and the codex daemon
//! (via `neige-mcp-stdio-shim`) is the client.
//!
//! Centralizing the framing helpers here:
//!
//!   * makes `mcp_server/transport.rs` and `mcp_server/handshake.rs`
//!     callers depend on this module rather than reaching across into
//!     `plugin_host::*` — keeps the layering one-directional;
//!   * gives PR7b/PR8 a single seat to add server-specific frame
//!     helpers (e.g. canceled-notification synthesizers) without
//!     bloating the plugin-host module.
//!
//! No new parsing logic. Re-export only.

pub(crate) use crate::plugin_host::mcp::{
    Frame, RequestId, RpcError, build_error_response_frame, build_ok_response_frame, parse_frame,
};
