//! `neige/cli` (#1801): the kernel parses, runs and renders every `neige` command. The forwarder
//! sends argv and writes back `{stdout, stderr, exit}` verbatim (docs/architecture/1801-kernel-served-cli.md).

mod commands;
pub mod help;
pub mod render;

use std::sync::Arc;

use serde_json::{Value, json};

use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{AppContext, ConnectionIdentity, ToolRegistry};
use crate::mcp_server::transport::call_registered_tool;

/// The only exit codes the kernel emits; the forwarder alone uses 2, 3, 5 and 141, and 4 for its transport failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliExit {
    Success,
    /// argv shape, unknown command or a missing `--force`.
    Usage,
    /// The tool refused or its result could not be rendered.
    Failed,
}

impl CliExit {
    pub const ALL: [Self; 3] = [Self::Success, Self::Usage, Self::Failed];

    pub fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Usage => 1,
            Self::Failed => 4,
        }
    }
}

struct Output {
    stdout: String,
    stderr: String,
    exit: CliExit,
}

impl Output {
    fn success(stdout: String) -> Self {
        Self {
            stdout,
            stderr: String::new(),
            exit: CliExit::Success,
        }
    }

    /// `neige: <message>`, or `{"error":{message,detail}}` under `--json`.
    fn error(exit: CliExit, json: bool, message: String, detail: Value) -> Self {
        let stderr = if json {
            format!(
                "{}\n",
                json!({ "error": { "message": message, "detail": detail } })
            )
        } else {
            format!("neige: {message}\n")
        };
        Self {
            stdout: String::new(),
            stderr,
            exit,
        }
    }

    fn usage(message: String, json: bool, command: Option<&str>) -> Self {
        let detail = json!({ "kind": "usage", "usage": help::usage_line(command) });
        Self::error(CliExit::Usage, json, message, detail)
    }
}

/// Only a card-bound connection is served: `neige` presents the per-card `NEIGE_MCP_TOKEN`.
pub(crate) async fn serve(
    ctx: &Arc<AppContext>,
    registry: &Arc<ToolRegistry>,
    connection_identity: &ConnectionIdentity,
    params: Value,
) -> Result<Value, RpcError> {
    if !matches!(connection_identity, ConnectionIdentity::CardBound(_)) {
        return Err(RpcError::custom(
            RpcError::INVALID_REQUEST,
            "neige/cli: requires a card-bound connection (per-card NEIGE_MCP_TOKEN)",
        ));
    }
    let argv: Vec<String> = params
        .get("argv")
        .cloned()
        .and_then(|argv| serde_json::from_value(argv).ok())
        .ok_or_else(|| RpcError::invalid_params("neige/cli: `argv` must be an array of strings"))?;
    let out = run(ctx, registry, connection_identity, &argv).await;
    Ok(json!({ "stdout": out.stdout, "stderr": out.stderr, "exit": out.exit.code() }))
}

async fn run(
    ctx: &Arc<AppContext>,
    registry: &ToolRegistry,
    connection_identity: &ConnectionIdentity,
    argv: &[String],
) -> Output {
    if let Some(request) = help::request(argv) {
        return match help::render(request) {
            Some(text) => Output::success(text),
            None => Output::usage(
                help::unknown_command_message(
                    request.command().expect("only command help can be unknown"),
                ),
                argv.iter().any(|arg| arg == "--json"),
                None,
            ),
        };
    }
    let parsed = match commands::parse(argv) {
        Ok(parsed) => parsed,
        Err(usage) => return Output::usage(usage.message, usage.json, usage.command),
    };
    let tool = parsed.tool;
    match call_registered_tool(ctx, registry, connection_identity, None, tool, parsed.args).await {
        Err(error) => Output::error(
            CliExit::Failed,
            parsed.json,
            format!("{tool}: {} (code {})", error.message, error.code),
            json!({ "kind": "rpc", "method": tool, "rpc_error": error }),
        ),
        Ok(result) => {
            match render::render(parsed.render, tool, parsed.json, &result.into_structured()) {
                Ok(stdout) => Output::success(stdout),
                Err(error) => {
                    Output::error(CliExit::Failed, parsed.json, error.message, error.detail)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CliExit;
    use std::collections::BTreeSet;

    /// Exit codes only the forwarder emits (docs/architecture/1801-kernel-served-cli.md §3.3).
    const FORWARDER_RESERVED: [u8; 4] = [2, 3, 5, 141];

    #[test]
    fn kernel_exit_codes_are_exactly_0_1_4() {
        let kernel: BTreeSet<u8> = CliExit::ALL.iter().map(|exit| exit.code()).collect();
        assert_eq!(kernel, BTreeSet::from([0, 1, 4]));
        assert!(FORWARDER_RESERVED.iter().all(|code| !kernel.contains(code)));
    }
}
