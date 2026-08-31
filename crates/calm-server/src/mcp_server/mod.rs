//! Kernel MCP server.
//!
//! The codex daemons spawned for Spec / Worker cards need a write path
//! back into the kernel for dispatch, task outcomes, verdicts, and report
//! updates. The transport is MCP over a Unix domain socket so per-card identity is
//! cryptographic (per-card token in `card_mcp_tokens`) and the wire
//! shape is the same JSON-RPC the plugin host already speaks.
//!
//! ```text
//!   codex daemon ── stdio ──> neige-mcp-stdio-shim ── UDS ──> kernel
//!     (initialize with                                          mcp_server
//!      `_meta["dev.neige/auth"].token` from per-card or daemon env) │
//!                                                                  ▼
//!                                                            ToolRegistry
//!                                                                  │
//!                                                                  ▼
//!                                                          write_with_event
//! ```

pub mod auth;
pub mod framing;
pub mod handshake;
pub mod registry;
pub(crate) mod tool_visibility;
pub mod tools;
pub mod transport;
pub mod wiring;

pub use registry::{AppContext, CardIdentity, ConnectionIdentity, ToolCallIdentity, ToolRegistry};
pub use transport::{McpServer, McpShimConfig};

use std::sync::Arc;

pub fn build_default_registry() -> Arc<ToolRegistry> {
    let mut r = ToolRegistry::new();
    tools::register_default_tools(&mut r);
    Arc::new(r)
}
