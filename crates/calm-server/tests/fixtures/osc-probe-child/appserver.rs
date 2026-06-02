//! Minimal fake `codex app-server` for the OSC-roundtrip tests (#293).
//!
//! Since the #293 push cutover, `POST /api/waves` ALWAYS boots a real
//! `codex app-server` (the kernel-owned push channel) before spawning the
//! spec card's PTY. The OSC-roundtrip tests can't ship a real codex, so this
//! stub stands in: invoked as `codex app-server --listen unix://<sock>`, it
//! binds the socket, accepts the kernel's WebSocket connection, and answers
//! just enough of the v2 JSON-RPC protocol for
//! `spec_appserver::spawn_spec_appserver`'s DECISION-A boot sequence to
//! succeed:
//!
//!   initialize → thread/start → turn/start → (emit `turn/started`)
//!
//! It then stays alive (looping on the connection) so the kernel's handle
//! keeps a live child; the test reaps it via the registry teardown / tempdir
//! drop. No model work is performed — `turn/started` is the only signal the
//! kernel awaits (it proves a rollout exists; the kernel does NOT await
//! `turn/completed`).

use std::path::PathBuf;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::UnixListener;
use tokio_tungstenite::tungstenite::Message;

/// Parse `--listen unix://<path>` out of argv.
fn listen_sock_path() -> PathBuf {
    let av: Vec<String> = std::env::args().collect();
    let mut listen = None;
    for i in 1..av.len().saturating_sub(1) {
        if av[i] == "--listen" {
            listen = Some(av[i + 1].clone());
            break;
        }
    }
    let raw = listen.expect("fake app-server: --listen <uri> required");
    let path = raw.strip_prefix("unix://").unwrap_or(&raw);
    PathBuf::from(path)
}

/// Blocking entry point — spins up a single-threaded tokio runtime and runs
/// the accept/serve loop until the connection closes or the process is
/// killed (which is how the test reaps us).
pub fn run_fake_app_server() {
    let sock = listen_sock_path();
    if let Some(parent) = sock.parent()
        && !parent.exists()
    {
        let _ = std::fs::create_dir_all(parent);
    }
    if sock.exists() {
        let _ = std::fs::remove_file(&sock);
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("fake app-server: build tokio runtime");
    rt.block_on(async move {
        let control = WedgeControl::for_sock(&sock);
        let listener = UnixListener::bind(&sock)
            .unwrap_or_else(|e| panic!("fake app-server: bind {}: {e}", sock.display()));
        // Serve connections forever; the kernel opens one, and the test
        // kills us at teardown. Each connection is handled to completion.
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    if let Err(e) = serve_conn(stream, control.clone()).await {
                        eprintln!("fake app-server: connection ended: {e}");
                    }
                }
                Err(e) => {
                    eprintln!("fake app-server: accept failed: {e}");
                    return;
                }
            }
        }
    });
}

#[derive(Clone)]
struct WedgeControl {
    active: bool,
    wedge_this_process: bool,
}

impl WedgeControl {
    fn for_sock(sock: &std::path::Path) -> Self {
        let path = sock.with_extension("wedge-count");
        if let Ok(raw) = std::env::var("FAKE_CODEX_WEDGE_PROCESS_COUNT")
            && !path.exists()
        {
            let _ = std::fs::write(&path, raw);
        }

        let active = path.exists();
        let mut remaining = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| raw.trim().parse::<u32>().ok())
            .unwrap_or(0);
        let wedge_this_process = remaining > 0;
        if wedge_this_process {
            remaining -= 1;
            let _ = std::fs::write(&path, remaining.to_string());
        }

        Self {
            active,
            wedge_this_process,
        }
    }

    fn turn_completion_delay(&self) -> Option<std::time::Duration> {
        env_delay("FAKE_CODEX_TURN_COMPLETED_DELAY_MS").or_else(|| {
            (self.active && !self.wedge_this_process)
                .then_some(std::time::Duration::from_millis(25))
        })
    }
}

async fn serve_conn(stream: tokio::net::UnixStream, control: WedgeControl) -> Result<(), String> {
    let ws = tokio_tungstenite::accept_async(stream)
        .await
        .map_err(|e| format!("ws accept: {e}"))?;
    let (mut write, mut read) = ws.split();

    let thread_id = "fake-thread-0001";
    let turn_id = "fake-turn-0001";

    while let Some(msg) = read.next().await {
        let msg = msg.map_err(|e| format!("ws read: {e}"))?;
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Binary(b) => String::from_utf8_lossy(&b).to_string(),
            Message::Close(_) => break,
            Message::Ping(p) => {
                write
                    .send(Message::Pong(p))
                    .await
                    .map_err(|e| format!("pong: {e}"))?;
                continue;
            }
            _ => continue,
        };

        let req: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        record_request(&req);
        let id = req.get("id").cloned();
        let method = req
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        // Notifications (no id) — ignore.
        let Some(id) = id else { continue };

        match method.as_str() {
            "initialize" => {
                if let Some(delay) = env_delay("FAKE_CODEX_INITIALIZE_DELAY_MS") {
                    tokio::time::sleep(delay).await;
                }
                if env_flag("FAKE_CODEX_FAIL_INITIALIZE") {
                    send_error(&mut write, &id, -32000, "forced initialize failure").await?;
                    continue;
                }
                send_result(
                    &mut write,
                    &id,
                    json!({ "userAgent": "fake-codex-app-server/0" }),
                )
                .await?;
            }
            "thread/start" | "thread/resume" => {
                send_result(
                    &mut write,
                    &id,
                    json!({ "thread": { "id": thread_id }, "model": "fake-model" }),
                )
                .await?;
                // `thread/started` notification (best-effort; the kernel
                // tracks thread ids from it but doesn't block on it).
                send_notification(
                    &mut write,
                    "thread/started",
                    json!({ "threadId": thread_id }),
                )
                .await?;
            }
            "turn/start" => {
                if env_flag("FAKE_CODEX_FAIL_TURN_START") {
                    send_error(&mut write, &id, -32000, "forced turn/start failure").await?;
                    continue;
                }
                // Ack with the turn object first…
                send_result(&mut write, &id, json!({ "turn": { "id": turn_id } })).await?;
                if env_flag("FAKE_CODEX_EXIT_AFTER_TURN_ACK") {
                    std::process::exit(0);
                }
                // …then emit the `turn/started` notification the kernel's
                // DECISION-A sequence awaits (proves a rollout exists).
                if !env_flag("FAKE_CODEX_SKIP_TURN_STARTED") {
                    if let Some(delay) = env_delay("FAKE_CODEX_TURN_STARTED_DELAY_MS") {
                        tokio::time::sleep(delay).await;
                    }
                    send_notification(
                        &mut write,
                        "turn/started",
                        json!({ "threadId": thread_id, "turn": { "id": turn_id } }),
                    )
                    .await?;
                }
                if let Some(delay) = control.turn_completion_delay() {
                    tokio::time::sleep(delay).await;
                    send_notification(
                        &mut write,
                        "turn/completed",
                        json!({ "threadId": thread_id, "turn": { "id": turn_id } }),
                    )
                    .await?;
                }
            }
            "turn/interrupt" => {
                if let Ok(path) = std::env::var("FAKE_CODEX_INTERRUPT_MARKER") {
                    let _ = std::fs::write(path, "1");
                }
                if control.wedge_this_process {
                    send_result(&mut write, &id, json!({})).await?;
                    continue;
                }
                if env_flag("FAKE_CODEX_IGNORE_TURN_INTERRUPT") {
                    continue;
                }
                send_result(&mut write, &id, json!({})).await?;
                if !env_flag("FAKE_CODEX_INTERRUPT_NO_COMPLETED") {
                    if let Some(delay) = env_delay("FAKE_CODEX_INTERRUPT_COMPLETED_DELAY_MS") {
                        tokio::time::sleep(delay).await;
                    }
                    send_notification(
                        &mut write,
                        "turn/completed",
                        json!({
                            "threadId": thread_id,
                            "turn": { "id": turn_id, "status": "interrupted" }
                        }),
                    )
                    .await?;
                }
            }
            // Anything else (turn/steer, thread/inject_items, …) — ack with
            // an empty object so a caller never wedges on a missing response.
            _ => {
                send_result(&mut write, &id, json!({})).await?;
            }
        }
    }
    Ok(())
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes") | Ok("YES")
    )
}

fn env_delay(name: &str) -> Option<std::time::Duration> {
    let raw = std::env::var(name).ok()?;
    let ms = raw.parse::<u64>().ok()?;
    Some(std::time::Duration::from_millis(ms))
}

fn record_request(req: &Value) {
    let Ok(path) = std::env::var("FAKE_CODEX_CAPTURE_REQUESTS") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = writeln!(file, "{req}");
    }
}

async fn send_result<S>(write: &mut S, id: &Value, result: Value) -> Result<(), String>
where
    S: SinkExt<Message> + Unpin,
    <S as futures::Sink<Message>>::Error: std::fmt::Display,
{
    let frame = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    write
        .send(Message::Text(frame.to_string()))
        .await
        .map_err(|e| format!("send result: {e}"))
}

async fn send_error<S>(write: &mut S, id: &Value, code: i64, message: &str) -> Result<(), String>
where
    S: SinkExt<Message> + Unpin,
    <S as futures::Sink<Message>>::Error: std::fmt::Display,
{
    let frame = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    });
    write
        .send(Message::Text(frame.to_string()))
        .await
        .map_err(|e| format!("send error: {e}"))
}

async fn send_notification<S>(write: &mut S, method: &str, params: Value) -> Result<(), String>
where
    S: SinkExt<Message> + Unpin,
    <S as futures::Sink<Message>>::Error: std::fmt::Display,
{
    let frame = json!({ "jsonrpc": "2.0", "method": method, "params": params });
    write
        .send(Message::Text(frame.to_string()))
        .await
        .map_err(|e| format!("send notification: {e}"))
}
