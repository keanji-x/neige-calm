//! Integration tests for `neige-mcp-stdio-shim`: a stub UDS server, the spawned shim
//! binary, and bytes driven in each direction.

#![cfg(unix)]

mod common;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::process::Command;
use tokio::time::timeout;

const SHIM_BIN: &str = env!("CARGO_BIN_EXE_neige-mcp-stdio-shim");
const TEST_BUDGET: Duration = Duration::from_secs(5);

/// Spawn a UDS listener at `socket_path`.
fn listen(socket_path: &std::path::Path) -> UnixListener {
    calm_test_sockets::assert_fits(socket_path);
    UnixListener::bind(socket_path).unwrap_or_else(|e| {
        panic!(
            "bind stub UDS at {} ({} bytes): {e}",
            socket_path.display(),
            socket_path.as_os_str().as_encoded_bytes().len()
        )
    })
}

#[tokio::test]
async fn stdin_to_socket_forwards_bytes() {
    let tmp = calm_test_sockets::socket_dir("shim");
    let socket_path: PathBuf = calm_test_sockets::socket_path(tmp.path(), "kernel.sock");
    let listener = listen(&socket_path);

    let mut child = Command::new(SHIM_BIN)
        .env("NEIGE_MCP_SOCKET", &socket_path)
        .env_remove("NEIGE_MCP_DAEMON_TOKEN")
        .env("NEIGE_MCP_TOKEN", "test-byte-pump-token")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn shim");

    let (server_stream, _addr) = timeout(TEST_BUDGET, listener.accept())
        .await
        .expect("shim connected within budget")
        .expect("accept ok");
    let (server_rd, server_wr) = server_stream.into_split();
    let mut server_reader = BufReader::new(server_rd);

    // A non-JSON first frame is byte-pumped verbatim.
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

    // On stdin EOF the pump half-closes the socket and keeps reading it until the
    // kernel hangs up, so the server-side write half must be dropped too.
    drop(child_stdin);
    drop(server_wr);
    let _ = timeout(TEST_BUDGET, child.wait()).await;
}

#[tokio::test]
async fn socket_to_stdout_forwards_bytes() {
    let tmp = calm_test_sockets::socket_dir("shim");
    let socket_path: PathBuf = calm_test_sockets::socket_path(tmp.path(), "kernel.sock");
    let listener = listen(&socket_path);

    let mut child = Command::new(SHIM_BIN)
        .env("NEIGE_MCP_SOCKET", &socket_path)
        .env_remove("NEIGE_MCP_DAEMON_TOKEN")
        .env("NEIGE_MCP_TOKEN", "test-byte-pump-token")
        // The pump keeps reading the socket after stdin EOF (it only half-closes its
        // write side), so the post-accept socket write does not race a shim that is gone.
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

    server_wr
        .write_all(b"hello-from-socket\n")
        .await
        .expect("write socket");
    server_wr.flush().await.expect("flush socket");

    let child_stdout = child.stdout.take().expect("stdout piped");
    let mut reader = BufReader::new(child_stdout);
    let mut line = String::new();
    timeout(TEST_BUDGET, reader.read_line(&mut line))
        .await
        .expect("stdout read within budget")
        .expect("read_line ok");
    assert_eq!(line, "hello-from-socket\n");

    drop(server_wr);
    let _ = timeout(TEST_BUDGET, child.wait()).await;
}

/// Stdin EOF is a half-close: the shim shuts down its socket write side and keeps
/// reading the socket until the kernel hangs up, so a late kernel frame still lands.
#[tokio::test]
async fn shim_stays_alive_after_stdin_eof_until_socket_closes() {
    let tmp = calm_test_sockets::socket_dir("shim");
    let socket_path: PathBuf = calm_test_sockets::socket_path(tmp.path(), "kernel.sock");
    let listener = listen(&socket_path);

    let mut child = Command::new(SHIM_BIN)
        .env("NEIGE_MCP_SOCKET", &socket_path)
        .env_remove("NEIGE_MCP_DAEMON_TOKEN")
        .env("NEIGE_MCP_TOKEN", "test-byte-pump-token")
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

    // Long enough for a shim that exits on stdin EOF to be gone already.
    tokio::time::sleep(Duration::from_millis(100)).await;

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

    drop(server_wr);
    let _ = timeout(TEST_BUDGET, child.wait()).await;
}

#[tokio::test]
async fn missing_socket_env_exits_nonzero() {
    let child = Command::new(SHIM_BIN)
        .env_remove("NEIGE_MCP_SOCKET")
        .env_remove("NEIGE_MCP_DAEMON_TOKEN")
        .env_remove("NEIGE_MCP_TOKEN")
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

#[tokio::test]
async fn missing_token_env_exits_nonzero() {
    let tmp = calm_test_sockets::socket_dir("shim");
    let socket_path: PathBuf = calm_test_sockets::socket_path(tmp.path(), "kernel.sock");
    // The missing-token check runs before connect; the listener is only defensive.
    let _listener = listen(&socket_path);

    let child = Command::new(SHIM_BIN)
        .env("NEIGE_MCP_SOCKET", &socket_path)
        .env_remove("NEIGE_MCP_DAEMON_TOKEN")
        .env_remove("NEIGE_MCP_TOKEN")
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
        "shim must fail without NEIGE_MCP_DAEMON_TOKEN or NEIGE_MCP_TOKEN; got status {:?}",
        out.status
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("NEIGE_MCP_DAEMON_TOKEN") && stderr.contains("NEIGE_MCP_TOKEN"),
        "shim stderr should mention the missing env var; got: {stderr}"
    );
}

#[tokio::test]
async fn initialize_first_frame_gets_token_injected() {
    let tmp = calm_test_sockets::socket_dir("shim");
    let socket_path: PathBuf = calm_test_sockets::socket_path(tmp.path(), "kernel.sock");
    let listener = listen(&socket_path);

    let mut child = Command::new(SHIM_BIN)
        .env("NEIGE_MCP_SOCKET", &socket_path)
        .env_remove("NEIGE_MCP_DAEMON_TOKEN")
        .env("NEIGE_MCP_TOKEN", "e2e-shim-token-xyz")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn shim");

    let (server_stream, _addr) = timeout(TEST_BUDGET, listener.accept())
        .await
        .expect("shim connected within budget")
        .expect("accept ok");
    let (server_rd, server_wr) = server_stream.into_split();
    let mut server_reader = BufReader::new(server_rd);

    let mut child_stdin = child.stdin.take().expect("stdin piped");
    let init_frame = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\"}}\n";
    child_stdin
        .write_all(init_frame)
        .await
        .expect("write stdin");
    child_stdin.flush().await.expect("flush stdin");

    let mut received = String::new();
    timeout(TEST_BUDGET, server_reader.read_line(&mut received))
        .await
        .expect("server read within budget")
        .expect("read line ok");

    let parsed: serde_json::Value =
        serde_json::from_str(received.trim_end()).expect("kernel received valid JSON");
    assert_eq!(parsed["method"], "initialize");
    let token = parsed["params"]["_meta"]["dev.neige/auth"]["token"]
        .as_str()
        .expect("shim stamped token slot");
    assert_eq!(token, "e2e-shim-token-xyz");

    drop(child_stdin);
    drop(server_wr);
    let _ = timeout(TEST_BUDGET, child.wait()).await;
}

#[tokio::test]
async fn daemon_token_env_takes_precedence_over_legacy_token() {
    let tmp = calm_test_sockets::socket_dir("shim");
    let socket_path: PathBuf = calm_test_sockets::socket_path(tmp.path(), "kernel.sock");
    let listener = listen(&socket_path);

    let mut child = Command::new(SHIM_BIN)
        .env("NEIGE_MCP_SOCKET", &socket_path)
        .env("NEIGE_MCP_DAEMON_TOKEN", "daemon-token")
        .env("NEIGE_MCP_TOKEN", "legacy-token")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn shim");

    let (server_stream, _addr) = timeout(TEST_BUDGET, listener.accept())
        .await
        .expect("shim connected within budget")
        .expect("accept ok");
    let (server_rd, server_wr) = server_stream.into_split();
    let mut server_reader = BufReader::new(server_rd);

    let mut child_stdin = child.stdin.take().expect("stdin piped");
    let init_frame = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n";
    child_stdin
        .write_all(init_frame)
        .await
        .expect("write stdin");
    child_stdin.flush().await.expect("flush stdin");

    let mut received = String::new();
    timeout(TEST_BUDGET, server_reader.read_line(&mut received))
        .await
        .expect("server read within budget")
        .expect("read line ok");
    let parsed: serde_json::Value =
        serde_json::from_str(received.trim_end()).expect("kernel received valid JSON");
    assert_eq!(
        parsed["params"]["_meta"]["dev.neige/auth"]["token"],
        serde_json::json!("daemon-token")
    );

    drop(child_stdin);
    drop(server_wr);
    let _ = timeout(TEST_BUDGET, child.wait()).await;
}

#[tokio::test]
async fn kernel_restart_reconnects_and_replays_initialize() {
    let (_tmp, socket_path) = common::socket();
    let listener = common::listen(&socket_path);
    let mut child = common::spawn_shim(&socket_path);
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));

    let mut conn = common::accept(&listener, "first connection").await;
    common::write_stdin(&mut stdin, &common::initialize_line(1)).await;
    let init = conn.read_frame("initialize on first connection").await;
    common::assert_replayed_initialize(&init, 1);
    conn.reply_ok(&init["id"]).await;
    let resp = common::read_stdout(&mut stdout, "initialize response").await;
    assert_eq!(resp["id"], serde_json::json!(1));

    drop(conn);
    drop(listener);
    let listener = common::rebind(&socket_path);

    common::write_stdin(&mut stdin, &common::tools_call_line(2)).await;

    let mut conn = common::accept(&listener, "reconnect after kernel restart").await;
    let replayed = conn.read_frame("replayed initialize").await;
    common::assert_replayed_initialize(&replayed, 1);
    conn.reply_ok(&replayed["id"]).await;
    let call = conn.read_frame("tools/call after replay").await;
    assert_eq!(call["method"], "tools/call", "got {call}");
    assert_eq!(call["id"], serde_json::json!(2));
    conn.reply_ok(&call["id"]).await;

    // The replayed handshake response was swallowed by the shim.
    let resp = common::read_stdout(&mut stdout, "tools/call response").await;
    assert_eq!(
        resp["id"],
        serde_json::json!(2),
        "first stdout frame after reconnect must be the tools/call response, got {resp}"
    );
    common::assert_alive(&mut child);

    drop(stdin);
    drop(conn);
    let _ = timeout(TEST_BUDGET, child.wait()).await;
}
