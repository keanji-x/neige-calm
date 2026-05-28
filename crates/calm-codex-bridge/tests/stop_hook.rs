//! #293 cutover — integration test for the `neige-codex-bridge` Stop hook.
//!
//! The bridge is a small CLI that codex spawns per hook event. Before the
//! #293 push cutover, the `Stop` event was special-cased: it long-polled
//! `GET /internal/codex/pending_events` and emitted
//! `{"decision":"block","reason":...}` when events came back (the pull
//! model). Pull is gone — spec agents are now driven by observations pushed
//! onto their codex thread by the kernel — so `Stop` no longer special-cases
//! anything. It takes the same fire-and-forget path every other hook does:
//!
//!   * POST the raw payload to `POST /internal/codex/hook?card_id=...`, and
//!   * print `{}` to stdout (the codex hook contract for "continue").
//!
//! Strategy:
//!   1. Spin up a tiny tokio TCP listener that captures the request line +
//!      headers and answers a `204 No Content` (what `/internal/codex/hook`
//!      returns).
//!   2. Spawn the compiled bridge binary (via `env!("CARGO_BIN_EXE_<name>")`)
//!      with `NEIGE_CARD_ID` + `NEIGE_CALM_BASE_URL` pointing at the stub.
//!   3. Pipe `Stop` hook JSON on stdin, assert stdout is `{}`, exit 0, and
//!      the request hit `/internal/codex/hook` (NOT the removed
//!      `/internal/codex/pending_events`).
//!
//! Tests use a 10s wall-clock cap on the whole bridge process so a hung test
//! doesn't strand the suite.

use std::io::{ErrorKind, Write};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TEST_BUDGET: Duration = Duration::from_secs(10);

/// Bind a tokio TCP listener on an ephemeral port and return both the
/// listener and the address. We pass the address into the bridge as
/// `NEIGE_CALM_BASE_URL=http://<addr>`.
async fn bind_stub() -> Option<(TcpListener, String)> {
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            eprintln!("skipping bridge process test: sandbox denied loopback bind: {e}");
            return None;
        }
        Err(e) => panic!("bind 127.0.0.1:0: {e}"),
    };
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{}", addr);
    Some((listener, base))
}

/// Accept exactly one connection, capture the raw request bytes into
/// `captured`, and answer `204 No Content` (what `/internal/codex/hook`
/// returns on success).
async fn serve_one_hook(listener: TcpListener, captured: Arc<Mutex<String>>) {
    let (mut stream, _) = listener.accept().await.expect("accept stub conn");
    let mut req_buf = vec![0u8; 8192];
    let n = match tokio::time::timeout(Duration::from_secs(2), stream.read(&mut req_buf)).await {
        Ok(Ok(n)) => n,
        _ => 0,
    };
    {
        let mut g = captured.lock().unwrap();
        *g = String::from_utf8_lossy(&req_buf[..n]).to_string();
    }

    let resp = "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n";
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Spawn the bridge as a subprocess with a `Stop` hook payload on stdin.
/// Returns `(stdout_string, exit_status, stderr_string)`.
fn spawn_bridge_with_stop(
    base_url: &str,
    provider: Option<&str>,
) -> (String, std::process::ExitStatus, String) {
    let bridge_bin = env!("CARGO_BIN_EXE_neige-codex-bridge");
    let mut command = std::process::Command::new(bridge_bin);
    command
        .env("NEIGE_CARD_ID", "test-card-123")
        .env("NEIGE_CALM_BASE_URL", base_url);
    if let Some(provider) = provider {
        command.arg("--provider").arg(provider);
    }
    let mut child = command
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

    let output = wait_with_timeout(child, TEST_BUDGET);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (stdout, output.status, stderr)
}

fn wait_with_timeout(mut child: std::process::Child, timeout: Duration) -> std::process::Output {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(_status) = child.try_wait().expect("try_wait") {
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

/// #293 — Stop takes the fire-and-forget POST path: it POSTs the payload to
/// `/internal/codex/hook` and prints `{}` (no `decision:"block"`, no
/// long-poll against the removed `/internal/codex/pending_events`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_posts_to_hook_and_emits_empty_object() {
    let Some((listener, base)) = bind_stub().await else {
        return;
    };
    let captured = Arc::new(Mutex::new(String::new()));
    let stub_handle = tokio::spawn(serve_one_hook(listener, captured.clone()));

    let base_clone = base.clone();
    let (stdout, status, _stderr) =
        tokio::task::spawn_blocking(move || spawn_bridge_with_stop(&base_clone, None))
            .await
            .expect("spawn_blocking join");

    let _ = tokio::time::timeout(Duration::from_secs(2), stub_handle).await;

    assert!(status.success(), "bridge must exit 0 (got {status:?})");

    // stdout is the bare `{}` continue contract — never a `decision:"block"`.
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("bridge stdout is JSON; got: {stdout}"));
    assert!(
        parsed.as_object().map(|o| o.is_empty()).unwrap_or(false),
        "Stop must print bare `{{}}` (fire-and-forget); got: {stdout}",
    );

    // The request went to /internal/codex/hook (the ingest route), NOT the
    // removed pending_events long-poll endpoint.
    let req = captured.lock().unwrap().clone();
    assert!(
        req.contains("POST /internal/codex/hook"),
        "Stop must POST to /internal/codex/hook; request was:\n{req}"
    );
    assert!(
        !req.contains("pending_events"),
        "Stop must NOT hit the removed pending_events endpoint; request was:\n{req}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_provider_posts_to_claude_hook_and_emits_continue_true() {
    let Some((listener, base)) = bind_stub().await else {
        return;
    };
    let captured = Arc::new(Mutex::new(String::new()));
    let stub_handle = tokio::spawn(serve_one_hook(listener, captured.clone()));

    let base_clone = base.clone();
    let (stdout, status, _stderr) =
        tokio::task::spawn_blocking(move || spawn_bridge_with_stop(&base_clone, Some("claude")))
            .await
            .expect("spawn_blocking join");

    let _ = tokio::time::timeout(Duration::from_secs(2), stub_handle).await;

    assert!(status.success(), "bridge must exit 0 (got {status:?})");
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("bridge stdout is JSON; got: {stdout}"));
    assert_eq!(parsed, serde_json::json!({ "continue": true }));

    let req = captured.lock().unwrap().clone();
    assert!(
        req.contains("POST /internal/claude/hook"),
        "Claude mode must POST to /internal/claude/hook; request was:\n{req}"
    );
    let req_lower = req.to_ascii_lowercase();
    assert!(
        req_lower.contains("x-calm-actor: ai:claude"),
        "Claude mode must stamp ai:claude actor header; request was:\n{req}"
    );
}
