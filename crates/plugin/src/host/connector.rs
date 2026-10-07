//! Server composition of concrete plugin transports and compiled backends.
pub use crate::connector::*;
use crate::{cli_query::CliQueryRuntime, http_mcp::HttpMcpClient, mcp::McpClient};
use std::sync::Arc;

/// What a running plugin/connector talks to; `Clone` is cheap by construction (every payload is behind an `Arc`).
#[derive(Clone)]
pub enum ConnectorClient {
    /// `kind: app` — the stdio child process.
    Stdio(Arc<McpClient>),
    /// Trusted compiled implementation; no process or IPC.
    Builtin(&'static dyn super::ports::BuiltinBackend),
    /// `kind: mcp-http` — remote streamable-HTTP MCP server.
    Http(Arc<HttpMcpClient>),
    /// `kind: cli-query` — a pinned local query binary; no child is supervised, each `tools/call` forks a fresh short-lived process.
    Cli(Arc<CliQueryRuntime>),
}

impl std::fmt::Debug for ConnectorClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No payloads: the HTTP variant holds an API key and the CLI variant holds a secret environment.
        f.write_str(match self {
            Self::Stdio(_) => "ConnectorClient::Stdio",
            Self::Builtin(_) => "ConnectorClient::Builtin",
            Self::Http(_) => "ConnectorClient::Http",
            Self::Cli(_) => "ConnectorClient::Cli",
        })
    }
}

impl ConnectorClient {
    /// Short wire-ish label for logs and error messages.
    pub fn variant_name(&self) -> &'static str {
        match self {
            Self::Stdio(_) => "stdio",
            Self::Builtin(_) => "builtin",
            Self::Http(_) => "mcp-http",
            Self::Cli(_) => "cli-query",
        }
    }

    /// The stdio client, or `None` for connectors; used by callers that genuinely require a `kind: app` plugin rather than widening.
    pub fn as_stdio(&self) -> Option<&Arc<McpClient>> {
        match self {
            Self::Stdio(c) => Some(c),
            _ => None,
        }
    }
}
