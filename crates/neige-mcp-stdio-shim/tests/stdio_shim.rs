//! PR7a.1 (#136 followup) — integration tests for `neige-mcp-stdio-shim`.
//!
//! The shim is a tiny byte-pump: stdin -> UDS write half, UDS read half ->
//! stdout. These tests boot a stub UDS server, spawn the shim binary
//! with `NEIGE_MCP_SOCKET` pointed at the stub, then drive bytes in
//! each direction and assert they land on the other side.
//!
//! Test budget: 5 seconds per case. The shim is line-pumped JSON-RPC in
//! production but the byte copy is content-agnostic — we exercise both
//! directions with simple line-delimited payloads.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::process::Command;
use tokio::time::timeout;

const SHIM_BIN: &str = env!("CARGO_BIN_EXE_neige-mcp-stdio-shim");
const TEST_BUDGET: Duration = Duration::from_secs(5);

/// Spawn a UDS listener at `socket_path`. Returns the listener; the
/// caller `accept()`s once when the shim connects.
fn listen(socket_path: &std::path::Path) -> UnixListener {
    UnixListener::bind(socket_path).expect("bind stub UDS")
}

#[tokio::test]
async fn stdin_to_socket_forwards_bytes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket_path: PathBuf = tmp.path().join("kernel.sock");
    let listener = listen(&socket_path);

    // Spawn the shim with the env var pointing at our stub socket.
    let mut child = Command::new(SHIM_BIN)
        .env("NEIGE_MCP_SOCKET", &socket_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn shim");

    // Accept the shim's connection.
    let (server_stream, _addr) = timeout(TEST_BUDGET, listener.accept())
        .await
        .expect("shim connected within budget")
        .expect("accept ok");
    let (server_rd, server_wr) = server_stream.into_split();
    let mut server_reader = BufReader::new(server_rd);

    // Write a line to the shim's stdin. The shim should forward it
    // verbatim to the socket.
    let mut child_stdin = child.stdin.take().expect("stdin piped");
    child_stdin
        .write_all(b"hello-from-stdin\n")
        .await
        .expect("write stdin");
    child_stdin.flush().await.expect("flush stdin");

    let mut received = String::new();
    timeout(TEST_BUDGET, server_reader.read_line(&mut received))
        .await
        .expect("server read within budget")
        .expect("read line ok");
    assert_eq!(received, "hello-from-stdin\n");

    // Cleanup. The shim now waits for BOTH directions to drain (the
    // PR #221 fix) before exiting — so closing stdin alone isn't
    // enough. Drop the server-side write half too so the shim's
    // `sock_to_stdout` sees EOF on its read half. With both ends
    // closed the shim exits and we reap it.
    drop(child_stdin);
    drop(server_wr);
    let _ = timeout(TEST_BUDGET, child.wait()).await;
}

#[tokio::test]
async fn socket_to_stdout_forwards_bytes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket_path: PathBuf = tmp.path().join("kernel.sock");
    let listener = listen(&socket_path);

    let mut child = Command::new(SHIM_BIN)
        .env("NEIGE_MCP_SOCKET", &socket_path)
        // `Stdio::null()` for stdin is the natural "no inbound bytes
        // from codex" shape for this direction-isolated test. With
        // the PR #221 fix, the shim's `join!` waits for the socket
        // direction even after the stdin direction EOFs immediately
        // on null — so we no longer race the post-accept socket
        // write into a half-closed shim.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn shim");

    let (server_stream, _addr) = timeout(TEST_BUDGET, listener.accept())
        .await
        .expect("shim connected within budget")
        .expect("accept ok");
    let (_server_rd, mut server_wr) = server_stream.into_split();

    // Write a line on the socket. The shim should pipe it back out
    // through its stdout.
    server_wr
        .write_all(b"hello-from-socket\n")
        .await
        .expect("write socket");
    server_wr.flush().await.expect("flush socket");

    // Read one line from the shim's stdout. We read line-by-line
    // (rather than `read_to_end`) so the assert lands as soon as the
    // shim flushes the byte forward, no matter when the process
    // actually exits.
    let child_stdout = child.stdout.take().expect("stdout piped");
    let mut reader = BufReader::new(child_stdout);
    let mut line = String::new();
    timeout(TEST_BUDGET, reader.read_line(&mut line))
        .await
        .expect("stdout read within budget")
        .expect("read_line ok");
    assert_eq!(line, "hello-from-socket\n");

    // Drop the server-side write half so the shim sees EOF on the
    // socket; combined with the `Stdio::null()` stdin (also EOF), the
    // shim exits and `child.wait()` returns.
    drop(server_wr);
    let _ = timeout(TEST_BUDGET, child.wait()).await;
}

/// Regression test for the PR #221 race fix.
///
/// Before the fix, the shim's `tokio::select!` would exit as soon as
/// EITHER direction completed. With `Stdio::null()` on stdin, the
/// `stdin_to_sock` half resolved within microseconds of spawn (null
/// EOF → 0-byte copy → shutdown). The shim would then drop the socket
/// owner futures, closing the connection — and any kernel write
/// arriving after that point would EPIPE.
///
/// This test simulates the production race directly: spawn the shim
/// with `Stdio::null()` stdin (so `stdin_to_sock` resolves immediately),
/// wait long enough for the buggy version to have exited, THEN write
/// a frame on the socket and assert it lands on the shim's stdout.
/// Under the buggy `select!` shape this fails with EPIPE on the
/// socket write or EOF on the stdout read; under the fixed `join!`
/// shape it passes deterministically.
#[tokio::test]
async fn shim_stays_alive_after_stdin_eof_until_socket_closes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket_path: PathBuf = tmp.path().join("kernel.sock");
    let listener = listen(&socket_path);

    let mut child = Command::new(SHIM_BIN)
        .env("NEIGE_MCP_SOCKET", &socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn shim");

    let (server_stream, _addr) = timeout(TEST_BUDGET, listener.accept())
        .await
        .expect("shim connected within budget")
        .expect("accept ok");
    let (_server_rd, mut server_wr) = server_stream.into_split();

    // Sleep long enough that the buggy `select!`-shaped shim would
    // have already noticed the null stdin EOF, called shutdown on
    // its socket write half, and exited. 100 ms is overkill for the
    // microsecond-scale race window but gives CI under load room.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Now write a frame on the socket. Under the buggy shape the
    // shim is gone, the kernel has lost its peer, and the write
    // either returns EPIPE here or strands the bytes on the
    // closed socket.
    server_wr
        .write_all(b"late-frame-from-socket\n")
        .await
        .expect("socket write after stdin EOF (shim must still be alive)");
    server_wr
        .flush()
        .await
        .expect("socket flush after stdin EOF");

    let child_stdout = child.stdout.take().expect("stdout piped");
    let mut reader = BufReader::new(child_stdout);
    let mut line = String::new();
    timeout(TEST_BUDGET, reader.read_line(&mut line))
        .await
        .expect("stdout read within budget")
        .expect("read_line ok");
    assert_eq!(
        line, "late-frame-from-socket\n",
        "shim must forward socket frames that arrive after stdin EOF"
    );

    // Cleanup. Closing the socket write half lets the shim's
    // `sock_to_stdout` see EOF; combined with the already-EOF'd
    // stdin, the shim exits.
    drop(server_wr);
    let _ = timeout(TEST_BUDGET, child.wait()).await;
}

#[tokio::test]
async fn missing_socket_env_exits_nonzero() {
    // No `NEIGE_MCP_SOCKET` env → shim exits 2 with a stderr message.
    // The production binary is launched by codex, which the kernel
    // controls; this test pins the "operator misconfigured the env"
    // error path so a future refactor doesn't silently swallow it.
    let child = Command::new(SHIM_BIN)
        .env_remove("NEIGE_MCP_SOCKET")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn shim");
    let out = timeout(TEST_BUDGET, child.wait_with_output())
        .await
        .expect("shim exited within budget")
        .expect("wait ok");
    assert!(
        !out.status.success(),
        "shim must fail without NEIGE_MCP_SOCKET; got status {:?}",
        out.status
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("NEIGE_MCP_SOCKET"),
        "shim stderr should mention the missing env var; got: {stderr}"
    );
}
