//! `neige-mcp-stdio-shim` — bridge between the codex CLI's stdio MCP
//! transport and the neige-calm kernel's per-card UDS MCP server.
//!
//! PR7a (#136) of the Track-as-Actor cut. The codex CLI's MCP client
//! transport is stdio JSON-RPC; the kernel exposes its MCP server over
//! a Unix domain socket so it can authenticate the caller per-card via
//! `card_mcp_tokens`. This shim is the glue: codex spawns it with
//! `NEIGE_MCP_DAEMON_TOKEN` + `NEIGE_MCP_SOCKET` in the env (set by
//! `planner_card::build_codex_env_map`), the shim opens the socket, injects
//! the token into the `initialize` frame, then ferries line-delimited
//! frames in both directions (`pump.rs`; reconnect timing in `budget.rs`).
//!
//! ## Lifecycle
//!
//! Codex keeps one shim per thread for the thread's whole life and does
//! not respawn it, so when the kernel goes away while stdin is still
//! open the shim reconnects and replays `initialize` itself (#1699).
//!
//! ## Token threading (issue #236 followup)
//!
//! Earlier revisions of this shim left token embedding to the codex CLI
//! itself ("codex CLI is responsible for embedding the token in _meta").
//! Vanilla codex CLI 0.132 has no knowledge of `NEIGE_MCP_TOKEN` and
//! does not stamp anything into `params._meta`, so the kernel's
//! `handle_initialize` rejected every connection with `InvalidParams:
//! missing _meta["dev.neige/auth"].token`. The shim now owns that
//! injection: it classifies every line from stdin (line-delimited
//! JSON-RPC per codex's transport convention), and when the line is an
//! `initialize` request it writes the token into
//! `params._meta["dev.neige/auth"].token` before forwarding to the UDS
//! (`frames.rs`). Every other frame is forwarded byte-for-byte.
//!
//! Forward-compat: if `_meta["dev.neige/auth"].token` is already
//! populated on the inbound frame (e.g. a future codex revision starts
//! stamping it natively, or something else upstream wires it through),
//! the shim leaves it alone and emits a stderr note. The kernel
//! verifies the token via constant-time hash compare regardless of
//! which side stamped the slot, so a stale upstream stamp gets cleanly
//! rejected — silently overwriting it would mask a real configuration
//! bug.
//!
//! ## Trust model
//!
//! No additional auth on the UDS itself — file-mode 0600 on the socket
//! at the kernel side restricts access to the same uid. The token is
//! the per-card identity binding once the connection is up, and the
//! kernel rejects any `initialize` that doesn't carry a known card's
//! per-card token at `_meta["dev.neige/auth"].token`.
//!
//! ## Exit codes
//!
//! 0 clean; 2 env missing; 3 first connection failed within
//! [`budget::INITIAL_CONNECT_BUDGET`]; 4 the kernel rejected a
//! (replayed) `initialize`; 5 an outage outlived its budget
//! ([`budget::RECONNECT_BUDGET`], or [`budget::INITIAL_CONNECT_BUDGET`]
//! while codex has no `initialize` response yet).

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

    // Resolve the UDS path from the env. Codex sets this from the
    // `[mcp_servers.calm].env` block the kernel writes into the per-card
    // config.toml — see `planner_card::build_role_codex_config_toml`.
    let socket = match env::var(ENV_SOCKET) {
        Ok(v) if !v.is_empty() => v,
        _ => {
            // Stderr only — stdout is the codex MCP wire. Anything we
            // print there would be parsed as a JSON-RPC frame and
            // crash the daemon's reader.
            let _ = writeln!(
                io::stderr(),
                "neige-mcp-stdio-shim: missing {ENV_SOCKET} env var; not started by neige-calm?"
            );
            return ExitCode::from(2);
        }
    };

    // Issue #236 followup — per-card MCP token. Parallel "missing =>
    // fail loudly" treatment to ENV_SOCKET above. Without the token the
    // kernel's `handle_initialize` would reject the connection on its
    // first frame; failing fast here gives a clear "operator
    // misconfigured" stderr instead of an opaque JSON-RPC error written
    // to stdout.
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
    // Exits 3, 4 and 5 happen with codex alive and stdin still open; a
    // blocking stdin read may be parked on a runtime worker thread, and
    // dropping the runtime would wait for it, so leave without
    // unwinding. Every stdout write was flushed by the pump before it
    // returned.
    std::process::exit(code)
}

fn resolve_token() -> Result<String, env::VarError> {
    match env::var(ENV_DAEMON_TOKEN) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => env::var(ENV_TOKEN),
    }
}
