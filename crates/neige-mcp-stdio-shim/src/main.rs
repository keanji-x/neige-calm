//! `neige-mcp-stdio-shim` — bridge between the codex CLI's stdio MCP
//! transport and the neige-calm kernel's per-card UDS MCP server.
//!
//! PR7a (#136) of the Wave-as-Actor cut. The codex CLI's MCP client
//! transport is stdio JSON-RPC; the kernel exposes its MCP server over
//! a Unix domain socket so it can authenticate the caller per-card via
//! `card_mcp_tokens`. This shim is the glue: codex spawns it with
//! `NEIGE_MCP_TOKEN` + `NEIGE_MCP_SOCKET` in the env (set by
//! `spec_card::build_codex_env_map`), the shim opens the socket, then
//! ferries bytes in both directions until either side closes.
//!
//! ## Lifecycle
//!
//! Codex launches this binary fresh on every `mcp_servers.calm` invocation
//! (one process per codex daemon, kept alive by codex for the session).
//! When stdin or the UDS hangs up, the shim exits 0 and codex respawns
//! it if needed. We don't reconnect on UDS failure — codex's own retry
//! path is the right place to handle "kernel went away".
//!
//! ## Token threading
//!
//! The shim does NOT itself embed the token in `initialize.params._meta`.
//! That's the codex CLI's job (it owns the `initialize` request shape).
//! We surface `NEIGE_MCP_TOKEN` *into the codex daemon's env*; codex
//! then includes it in the handshake. The shim's only auth-related
//! responsibility is choosing the right socket path (per-instance
//! `NEIGE_MCP_SOCKET`) and refusing to start if it's missing.
//!
//! ## Trust model
//!
//! No additional auth on the UDS itself — file-mode 0600 on the socket
//! at the kernel side restricts access to the same uid. The token is
//! the per-card identity binding once the connection is up, and the
//! kernel rejects any `initialize` that doesn't echo back the expected
//! `dev.neige/auth.expected_echo` from a known card's token row.

use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

use tokio::io::{AsyncReadExt, AsyncWriteExt, copy};
use tokio::net::UnixStream;

const ENV_SOCKET: &str = "NEIGE_MCP_SOCKET";

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    // Resolve the UDS path from the env. Codex sets this from the
    // `[mcp_servers.calm].env` block the kernel writes into the per-card
    // config.toml — see `spec_card::build_codex_config_toml_with_prompt`.
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

    // Connect. A missing/bad socket is the only synchronous failure mode
    // worth surfacing — once the stream is up, the bidirectional copy
    // below handles all the I/O.
    let stream = match UnixStream::connect(&socket).await {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(io::stderr(), "neige-mcp-stdio-shim: connect {socket}: {e}");
            return ExitCode::from(3);
        }
    };

    // Split for true full-duplex copy. `into_split` lets each direction
    // own its half independently — needed because the two `copy` calls
    // run concurrently below.
    let (mut sock_rd, mut sock_wr) = stream.into_split();

    // tokio's `stdin()` / `stdout()` are owned handles backed by the
    // process's std fds. They wrap blocking syscalls in a worker pool
    // — but on `current_thread` runtime that pool is the same single
    // thread, which is fine for a shim whose entire job is byte copy.
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    // Two concurrent copy futures: stdin -> socket and socket -> stdout.
    // `tokio::join!` waits for BOTH directions to terminate — exiting
    // as soon as one finishes (the previous `select!` shape) drops
    // the still-pending direction mid-flight, which closes the FDs
    // it owned and races the peer's last write into EPIPE. The two
    // sides terminate independently:
    //   * `stdin_to_sock` ends when codex closes our stdin (clean
    //     handshake teardown) or stdin errors.
    //   * `sock_to_stdout` ends when the kernel hangs up its UDS
    //     write half (clean per-card MCP shutdown) or the socket
    //     errors.
    // Each side half-closes its write end after its copy completes so
    // the peer sees EOF and reciprocates. Holding both halves until
    // both directions have drained is the only way to guarantee no
    // last-byte loss across a half-closed handover (e.g. codex closes
    // stdin before the kernel flushes its final response).
    //
    // We use `tokio::io::copy` for the heavy lifting on the stdin
    // side — it does an internal 8 KiB buffer + read/write loop with
    // periodic flushes.
    let stdin_to_sock = async move {
        let _ = copy(&mut stdin, &mut sock_wr).await;
        // Half-close so the kernel reader sees EOF and drops its
        // per-connection task. We ignore the result — the kernel may
        // already be gone.
        let _ = sock_wr.shutdown().await;
    };
    let sock_to_stdout = async move {
        // Manual loop instead of `tokio::io::copy` so we flush after
        // every chunk — JSON-RPC frames are line-delimited and codex's
        // reader is blocking on a newline. An unflushed write would
        // strand a frame in the libc stdout buffer until the next
        // copy yields a full buffer.
        let mut buf = [0u8; 8192];
        loop {
            match sock_rd.read(&mut buf).await {
                Ok(0) => break, // kernel hung up
                Ok(n) => {
                    if stdout.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    if stdout.flush().await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    };

    tokio::join!(stdin_to_sock, sock_to_stdout);

    ExitCode::SUCCESS
}
