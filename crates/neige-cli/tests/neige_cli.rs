#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::process::Command;
use tokio::time::timeout;

const NEIGE_BIN: &str = env!("CARGO_BIN_EXE_neige");
const TEST_BUDGET: Duration = Duration::from_secs(5);

fn listen(socket_path: &Path) -> UnixListener {
    UnixListener::bind(socket_path).expect("bind stub UDS")
}

async fn write_frame(stream: &mut tokio::net::unix::OwnedWriteHalf, value: Value) {
    let mut line = serde_json::to_vec(&value).expect("serialize frame");
    line.push(b'\n');
    stream.write_all(&line).await.expect("write frame");
    stream.flush().await.expect("flush frame");
}

async fn read_frame(reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> Value {
    let mut line = String::new();
    timeout(TEST_BUDGET, reader.read_line(&mut line))
        .await
        .expect("read within budget")
        .expect("read frame");
    serde_json::from_str(line.trim_end()).expect("valid JSON frame")
}

fn tool_result(id: i64, structured: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": structured.to_string() }],
            "structuredContent": structured,
            "isError": false,
        }
    })
}

async fn accept_initialized(
    listener: UnixListener,
) -> (
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
) {
    accept_initialized_with_token(listener, "test-token").await
}

async fn accept_initialized_with_token(
    listener: UnixListener,
    expected_token: &str,
) -> (
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
) {
    let (server_stream, _addr) = timeout(TEST_BUDGET, listener.accept())
        .await
        .expect("client connected")
        .expect("accept ok");
    let (rd, mut wr) = server_stream.into_split();
    let mut reader = BufReader::new(rd);

    let init = read_frame(&mut reader).await;
    assert_eq!(init["method"], "initialize");
    assert_eq!(
        init["params"]["_meta"]["dev.neige/auth"]["token"],
        json!(expected_token)
    );
    write_frame(
        &mut wr,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "stub", "version": "0" }
            }
        }),
    )
    .await;

    (reader, wr)
}

async fn spawn_neige(socket_path: &Path, args: &[&str]) -> tokio::process::Child {
    Command::new(NEIGE_BIN)
        .args(args)
        .env("NEIGE_MCP_SOCKET", socket_path)
        .env_remove("NEIGE_MCP_DAEMON_TOKEN")
        .env("NEIGE_MCP_TOKEN", "test-token")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn neige")
}

async fn spawn_neige_with_daemon_token(socket_path: &Path, args: &[&str]) -> tokio::process::Child {
    Command::new(NEIGE_BIN)
        .args(args)
        .env("NEIGE_MCP_SOCKET", socket_path)
        .env_remove("NEIGE_MCP_TOKEN")
        .env("NEIGE_MCP_DAEMON_TOKEN", "daemon-test-token")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn neige")
}

#[tokio::test]
async fn ls_root_lists_top_level_entries() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket_path: PathBuf = tmp.path().join("kernel.sock");
    let listener = listen(&socket_path);
    let child = spawn_neige(&socket_path, &["ls"]).await;

    let (mut reader, mut wr) = accept_initialized(listener).await;
    let call = read_frame(&mut reader).await;
    assert_eq!(call["method"], "tools/call");
    assert_eq!(call["params"]["name"], json!("calm.wave.ls"));
    assert_eq!(call["params"]["arguments"], json!({ "path": "/" }));
    write_frame(
        &mut wr,
        tool_result(
            2,
            json!([
                { "name": "index.md", "kind": "file" },
                { "name": "wave.json", "kind": "file" },
                { "name": "report.md", "kind": "file" },
                { "name": "cards/", "kind": "dir", "size": 3 }
            ]),
        ),
    )
    .await;

    let out = timeout(TEST_BUDGET, child.wait_with_output())
        .await
        .expect("child exited")
        .expect("wait ok");
    assert!(
        out.status.success(),
        "stderr = {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    assert!(stdout.contains("- index.md"), "stdout = {stdout:?}");
    assert!(stdout.contains("- wave.json"), "stdout = {stdout:?}");
    assert!(stdout.contains("- report.md"), "stdout = {stdout:?}");
    assert!(stdout.contains("d cards/"), "stdout = {stdout:?}");
}

#[tokio::test]
async fn daemon_token_env_alias_is_accepted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket_path: PathBuf = tmp.path().join("kernel.sock");
    let listener = listen(&socket_path);
    let child = spawn_neige_with_daemon_token(&socket_path, &["state"]).await;

    let (mut reader, mut wr) = accept_initialized_with_token(listener, "daemon-test-token").await;
    let call = read_frame(&mut reader).await;
    assert_eq!(call["params"]["name"], json!("calm.get_wave_state"));
    write_frame(&mut wr, tool_result(2, json!({"ok": true}))).await;

    let out = timeout(TEST_BUDGET, child.wait_with_output())
        .await
        .expect("child exited")
        .expect("wait ok");
    assert!(out.status.success());
}

#[tokio::test]
async fn cat_report_writes_report_content() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket_path: PathBuf = tmp.path().join("kernel.sock");
    let listener = listen(&socket_path);
    let child = spawn_neige(&socket_path, &["cat", "report.md"]).await;

    let (mut reader, mut wr) = accept_initialized(listener).await;
    let call = read_frame(&mut reader).await;
    assert_eq!(call["params"]["name"], json!("calm.wave.cat"));
    assert_eq!(call["params"]["arguments"], json!({ "path": "report.md" }));
    write_frame(
        &mut wr,
        tool_result(
            2,
            json!({
                "content": "# Report\n\nReady.\n",
                "content_type": "text/markdown"
            }),
        ),
    )
    .await;

    let out = timeout(TEST_BUDGET, child.wait_with_output())
        .await
        .expect("child exited")
        .expect("wait ok");
    assert!(
        out.status.success(),
        "stderr = {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "# Report\n\nReady.\n"
    );
}

#[tokio::test]
async fn state_outputs_pretty_wave_state_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket_path: PathBuf = tmp.path().join("kernel.sock");
    let listener = listen(&socket_path);
    let child = spawn_neige(&socket_path, &["state"]).await;

    let (mut reader, mut wr) = accept_initialized(listener).await;
    let call = read_frame(&mut reader).await;
    assert_eq!(call["method"], "tools/call");
    assert_eq!(call["params"]["name"], json!("calm.get_wave_state"));
    assert_eq!(call["params"]["arguments"], json!({}));
    let state = json!({
        "wave": { "id": "w1", "lifecycle": "working" },
        "cards": []
    });
    write_frame(&mut wr, tool_result(2, state.clone())).await;

    let out = timeout(TEST_BUDGET, child.wait_with_output())
        .await
        .expect("child exited")
        .expect("wait ok");
    assert!(
        out.status.success(),
        "stderr = {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    assert!(stdout.contains('\n'), "stdout should be pretty: {stdout:?}");
    let parsed: Value = serde_json::from_str(&stdout).expect("stdout JSON parses");
    assert_eq!(parsed, state);
}

#[tokio::test]
async fn state_json_outputs_compact_wave_state_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket_path: PathBuf = tmp.path().join("kernel.sock");
    let listener = listen(&socket_path);
    let child = spawn_neige(&socket_path, &["--json", "state"]).await;

    let (mut reader, mut wr) = accept_initialized(listener).await;
    let call = read_frame(&mut reader).await;
    assert_eq!(call["params"]["name"], json!("calm.get_wave_state"));
    assert_eq!(call["params"]["arguments"], json!({}));
    let state = json!({
        "wave": { "id": "w1", "lifecycle": "working" },
        "cards": []
    });
    write_frame(&mut wr, tool_result(2, state.clone())).await;

    let out = timeout(TEST_BUDGET, child.wait_with_output())
        .await
        .expect("child exited")
        .expect("wait ok");
    assert!(
        out.status.success(),
        "stderr = {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    assert!(
        !stdout.trim_end().contains('\n'),
        "stdout should be compact: {stdout:?}"
    );
    let parsed: Value = serde_json::from_str(&stdout).expect("stdout JSON parses");
    assert_eq!(parsed, state);
}

#[tokio::test]
async fn missing_token_env_exits_nonzero() {
    let out = timeout(
        TEST_BUDGET,
        Command::new(NEIGE_BIN)
            .arg("ls")
            .env("NEIGE_MCP_SOCKET", "/tmp/neige-test.sock")
            .env_remove("NEIGE_MCP_DAEMON_TOKEN")
            .env_remove("NEIGE_MCP_TOKEN")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .expect("child exited")
    .expect("wait ok");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("NEIGE_MCP_TOKEN") && stderr.contains("NEIGE_MCP_DAEMON_TOKEN"),
        "stderr = {stderr}"
    );
}

#[tokio::test]
async fn missing_socket_env_exits_nonzero() {
    let out = timeout(
        TEST_BUDGET,
        Command::new(NEIGE_BIN)
            .arg("ls")
            .env_remove("NEIGE_MCP_SOCKET")
            .env_remove("NEIGE_MCP_DAEMON_TOKEN")
            .env("NEIGE_MCP_TOKEN", "test-token")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .expect("child exited")
    .expect("wait ok");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("NEIGE_MCP_SOCKET"), "stderr = {stderr}");
}

#[tokio::test]
async fn token_argv_flag_is_rejected() {
    let out = timeout(
        TEST_BUDGET,
        Command::new(NEIGE_BIN)
            .args(["--token", "secret", "ls"])
            .env_remove("NEIGE_MCP_SOCKET")
            .env_remove("NEIGE_MCP_DAEMON_TOKEN")
            .env_remove("NEIGE_MCP_TOKEN")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .expect("child exited")
    .expect("wait ok");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown option `--token`"),
        "stderr = {stderr}"
    );
}

#[tokio::test]
async fn ls_json_outputs_parseable_array() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket_path: PathBuf = tmp.path().join("kernel.sock");
    let listener = listen(&socket_path);
    let child = spawn_neige(&socket_path, &["ls", "--json"]).await;

    let (mut reader, mut wr) = accept_initialized(listener).await;
    let call = read_frame(&mut reader).await;
    assert_eq!(call["params"]["arguments"], json!({ "path": "/" }));
    write_frame(
        &mut wr,
        tool_result(2, json!([{ "name": "index.md", "kind": "file" }])),
    )
    .await;

    let out = timeout(TEST_BUDGET, child.wait_with_output())
        .await
        .expect("child exited")
        .expect("wait ok");
    assert!(
        out.status.success(),
        "stderr = {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: Value = serde_json::from_slice(&out.stdout).expect("stdout JSON parses");
    assert_eq!(parsed, json!([{ "name": "index.md", "kind": "file" }]));
}

#[tokio::test]
async fn server_unknown_path_error_is_clear_and_json_parseable() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket_path: PathBuf = tmp.path().join("kernel.sock");
    let listener = listen(&socket_path);
    let child = spawn_neige(&socket_path, &["ls", "missing"]).await;

    let (mut reader, mut wr) = accept_initialized(listener).await;
    let _call = read_frame(&mut reader).await;
    write_frame(
        &mut wr,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "error": {
                "code": -32602,
                "message": "calm.wave: path not available in this view: missing"
            }
        }),
    )
    .await;
    let out = timeout(TEST_BUDGET, child.wait_with_output())
        .await
        .expect("child exited")
        .expect("wait ok");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("path not available in this view: missing"),
        "stderr = {stderr}"
    );

    let socket_path: PathBuf = tmp.path().join("kernel-json.sock");
    let listener = listen(&socket_path);
    let child = spawn_neige(&socket_path, &["ls", "--json", "missing"]).await;
    let (mut reader, mut wr) = accept_initialized(listener).await;
    let _call = read_frame(&mut reader).await;
    write_frame(
        &mut wr,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "error": {
                "code": -32602,
                "message": "calm.wave: path not available in this view: missing"
            }
        }),
    )
    .await;
    let out = timeout(TEST_BUDGET, child.wait_with_output())
        .await
        .expect("child exited")
        .expect("wait ok");
    assert!(!out.status.success());
    let parsed: Value = serde_json::from_slice(&out.stderr).expect("stderr JSON parses");
    assert_eq!(
        parsed["error"]["detail"]["rpc_error"]["code"],
        json!(-32602)
    );
}
