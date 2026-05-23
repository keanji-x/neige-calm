//! `neige-mcp-stdio-shim` — bridge between the codex CLI's stdio MCP
//! transport and the neige-calm kernel's per-card UDS MCP server.
//!
//! PR7a (#136) of the Wave-as-Actor cut. The codex CLI's MCP client
//! transport is stdio JSON-RPC; the kernel exposes its MCP server over
//! a Unix domain socket so it can authenticate the caller per-card via
//! `card_mcp_tokens`. This shim is the glue: codex spawns it with
//! `NEIGE_MCP_TOKEN` + `NEIGE_MCP_SOCKET` in the env (set by
//! `spec_card::build_codex_env_map`), the shim opens the socket, injects
//! the token into the first `initialize` frame, then ferries bytes in
//! both directions until either side closes.
//!
//! ## Lifecycle
//!
//! Codex launches this binary fresh on every `mcp_servers.calm` invocation
//! (one process per codex daemon, kept alive by codex for the session).
//! When stdin or the UDS hangs up, the shim exits 0 and codex respawns
//! it if needed. We don't reconnect on UDS failure — codex's own retry
//! path is the right place to handle "kernel went away".
//!
//! ## Token threading (issue #236 followup)
//!
//! Earlier revisions of this shim left token embedding to the codex CLI
//! itself ("codex CLI is responsible for embedding the token in _meta").
//! Vanilla codex CLI 0.132 has no knowledge of `NEIGE_MCP_TOKEN` and
//! does not stamp anything into `params._meta`, so the kernel's
//! `handle_initialize` rejected every connection with `InvalidParams:
//! missing _meta["dev.neige/auth"].token`. The shim now owns that
//! injection: it reads the first line from stdin (line-delimited
//! JSON-RPC per codex's transport convention), parses it as JSON, and
//! if it's an `initialize` request it writes the token into
//! `params._meta["dev.neige/auth"].token` before forwarding to the UDS.
//! Every subsequent frame is byte-pumped unchanged — the parser only
//! runs once per shim process.
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

use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, copy};
use tokio::net::UnixStream;

const ENV_SOCKET: &str = "NEIGE_MCP_SOCKET";
const ENV_TOKEN: &str = "NEIGE_MCP_TOKEN";

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

    // Issue #236 followup — per-card MCP token. Parallel "missing =>
    // fail loudly" treatment to ENV_SOCKET above. Without the token the
    // kernel's `handle_initialize` would reject the connection on its
    // first frame; failing fast here gives a clear "operator
    // misconfigured" stderr instead of an opaque JSON-RPC error written
    // to stdout.
    let token = match env::var(ENV_TOKEN) {
        Ok(v) if !v.is_empty() => v,
        _ => {
            let _ = writeln!(
                io::stderr(),
                "neige-mcp-stdio-shim: missing {ENV_TOKEN} env var; not started by neige-calm?"
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
    let stdin = tokio::io::stdin();
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
    // Issue #236 followup — `stdin_to_sock` no longer hands stdin
    // directly to `tokio::io::copy`. Instead it `read_line`s the first
    // frame, runs it through `maybe_inject_token` to stamp the
    // per-card token into `params._meta["dev.neige/auth"]`, writes
    // that, then falls back to a `copy(BufReader<Stdin> -> sock_wr)`
    // for everything that follows. The parser runs at most once.
    let stdin_to_sock = async move {
        let mut reader = BufReader::new(stdin);
        let mut first_line = String::new();
        match reader.read_line(&mut first_line).await {
            Ok(0) => {
                // EOF before the first frame — codex never wrote
                // anything. Half-close and exit cleanly; the
                // `sock_to_stdout` half will see a peer hangup and
                // wind down too.
                let _ = sock_wr.shutdown().await;
                return;
            }
            Ok(_) => {
                let to_send = maybe_inject_token(&first_line, &token);
                if sock_wr.write_all(to_send.as_bytes()).await.is_err() {
                    return;
                }
                if sock_wr.flush().await.is_err() {
                    return;
                }
            }
            Err(_) => {
                let _ = sock_wr.shutdown().await;
                return;
            }
        }
        // Phase 2: dumb byte-pump for everything after the first
        // frame. `BufReader` may already have additional bytes buffered
        // (codex may batch several frames in one write). Wrapping
        // `copy` around the same `reader` drains those buffered bytes
        // before the next stdin read syscall fires, so we don't
        // accidentally strand a post-initialize notification in the
        // buffer.
        let _ = copy(&mut reader, &mut sock_wr).await;
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

/// Try to parse `line` as a JSON-RPC `initialize` request and inject
/// `params._meta["dev.neige/auth"].token = <token>`. On any non-applicable
/// shape (non-JSON, not an `initialize` request, `dev.neige/auth`
/// already populated, etc.) returns the input unchanged so the kernel
/// sees what codex actually sent.
///
/// Returns an owned `String` because the inject path re-serializes the
/// JSON; the no-op path just clones the input slice. Keeping the return
/// type uniform avoids a `Cow`-shaped API for a once-per-process call.
fn maybe_inject_token(line: &str, token: &str) -> String {
    // Preserve the trailer the input carried (the kernel's `read_line`
    // expects each frame to end with `\n`).
    let (body, trailer) = match line.strip_suffix('\n') {
        Some(rest) => (rest, "\n"),
        None => (line, ""),
    };
    // Some peers emit \r\n. Strip a trailing \r too, then re-emit it.
    let (body, trailer) = match body.strip_suffix('\r') {
        Some(rest) if trailer == "\n" => (rest, "\r\n"),
        _ => (body, trailer),
    };

    let mut value: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            // Not JSON — pass through unchanged. The kernel's framer
            // will surface the malformed frame; nothing for us to do.
            return line.to_string();
        }
    };

    // Only mutate `initialize` requests. Anything else (notifications,
    // tool calls, responses) is forwarded unchanged.
    let is_initialize = value
        .get("method")
        .and_then(|m| m.as_str())
        .is_some_and(|m| m == "initialize");
    if !is_initialize {
        return line.to_string();
    }

    // Walk down to `params._meta["dev.neige/auth"].token`, creating
    // intermediate objects as needed. The codex CLI's `initialize`
    // always carries a `params` object (it's required by the JSON-RPC
    // spec for `initialize`), but we defensively handle missing /
    // wrong-type cases by replacing them with empty objects.
    let params = ensure_object(&mut value, "params");
    let meta = ensure_object_in(params, "_meta");

    // Forward-compat: if `dev.neige/auth.token` is already populated
    // (a future codex revision, or an intermediate proxy), leave it
    // alone. The kernel still verifies via constant-time hash compare,
    // so a stale upstream stamp will be rejected cleanly — silently
    // overwriting it would mask a configuration bug.
    if let Some(existing) = meta.get("dev.neige/auth")
        && existing.get("token").and_then(|t| t.as_str()).is_some()
    {
        let _ = writeln!(
            io::stderr(),
            "neige-mcp-stdio-shim: initialize already carries _meta[\"dev.neige/auth\"].token; leaving untouched"
        );
        // Re-serialize the unchanged-shape frame (parsed fine, so the
        // round-trip is semantically a no-op) and re-emit the trailer.
        return format!("{value}{trailer}");
    }

    // Insert / overwrite our auth slot.
    meta.insert(
        "dev.neige/auth".to_string(),
        serde_json::json!({ "token": token }),
    );

    format!("{value}{trailer}")
}

/// Walk into `value[key]`, replacing the slot with an empty object if
/// it's missing or not an object. Returns a `&mut serde_json::Map` so
/// the caller can chain another `ensure_object_in` or `insert`.
fn ensure_object<'a>(
    value: &'a mut serde_json::Value,
    key: &str,
) -> &'a mut serde_json::Map<String, serde_json::Value> {
    if !value.is_object() {
        *value = serde_json::Value::Object(serde_json::Map::new());
    }
    let map = value.as_object_mut().expect("just-set object");
    ensure_object_in(map, key)
}

/// Same as `ensure_object` but operates on an existing `&mut Map`
/// (avoids the outer "make sure root is an object" step). Borrow-
/// checker-friendlier when chaining: `params -> _meta` doesn't need a
/// second mutable borrow of the root `Value`.
fn ensure_object_in<'a>(
    map: &'a mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> &'a mut serde_json::Map<String, serde_json::Value> {
    if !map.get(key).is_some_and(|v| v.is_object()) {
        map.insert(
            key.to_string(),
            serde_json::Value::Object(serde_json::Map::new()),
        );
    }
    map.get_mut(key)
        .expect("just-inserted")
        .as_object_mut()
        .expect("just-set object")
}

#[cfg(test)]
mod tests {
    //! Issue #236 followup — unit tests for the `initialize` token
    //! injector. Pure-function tests (no UDS, no process spawn); the
    //! integration tests in `tests/stdio_shim.rs` cover the wired-up
    //! shape end-to-end.

    use super::maybe_inject_token;
    use serde_json::Value;

    /// Helper: parse `injected` back as JSON and pluck out the
    /// auth-slot token.
    fn extract_token(injected: &str) -> Option<String> {
        let v: Value = serde_json::from_str(injected.trim_end()).ok()?;
        v.get("params")?
            .get("_meta")?
            .get("dev.neige/auth")?
            .get("token")?
            .as_str()
            .map(|s| s.to_string())
    }

    #[test]
    fn initialize_without_meta_gets_token_injected() {
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#;
        let out = maybe_inject_token(line, "test-token-abc");
        assert_eq!(extract_token(&out).as_deref(), Some("test-token-abc"));
    }

    #[test]
    fn initialize_with_existing_meta_block_preserves_siblings() {
        // The kernel's `handle_initialize` reads only
        // `_meta["dev.neige/auth"].token`, but MCP tooling may set
        // other `_meta` siblings (e.g. `progress-token` for
        // long-running ops). The shim must merge — not clobber.
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"_meta":{"progress-token":"abc"},"protocolVersion":"2024-11-05"}}"#;
        let out = maybe_inject_token(line, "tok-xyz");
        let v: Value = serde_json::from_str(out.trim_end()).expect("re-parse");
        // Auth slot landed:
        let token = v["params"]["_meta"]["dev.neige/auth"]["token"]
            .as_str()
            .expect("token present");
        assert_eq!(token, "tok-xyz");
        // Pre-existing sibling preserved:
        let progress = v["params"]["_meta"]["progress-token"]
            .as_str()
            .expect("progress-token preserved");
        assert_eq!(progress, "abc");
    }

    #[test]
    fn initialize_already_stamped_with_auth_is_left_untouched() {
        // Forward-compat path: if a future codex revision (or an
        // intermediate proxy) already stamped `_meta["dev.neige/auth"]
        // .token`, the shim must not clobber it. The kernel verifies
        // the token via constant-time hash compare regardless of who
        // stamped the slot, so a stale upstream stamp will be cleanly
        // rejected — but silently overwriting it would mask a real
        // configuration bug.
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"_meta":{"dev.neige/auth":{"token":"upstream-stamp"}}}}"#;
        let out = maybe_inject_token(line, "shim-stamp");
        assert_eq!(extract_token(&out).as_deref(), Some("upstream-stamp"));
    }

    #[test]
    fn non_initialize_frames_pass_through_unchanged() {
        let line = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let out = maybe_inject_token(line, "tok-xyz");
        assert_eq!(out, line);
        // No auth slot injected:
        let v: Value = serde_json::from_str(out.trim_end()).expect("re-parse");
        assert!(v["params"]["_meta"].is_null());
    }

    #[test]
    fn malformed_json_passes_through_unchanged() {
        // A non-JSON line shouldn't crash the shim or get rewritten —
        // the kernel's framer will surface the malformed frame.
        let line = "this is not JSON\n";
        let out = maybe_inject_token(line, "tok-xyz");
        assert_eq!(out, line);
    }

    #[test]
    fn newline_trailer_is_preserved() {
        // The kernel's `read_line` expects each frame to terminate
        // with `\n`. The injector must re-emit the trailer the input
        // carried.
        let line = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n";
        let out = maybe_inject_token(line, "tok");
        assert!(
            out.ends_with('\n'),
            "expected trailing newline; got {out:?}"
        );
        // And the body still parses + carries the token:
        assert_eq!(extract_token(&out).as_deref(), Some("tok"));
    }

    #[test]
    fn crlf_trailer_is_preserved() {
        // Defensive: some MCP clients on Windows-y stacks emit \r\n.
        // Codex itself uses \n on POSIX, but the shim should survive
        // either shape.
        let line = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\r\n";
        let out = maybe_inject_token(line, "tok");
        assert!(out.ends_with("\r\n"), "expected trailing CRLF; got {out:?}");
    }

    #[test]
    fn params_with_non_object_is_replaced() {
        // Defensive: malformed `initialize` that sets `params` to a
        // non-object (null, array, etc.). JSON-RPC says params for
        // `initialize` must be an object; the shim treats wrong-type
        // as missing and stamps the token slot.
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":null}"#;
        let out = maybe_inject_token(line, "tok");
        // Token landed:
        assert_eq!(extract_token(&out).as_deref(), Some("tok"));
    }
}
