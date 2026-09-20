//! Shared helpers for the shim integration tests: a stub kernel on a short-path UDS,
//! a spawned shim, and line-level JSON-RPC readers on both wires.

#![allow(dead_code)]

use std::net::Shutdown;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

pub const SHIM_BIN: &str = env!("CARGO_BIN_EXE_neige-mcp-stdio-shim");
pub const TEST_BUDGET: Duration = Duration::from_secs(5);
pub const TOKEN: &str = "test-reconnect-token";

/// Bind the stub listener at `socket_path` (short path, see `calm_test_sockets`).
pub fn listen(socket_path: &Path) -> UnixListener {
    calm_test_sockets::assert_fits(socket_path);
    UnixListener::bind(socket_path).unwrap_or_else(|e| {
        panic!(
            "bind stub UDS at {} ({} bytes): {e}",
            socket_path.display(),
            socket_path.as_os_str().as_encoded_bytes().len()
        )
    })
}

/// Drop-and-rebind at the same path. tokio does not unlink the socket file on drop.
pub fn rebind(socket_path: &Path) -> UnixListener {
    let _ = std::fs::remove_file(socket_path);
    listen(socket_path)
}

/// A short-path socket directory plus the `kernel.sock` path inside it.
pub fn socket() -> (calm_test_sockets::TempDir, PathBuf) {
    let tmp = calm_test_sockets::socket_dir("shim");
    let path = calm_test_sockets::socket_path(tmp.path(), "kernel.sock");
    (tmp, path)
}

/// Spawn the shim against `socket_path` with piped stdio and [`TOKEN`].
pub fn spawn_shim(socket_path: &Path) -> Child {
    Command::new(SHIM_BIN)
        .env("NEIGE_MCP_SOCKET", socket_path)
        .env_remove("NEIGE_MCP_DAEMON_TOKEN")
        .env("NEIGE_MCP_TOKEN", TOKEN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn shim")
}

/// One accepted stub-side connection, split into a line reader and a writer.
pub struct StubConn {
    pub reader: BufReader<OwnedReadHalf>,
    pub writer: OwnedWriteHalf,
}

impl StubConn {
    /// Read one frame the shim wrote on this connection, parsed as JSON.
    pub async fn read_frame(&mut self, what: &str) -> serde_json::Value {
        let mut line = String::new();
        let n = timeout(TEST_BUDGET, self.reader.read_line(&mut line))
            .await
            .unwrap_or_else(|_| panic!("stub read of {what} within budget"))
            .unwrap_or_else(|e| panic!("stub read of {what}: {e}"));
        assert!(n > 0, "stub saw EOF while waiting for {what}");
        serde_json::from_str(line.trim_end())
            .unwrap_or_else(|e| panic!("stub received non-JSON for {what}: {e}: {line:?}"))
    }

    /// Read one frame, or `None` if the shim closed the connection.
    pub async fn read_frame_or_eof(&mut self, what: &str) -> Option<serde_json::Value> {
        let mut line = String::new();
        let n = timeout(TEST_BUDGET, self.reader.read_line(&mut line))
            .await
            .unwrap_or_else(|_| panic!("stub read of {what} within budget"))
            .unwrap_or_else(|e| panic!("stub read of {what}: {e}"));
        if n == 0 {
            return None;
        }
        Some(
            serde_json::from_str(line.trim_end())
                .unwrap_or_else(|e| panic!("stub received non-JSON for {what}: {e}: {line:?}")),
        )
    }

    /// Write one JSON frame plus the newline trailer on this connection.
    pub async fn write_frame(&mut self, frame: &serde_json::Value) {
        let line = format!("{frame}\n");
        timeout(TEST_BUDGET, self.writer.write_all(line.as_bytes()))
            .await
            .expect("stub write within budget")
            .expect("stub write ok");
        self.writer.flush().await.expect("stub flush ok");
    }

    /// Reply `{"result":{}}` to request `id`.
    pub async fn reply_ok(&mut self, id: &serde_json::Value) {
        self.write_frame(&serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}))
            .await;
    }

    /// Reply a JSON-RPC error with `code` to request `id`.
    pub async fn reply_error(&mut self, id: &serde_json::Value, code: i64, message: &str) {
        self.write_frame(&serde_json::json!({
            "jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}
        }))
        .await;
    }
}

/// Accept the shim's next connection on `listener` within the budget.
pub async fn accept(listener: &UnixListener, what: &str) -> StubConn {
    let (stream, _addr) = timeout(TEST_BUDGET, listener.accept())
        .await
        .unwrap_or_else(|_| panic!("shim connected ({what}) within budget"))
        .expect("accept ok");
    let (rd, wr) = stream.into_split();
    StubConn {
        reader: BufReader::new(rd),
        writer: wr,
    }
}

/// Write one line to the shim's stdin (codex -> shim direction).
pub async fn write_stdin(stdin: &mut ChildStdin, line: &str) {
    timeout(TEST_BUDGET, stdin.write_all(line.as_bytes()))
        .await
        .expect("stdin write within budget")
        .expect("stdin write ok");
    stdin.flush().await.expect("stdin flush ok");
}

/// Read one JSON frame from the shim's stdout (shim -> codex direction).
pub async fn read_stdout(reader: &mut BufReader<ChildStdout>, what: &str) -> serde_json::Value {
    let mut line = String::new();
    let n = timeout(TEST_BUDGET, reader.read_line(&mut line))
        .await
        .unwrap_or_else(|_| panic!("stdout read of {what} within budget"))
        .expect("stdout read ok");
    assert!(n > 0, "shim closed stdout while waiting for {what}");
    serde_json::from_str(line.trim_end())
        .unwrap_or_else(|e| panic!("shim wrote non-JSON on stdout for {what}: {e}: {line:?}"))
}

/// The canonical `initialize` request codex sends first (id 1, no `_meta`).
pub fn initialize_line(id: i64) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"initialize\",\"params\":{{\"protocolVersion\":\"2024-11-05\"}}}}\n"
    )
}

/// A request with `id` and `method` and empty params (not an `initialize`).
pub fn request_line(id: i64, method: &str) -> String {
    format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{method}\",\"params\":{{}}}}\n")
}

/// A `tools/call` request with `id`.
pub fn tools_call_line(id: i64) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/call\",\"params\":{{\"name\":\"calm.plan.list\",\"arguments\":{{}}}}}}\n"
    )
}

/// Assert the frame is the shim's replayed `initialize`: same id, token stamped.
pub fn assert_replayed_initialize(frame: &serde_json::Value, id: i64) {
    assert_eq!(
        frame["method"], "initialize",
        "expected initialize, got {frame}"
    );
    assert_eq!(frame["id"], serde_json::json!(id), "initialize id: {frame}");
    assert_eq!(
        frame["params"]["_meta"]["dev.neige/auth"]["token"],
        serde_json::json!(TOKEN),
        "replayed initialize must carry the injected token: {frame}"
    );
}

/// `Ok(None)` from `try_wait` means the child is still running.
pub fn assert_alive(child: &mut Child) {
    let status = child.try_wait().expect("try_wait ok");
    assert!(status.is_none(), "shim exited unexpectedly: {status:?}");
}

/// Wait for the child to exit within the budget and return its exit code.
pub async fn wait_exit_code(child: &mut Child, what: &str) -> i32 {
    wait_exit_code_within(child, TEST_BUDGET, what).await
}

/// Wait for the child to exit within `within` and return its exit code.
pub async fn wait_exit_code_within(child: &mut Child, within: Duration, what: &str) -> i32 {
    let status = timeout(within, child.wait())
        .await
        .unwrap_or_else(|_| panic!("shim exited ({what}) within {within:?}"))
        .expect("wait ok");
    status
        .code()
        .unwrap_or_else(|| panic!("shim killed by signal: {status:?}"))
}

/// Read the shim's next stderr line (one per state change).
pub async fn read_stderr_line(reader: &mut BufReader<ChildStderr>) -> String {
    let mut line = String::new();
    let n = timeout(TEST_BUDGET, reader.read_line(&mut line))
        .await
        .expect("stderr line within budget")
        .expect("stderr read ok");
    assert!(n > 0, "shim closed stderr");
    line
}

/// Answer the handshake, then shut down the stub's READ side only: the shim's next
/// socket write fails with EPIPE while its read side sees no EOF.
pub async fn handshake_then_shut_read(
    listener: &UnixListener,
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
) -> std::os::unix::net::UnixStream {
    let (mut stream, _addr) = timeout(TEST_BUDGET, listener.accept())
        .await
        .expect("shim connected within budget")
        .expect("accept ok");
    write_stdin(stdin, &initialize_line(1)).await;
    let mut line = String::new();
    {
        let mut reader = BufReader::new(&mut stream);
        timeout(TEST_BUDGET, reader.read_line(&mut line))
            .await
            .expect("stub read initialize within budget")
            .expect("stub read ok");
    }
    let init: serde_json::Value = serde_json::from_str(line.trim_end()).expect("initialize JSON");
    assert_replayed_initialize(&init, 1);
    stream
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n")
        .await
        .expect("stub reply ok");
    let resp = read_stdout(stdout, "initialize response").await;
    assert_eq!(resp["id"], serde_json::json!(1));

    let std_stream = stream.into_std().expect("into_std");
    std_stream
        .shutdown(Shutdown::Read)
        .expect("shutdown read side");
    std_stream
}
