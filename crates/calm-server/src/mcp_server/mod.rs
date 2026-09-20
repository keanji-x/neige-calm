//! Kernel MCP server: the codex daemons' write path back into the kernel, MCP over a Unix
//! domain socket with per-card token identity.

pub mod auth;
pub mod framing;
pub mod handshake;
pub mod registry;
pub mod result;
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
