//! Source-agnostic event types consumed by the rest of neige.
//!
//! Historically this module also held a parser for Claude Code's
//! `--output-format=stream-json` NDJSON. That parser is gone: chat-mode
//! events are now produced by the Node sidecar runner
//! (`runners/neige-chat-runner`) which uses
//! `@anthropic-ai/claude-agent-sdk` and emits already-serialized
//! [`NeigeEvent`] JSON on stdout — the daemon forwards lines opaquely.
//!
//! Only the unified event types remain, since [`NeigeEvent`] is also
//! synthesized in-process by `neige-server` (e.g. for `Passthrough`
//! envelopes from MCP tools), so the type contract is shared across the
//! workspace.

pub mod unified;

pub use unified::{ContentBlock, McpServerInfo, NeigeEvent, PluginInfo, ToolResultContent};
