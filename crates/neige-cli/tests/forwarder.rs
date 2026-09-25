//! Frozen forwarding protocol v1 (docs/architecture/1801-kernel-served-cli.md §3): the forwarder's frames,
//! its verbatim output, and its fixed local failures. Command semantics are the kernel's and are tested there.

#![cfg(unix)]

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::process::Command;
use tokio::time::timeout;

const NEIGE_BIN: &str = env!("CARGO_BIN_EXE_neige");
const TEST_BUDGET: Duration = Duration::from_secs(5);

/// The two frozen request lines, spelled out byte for byte rather than rebuilt from the forwarder's code.
const FROZEN_INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"neige-forward","version":"1"},"_meta":{"dev.neige/auth":{"token":"tok\"en"}}}}"#;
const FROZEN_CLI: &str = r#"{"jsonrpc":"2.0","id":2,"method":"neige/cli","params":{"argv":["--json","cat","a b","ü\n"]}}"#;

fn listen(socket_path: &Path) -> UnixListener {
    calm_test_sockets::assert_fits(socket_path);
    UnixListener::bind(socket_path).expect("bind stub UDS")
}

fn command(socket: Option<&Path>, token: Option<&str>) -> Command {
    let mut cmd = Command::new(NEIGE_BIN);
    cmd.env_remove("NEIGE_MCP_SOCKET")
        .env_remove("NEIGE_MCP_TOKEN")
        .env_remove("NEIGE_MCP_DAEMON_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(socket) = socket {
        cmd.env("NEIGE_MCP_SOCKET", socket);
    }
    if let Some(token) = token {
        cmd.env("NEIGE_MCP_TOKEN", token);
    }
    cmd
}

async fn read_line(reader: &mut BufReader<OwnedReadHalf>) -> String {
    let mut line = String::new();
    timeout(TEST_BUDGET, reader.read_line(&mut line))
        .await
        .expect("frame within budget")
        .expect("read frame");
    line
}

async fn reply(wr: &mut OwnedWriteHalf, frame: Value) {
    let mut bytes = serde_json::to_vec(&frame).expect("serialize");
    bytes.push(b'\n');
    wr.write_all(&bytes).await.expect("write reply");
}

/// Accept one forwarder, answer `initialize`, return its raw lines and the `neige/cli` reply channel.
async fn accept(listener: &UnixListener) -> (String, String, OwnedWriteHalf) {
    let (stream, _) = timeout(TEST_BUDGET, listener.accept())
        .await
        .expect("forwarder connected")
        .expect("accept");
    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd);
    let init = read_line(&mut reader).await;
    reply(
        &mut wr,
        json!({ "jsonrpc": "2.0", "id": 1, "result": { "protocolVersion": "2024-11-05" } }),
    )
    .await;
    let cli = read_line(&mut reader).await;
    (init, cli, wr)
}

async fn finish(child: tokio::process::Child) -> std::process::Output {
    timeout(TEST_BUDGET, child.wait_with_output())
        .await
        .expect("forwarder exited")
        .expect("wait")
}

#[tokio::test]
async fn forwarder_frames_are_frozen() {
    let dir = calm_test_sockets::socket_dir("fwd");
    let socket = calm_test_sockets::socket_path(dir.path(), "kernel.sock");
    let listener = listen(&socket);
    let child = command(Some(&socket), Some("tok\"en"))
        .args(["--json", "cat", "a b", "ü\n"])
        .spawn()
        .expect("spawn");
    let (init, cli, mut wr) = accept(&listener).await;
    assert_eq!(init, format!("{FROZEN_INITIALIZE}\n"));
    assert_eq!(cli, format!("{FROZEN_CLI}\n"));
    reply(
        &mut wr,
        json!({ "jsonrpc": "2.0", "id": 2, "result": { "stdout": "", "stderr": "", "exit": 0 } }),
    )
    .await;
    assert!(finish(child).await.status.success());
}

#[tokio::test]
async fn forwarder_writes_bytes_and_exit_verbatim() {
    let dir = calm_test_sockets::socket_dir("fwd");
    let socket = calm_test_sockets::socket_path(dir.path(), "kernel.sock");
    let listener = listen(&socket);
    let child = command(Some(&socket), Some("t"))
        .args(["state"])
        .spawn()
        .expect("spawn");
    let (_, _, mut wr) = accept(&listener).await;
    reply(&mut wr, json!({ "jsonrpc": "2.0", "id": 2, "result": { "stdout": "a\n", "stderr": "b", "exit": 7 } })).await;
    let out = finish(child).await;
    assert_eq!(out.stdout, b"a\n");
    assert_eq!(out.stderr, b"b");
    assert_eq!(out.status.code(), Some(7));
}

async fn run_without_server(cmd: &mut Command) -> std::process::Output {
    timeout(TEST_BUDGET, cmd.output())
        .await
        .expect("forwarder exited")
        .expect("run")
}

#[tokio::test]
async fn forwarder_local_failures_are_fixed() {
    let dir = calm_test_sockets::socket_dir("fwd");
    let socket = calm_test_sockets::socket_path(dir.path(), "kernel.sock");

    // Missing or empty env vars: exit 2. The daemon token is never a substitute.
    for (sock, token, var) in [
        (None, Some("t"), "NEIGE_MCP_SOCKET"),
        (Some(socket.as_path()), None, "NEIGE_MCP_TOKEN"),
        (Some(Path::new("")), Some("t"), "NEIGE_MCP_SOCKET"),
        (Some(socket.as_path()), Some(""), "NEIGE_MCP_TOKEN"),
    ] {
        let mut cmd = command(sock, token);
        cmd.env("NEIGE_MCP_DAEMON_TOKEN", "daemon").arg("--help");
        let out = run_without_server(&mut cmd).await;
        assert_eq!(out.status.code(), Some(2), "{var}");
        assert!(out.stdout.is_empty());
        assert_eq!(
            String::from_utf8(out.stderr).unwrap(),
            format!("neige: missing {var} env var; run from a neige planner terminal\n")
        );
    }

    // Connect failure: exit 3.
    let out = run_without_server(command(Some(&socket), Some("t")).arg("state")).await;
    assert_eq!(out.status.code(), Some(3));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.starts_with(&format!("neige: connect {}: ", socket.display())),
        "{stderr:?}"
    );

    // Non-UTF-8 argv: exit 5, before any connection.
    let listener = listen(&socket);
    let mut cmd = command(Some(&socket), Some("t"));
    cmd.arg("cat").arg(OsStr::from_bytes(b"\xff"));
    let out = run_without_server(&mut cmd).await;
    assert_eq!(out.status.code(), Some(5));
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        "neige: argument 2 is not valid UTF-8\n"
    );

    // A lone `--version` is local: semver on stdout, exit 0, no connection.
    let out = run_without_server(command(Some(&socket), Some("t")).arg("--version")).await;
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("neige {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(
        timeout(Duration::from_millis(200), listener.accept())
            .await
            .is_err(),
        "--version or non-UTF-8 argv connected to the kernel"
    );

    // JSON-RPC error: exit 4 with `<method>: <message> (code N)`; `--version` among other args is forwarded.
    let child = command(Some(&socket), Some("t"))
        .args(["ls", "--version"])
        .spawn()
        .expect("spawn");
    let (_, cli, mut wr) = accept(&listener).await;
    assert!(cli.contains(r#""argv":["ls","--version"]"#), "{cli}");
    reply(&mut wr, json!({ "jsonrpc": "2.0", "id": 2, "error": { "code": -32601, "message": "method not found: neige/cli" } })).await;
    let out = finish(child).await;
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        "neige: neige/cli: method not found: neige/cli (code -32601)\n"
    );

    // Closed stdout (`neige cat x | head`): nothing more is written, exit 141.
    let mut child = command(Some(&socket), Some("t"))
        .args(["cat", "x"])
        .spawn()
        .expect("spawn");
    drop(child.stdout.take());
    let (_, _, mut wr) = accept(&listener).await;
    reply(&mut wr, json!({ "jsonrpc": "2.0", "id": 2, "result": { "stdout": "x".repeat(1 << 20), "stderr": "late", "exit": 0 } })).await;
    let out = finish(child).await;
    assert_eq!(out.status.code(), Some(141));
    assert!(
        out.stderr.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}
