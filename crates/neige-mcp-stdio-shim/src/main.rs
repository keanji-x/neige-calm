//! `neige-mcp-stdio-shim` — bridge between the codex CLI's stdio MCP
//! transport and the kernel's per-card UDS MCP server.
//!
//! Exit codes: 0 clean; 2 env missing; 3 first connection failed; 4 the kernel
//! rejected a (replayed) `initialize`; 5 an outage outlived its budget.

mod budget;
mod frames;
mod pump;

use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

const ENV_SOCKET: &str = "NEIGE_MCP_SOCKET";
const ENV_TOKEN: &str = "NEIGE_MCP_TOKEN";
const ENV_DAEMON_TOKEN: &str = "NEIGE_MCP_DAEMON_TOKEN";

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    if env::args().nth(1).as_deref() == Some("--version") {
        println!("neige-mcp-stdio-shim {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    let socket = match env::var(ENV_SOCKET) {
        Ok(v) if !v.is_empty() => v,
        _ => {
            // Stderr only — stdout is the codex MCP wire.
            let _ = writeln!(
                io::stderr(),
                "neige-mcp-stdio-shim: missing {ENV_SOCKET} env var; not started by neige-calm?"
            );
            return ExitCode::from(2);
        }
    };

    let token = match resolve_token() {
        Ok(v) if !v.is_empty() => v,
        _ => {
            let _ = writeln!(
                io::stderr(),
                "neige-mcp-stdio-shim: missing {ENV_DAEMON_TOKEN} or {ENV_TOKEN} env var; not started by neige-calm?"
            );
            return ExitCode::from(2);
        }
    };

    let code = match pump::run(socket, token).await {
        pump::Exit::Clean => 0,
        pump::Exit::InitialConnectFailed => 3,
        pump::Exit::InitializeRejected => 4,
        pump::Exit::BudgetExhausted => 5,
    };
    // A blocking stdin read may be parked on a runtime worker thread and dropping
    // the runtime would wait for it, so leave without unwinding.
    std::process::exit(code)
}

fn resolve_token() -> Result<String, env::VarError> {
    match env::var(ENV_DAEMON_TOKEN) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => env::var(ENV_TOKEN),
    }
}
