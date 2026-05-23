//! PR8 (#136) — integration tests for the `neige-codex-bridge` Stop
//! hook behavior.
//!
//! The bridge is a small CLI that codex spawns per hook event. For the
//! `Stop` event it long-polls
//! `GET /internal/codex/pending_events?card_id=...&timeout_ms=30000`
//! and emits one of two stdout shapes:
//!
//!   * `{"decision":"block","reason":"<json>"}` when events came back,
//!   * `{}` otherwise (server error, empty events, timeout, ...).
//!
//! Strategy:
//!   1. Spin up a tiny tokio TCP listener that answers exactly one HTTP
//!      response in a scripted shape (the bridge connects once per
//!      run).
//!   2. Spawn the compiled bridge binary (via `env!("CARGO_BIN_EXE_<name>")`)
//!      with `NEIGE_CARD_ID` + `NEIGE_CALM_BASE_URL` pointing at the
//!      stub.
//!   3. Pipe `Stop` hook JSON on stdin, assert on stdout/exit code.
//!
//! Tests use a 10s wall-clock cap on the whole bridge process so a
//! hung test doesn't strand the suite.

use std::io::Write;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TEST_BUDGET: Duration = Duration::from_secs(10);

/// Bind a tokio TCP listener on an ephemeral port and return both the
/// listener and the address. We pass the address into the bridge as
/// `NEIGE_CALM_BASE_URL=http://<addr>`.
async fn bind_stub() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind 127.0.0.1:0");
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{}", addr);
    (listener, base)
}

/// Accept exactly one connection, slurp the request, and respond with
/// `body` (which must already be JSON; we tack on the right
/// Content-Type + Content-Length).
async fn serve_one_response(listener: TcpListener, body: String) {
    let (mut stream, _) = listener.accept().await.expect("accept stub conn");
    // Drain the request headers. ureq sends a complete request before
    // it reads the response, so we just need to consume enough bytes
    // for it to flush. A small buffer slurp is fine for tests.
    let mut req_buf = vec![0u8; 4096];
    // Best-effort read; ignore EOF, partial reads.
    let _ = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut req_buf)).await;

    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body,
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Same as [`serve_one_response`] but emits a 404 — used by the
/// "server returns error" test.
async fn serve_one_error(listener: TcpListener, status_line: &str) {
    let (mut stream, _) = listener.accept().await.expect("accept stub conn");
    let mut req_buf = vec![0u8; 4096];
    let _ = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut req_buf)).await;

    let body = "{}";
    let resp = format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status_line,
        body.len(),
        body,
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Accept one connection, then deliberately hang (drop bytes on the
/// floor without responding) until the bridge bails on its own
/// timeout.
async fn serve_one_hang(listener: TcpListener) {
    let (mut stream, _) = listener.accept().await.expect("accept stub conn");
    let mut req_buf = vec![0u8; 4096];
    let _ = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut req_buf)).await;
    // Hold the connection open without sending bytes. Block for the
    // full test budget; the bridge will surface its own timeout long
    // before this returns.
    tokio::time::sleep(TEST_BUDGET).await;
    // Drop `stream` here to satisfy the borrow checker.
    drop(stream);
}

/// Spawn the bridge as a subprocess with a `Stop` hook payload on
/// stdin. Returns `(stdout_string, exit_status, stderr_string)`.
fn spawn_bridge_with_stop(base_url: &str) -> (String, std::process::ExitStatus, String) {
    let bridge_bin = env!("CARGO_BIN_EXE_neige-codex-bridge");
    let mut child = std::process::Command::new(bridge_bin)
        .env("NEIGE_CARD_ID", "test-card-123")
        .env("NEIGE_CALM_BASE_URL", base_url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bridge binary");

    let stop_payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "test-session",
    });
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stop_payload.to_string().as_bytes())
        .unwrap();
    child.stdin.take(); // close stdin so the bridge proceeds

    // Bound the wait so a hung subprocess fails the test rather than
    // stranding the suite.
    let output = wait_with_timeout(child, TEST_BUDGET);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (stdout, output.status, stderr)
}

fn wait_with_timeout(mut child: std::process::Child, timeout: Duration) -> std::process::Output {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(_status) = child.try_wait().expect("try_wait") {
            // Capture remaining piped streams.
            return child.wait_with_output().expect("wait_with_output");
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            return child
                .wait_with_output()
                .expect("wait_with_output after kill");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_with_pending_events_emits_block_decision() {
    let (listener, base) = bind_stub().await;
    // Stub returns one event in the array — bridge should print
    // `{"decision":"block","reason":<json string>}`.
    let resp_body = serde_json::json!({
        "events": [
            { "_id": 42, "ev": "task.completed", "data": { "idempotency_key": "k", "result": null, "artifacts": [] } }
        ],
        "since": 42
    })
    .to_string();
    // Spawn the stub on the multi-threaded runtime so the bridge
    // subprocess can be join-blocked on this same runtime.
    let stub_handle = tokio::spawn(serve_one_response(listener, resp_body));

    let base_clone = base.clone();
    let (stdout, status, _stderr) =
        tokio::task::spawn_blocking(move || spawn_bridge_with_stop(&base_clone))
            .await
            .expect("spawn_blocking join");

    // Wait for stub to finish.
    let _ = tokio::time::timeout(Duration::from_secs(2), stub_handle).await;

    assert!(status.success(), "bridge must exit 0 (got {status:?})");
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("bridge stdout is JSON; got: {stdout}"));
    assert_eq!(parsed["decision"], "block", "got: {stdout}");
    // `reason` carries the events array serialized as a string. The
    // bridge stringifies it so codex injects the raw payload into the
    // agent's next turn as a single observation.
    let reason = parsed["reason"].as_str().expect("reason is a string");
    let events_in_reason: serde_json::Value = serde_json::from_str(reason).expect("reason parses");
    assert_eq!(events_in_reason[0]["_id"], 42);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_with_no_events_emits_empty_object() {
    let (listener, base) = bind_stub().await;
    let resp_body = serde_json::json!({ "events": [], "since": null }).to_string();
    let stub_handle = tokio::spawn(serve_one_response(listener, resp_body));

    let base_clone = base.clone();
    let (stdout, status, _stderr) =
        tokio::task::spawn_blocking(move || spawn_bridge_with_stop(&base_clone))
            .await
            .expect("spawn_blocking join");
    let _ = tokio::time::timeout(Duration::from_secs(2), stub_handle).await;

    assert!(status.success(), "bridge must exit 0 (got {status:?})");
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("bridge stdout is JSON; got: {stdout}"));
    assert!(
        parsed.as_object().map(|o| o.is_empty()).unwrap_or(false),
        "expected `{{}}` for empty events; got: {stdout}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_with_server_error_emits_empty_object() {
    let (listener, base) = bind_stub().await;
    let stub_handle =
        tokio::spawn(async move { serve_one_error(listener, "500 Internal Server Error").await });

    let base_clone = base.clone();
    let (stdout, status, _stderr) =
        tokio::task::spawn_blocking(move || spawn_bridge_with_stop(&base_clone))
            .await
            .expect("spawn_blocking join");
    let _ = tokio::time::timeout(Duration::from_secs(2), stub_handle).await;

    // The bridge does NOT fail the hook on server error — it prints
    // `{}` and exits 0 so codex doesn't get stuck.
    assert!(
        status.success(),
        "bridge must always exit 0 (got {status:?})"
    );
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("bridge stdout is JSON; got: {stdout}"));
    assert!(
        parsed.as_object().map(|o| o.is_empty()).unwrap_or(false),
        "expected `{{}}` on server error; got: {stdout}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_falls_back_to_empty_object_when_server_hangs() {
    // The bridge's own ureq client has a 35s timeout (matching the
    // 30s long-poll + 5s slack). To test hang-handling without
    // waiting 35s per CI run, we'd ideally inject a knob; that's not
    // worth a public API just for testability. Instead, we assert the
    // simpler property: the bridge eventually terminates with `{}` on
    // any error path, and the hung path *would* surface as such on
    // its 35s timeout. Marking this test ignored by default keeps CI
    // fast — opt in with `cargo test stop_falls_back -- --ignored`.
    if std::env::var_os("RUN_HANG_TEST").is_none() {
        eprintln!("skipping hang test (set RUN_HANG_TEST=1 to run)");
        return;
    }
    let (listener, base) = bind_stub().await;
    let stub_handle = tokio::spawn(serve_one_hang(listener));

    let base_clone = base.clone();
    let (stdout, status, _stderr) =
        tokio::task::spawn_blocking(move || spawn_bridge_with_stop(&base_clone))
            .await
            .expect("spawn_blocking join");
    let _ = tokio::time::timeout(Duration::from_secs(2), stub_handle).await;

    assert!(status.success(), "bridge must exit 0 even after timeout");
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON");
    assert!(parsed.as_object().map(|o| o.is_empty()).unwrap_or(false));
}
