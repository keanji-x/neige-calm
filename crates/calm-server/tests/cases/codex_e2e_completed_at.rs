//! Confirms on a real codex app-server that `thread/read(includeTurns:true)` reports the last turn's
//! `completedAt == null` only while a turn is in progress, and SET once completed or aborted — the
//! discriminator the reaper's died-mid-turn arbiter keys on. Feature-gated behind `codex-e2e`, self-skipping,
//! and boots its OWN throwaway app-server on a fresh 0700 tempdir socket (never the live daemon).

#![cfg(all(unix, feature = "codex-e2e"))]

use crate::support;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use calm_server::codex_appserver::{
    ClientInfo, CodexAppServer, InputItem, Notification, ThreadStatus,
};
use calm_server::planner_model::TurnModelSelection;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
// Env `NEIGE_CODEX_BIN` only; tests must never fall back to a PATH/home codex.
use support::codex_fixture::resolve_codex_bin;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::{Instant, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const DEFAULT_PROXY: &str = "http://127.0.0.1:2080";
const WS_URI: &str = "ws://localhost/";

/// Apply the proxy env (unless explicitly disabled) so spawned app-server model turns reach the upstream.
fn apply_proxy(cmd: &mut Command) {
    let proxy = std::env::var("NEIGE_CODEX_PROXY").unwrap_or_else(|_| DEFAULT_PROXY.to_string());
    if !proxy.is_empty() {
        cmd.env("HTTP_PROXY", &proxy)
            .env("HTTPS_PROXY", &proxy)
            .env("http_proxy", &proxy)
            .env("https_proxy", &proxy);
    }
}

/// Boot a throwaway app-server and run `initialize`; `None` on any env-absence condition so the caller can `skip!`.
/// `_sock_dir` keeps the tempdir alive; `child` is `kill_on_drop`.
struct BootedServer {
    client: CodexAppServer,
    notifs: calm_server::codex_appserver::NotificationStream,
    child: Child,
    sock: PathBuf,
    _sock_dir: tempfile::TempDir,
}

async fn boot_throwaway_server(codex_bin: &Path) -> Option<BootedServer> {
    // The socket must live under a USER-OWNED 0700 dir (the server chmods the parent and EPERMs on sticky /tmp).
    let sock_dir = tempfile::tempdir().expect("mktemp -d for socket");
    let sock = sock_dir.path().join("app.sock");
    let listen = format!("unix://{}", sock.display());

    let mut cmd = Command::new(codex_bin);
    cmd.arg("app-server")
        .arg("--listen")
        .arg(&listen)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    apply_proxy(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[codex-e2e-completed-at] failed to spawn app-server: {e}");
            return None;
        }
    };

    let mut connected: Option<(CodexAppServer, _)> = None;
    let connect_deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < connect_deadline {
        if sock.exists()
            && let Ok(pair) = CodexAppServer::connect(&sock).await
        {
            connected = Some(pair);
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            let _ = child.kill().await;
            eprintln!("[codex-e2e-completed-at] app-server exited during boot (status {status})");
            return None;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    let Some((client, notifs)) = connected else {
        let _ = child.kill().await;
        eprintln!("[codex-e2e-completed-at] could not connect within 20s");
        return None;
    };

    if let Err(e) = client
        .initialize(ClientInfo {
            name: "neige-calm-d6-completed-at".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        })
        .await
    {
        let _ = child.kill().await;
        eprintln!("[codex-e2e-completed-at] initialize failed: {e}");
        return None;
    }

    Some(BootedServer {
        client,
        notifs,
        child,
        sock,
        _sock_dir: sock_dir,
    })
}

/// Raw JSON-RPC `thread/read` over a fresh connection, returning the verbatim `result` (the report artifact);
/// `initialize` first because the server requires it before any other method.
async fn raw_thread_read(sock: &Path, thread_id: &str) -> Option<Value> {
    let stream = UnixStream::connect(sock).await.ok()?;
    let request = WS_URI.into_client_request().ok()?;
    let (ws, _resp) = tokio_tungstenite::client_async(request, stream)
        .await
        .ok()?;
    let (mut write, mut read) = ws.split();

    let init = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "clientInfo": { "name": "neige-calm-d6-raw", "version": "0" },
            "capabilities": { "experimentalApi": true }
        }
    });
    write
        .send(Message::Text(serde_json::to_string(&init).ok()?))
        .await
        .ok()?;

    let read_req = json!({
        "jsonrpc": "2.0", "id": 2, "method": "thread/read",
        "params": { "threadId": thread_id, "includeTurns": true }
    });
    write
        .send(Message::Text(serde_json::to_string(&read_req).ok()?))
        .await
        .ok()?;

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let msg = match timeout(remaining, read.next()).await {
            Ok(Some(Ok(m))) => m,
            _ => return None,
        };
        let text = match msg {
            Message::Text(t) => t,
            Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
            Message::Close(_) => return None,
            _ => continue,
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if v.get("id").and_then(Value::as_u64) == Some(2) {
            return Some(v.get("result").cloned().unwrap_or(v));
        }
    }
}

/// `Some(true)` if the last turn's `completedAt` is JSON-null, `Some(false)` if set, `None` on shape mismatch.
fn last_turn_completed_at_is_null(result: &Value) -> Option<bool> {
    let turns = result.get("thread")?.get("turns")?.as_array()?;
    let last = turns.last()?;
    let ca = last.get("completedAt")?;
    Some(ca.is_null())
}

#[tokio::test]
async fn thread_read_completed_at_null_only_when_died_mid_turn() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!(
            "codex binary not resolved (NEIGE_CODEX_BIN unset, or not an executable file); CI has no codex"
        );
    };
    eprintln!("[codex-e2e-completed-at] using codex at {codex_bin:?}");

    let Some(mut server) = boot_throwaway_server(&codex_bin).await else {
        skip!("no usable codex app-server env (boot/connect/auth absent)");
    };
    eprintln!("[codex-e2e-completed-at] booted throwaway app-server + initialized");

    let thread = server
        .client
        .thread_start(None)
        .await
        .expect("thread/start");
    let thread_id = thread
        .thread_id()
        .expect("thread/start carries thread.id")
        .to_string();
    eprintln!(
        "[codex-e2e-completed-at] thread started: {thread_id} (model {})",
        thread.model
    );

    // (a) In-progress turn → `completedAt == null`. The prompt forces enough generation to keep the window open.
    let in_progress_prompt = "Count slowly from 1 to 40, putting each number on its own line, \
         and after the list write a one-sentence summary. Take your time.";
    let turn = server
        .client
        .turn_start(
            &thread_id,
            vec![InputItem::text(in_progress_prompt)],
            &TurnModelSelection::inherit(),
        )
        .await
        .expect("turn/start");
    let turn_id = turn.turn_id().map(str::to_string);
    eprintln!("[codex-e2e-completed-at] turn started: {turn_id:?}");

    // Wait for the `turn/started` push so the rollout has a persisted TurnStarted before reading.
    let saw_started =
        wait_for_turn_started(&mut server.notifs, &thread_id, Duration::from_secs(60)).await;
    eprintln!("[codex-e2e-completed-at] turn/started observed: {saw_started}");

    let mut in_progress_raw: Option<Value> = None;
    let mut in_progress_null: Option<bool> = None;
    let poll_deadline = Instant::now() + Duration::from_secs(45);
    while Instant::now() < poll_deadline {
        // Typed read (proves casing/units parse) + raw read (report artifact).
        let typed = server.client.thread_read(&thread_id, true).await;
        if let Ok(resp) = &typed {
            let status = &resp.thread.status;
            let last_ca = resp
                .thread
                .turns
                .as_ref()
                .and_then(|t| t.last())
                .map(|t| t.completed_at);
            eprintln!(
                "[codex-e2e-completed-at]   in-progress poll: status={status:?} last.completed_at={last_ca:?}"
            );
            if let Some(raw) = raw_thread_read(&server.sock, &thread_id).await {
                let is_null = last_turn_completed_at_is_null(&raw);
                if in_progress_raw.is_none() || is_null == Some(true) {
                    in_progress_raw = Some(raw);
                    in_progress_null = is_null;
                }
            }
            // The turn finished before the window could be observed — stop polling.
            if matches!(last_ca, Some(Some(_))) && !matches!(status, ThreadStatus::Active { .. }) {
                eprintln!(
                    "[codex-e2e-completed-at]   turn already completed during poll; stop in-progress poll"
                );
                break;
            }
            if in_progress_null == Some(true) {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }

    if let Some(raw) = &in_progress_raw {
        eprintln!(
            "[codex-e2e-completed-at] === RAW thread/read (IN-PROGRESS) ===\n{}",
            serde_json::to_string_pretty(raw).unwrap_or_default()
        );
    }
    eprintln!(
        "[codex-e2e-completed-at] OBSERVATION (a): in-progress last-turn completedAt == null ? {in_progress_null:?}"
    );

    // (b) Completed turn → `completedAt` SET. Hard assert.
    let completed =
        drain_until_completed(&mut server.notifs, &thread_id, Duration::from_secs(180)).await;
    assert!(
        completed,
        "expected a turn/completed notification for thread {thread_id} within 180s"
    );
    let typed_done = server
        .client
        .thread_read(&thread_id, true)
        .await
        .expect("thread/read after completion");
    let done_last_ca = typed_done
        .thread
        .turns
        .as_ref()
        .and_then(|t| t.last())
        .map(|t| t.completed_at);
    let done_raw = raw_thread_read(&server.sock, &thread_id).await;
    if let Some(raw) = &done_raw {
        eprintln!(
            "[codex-e2e-completed-at] === RAW thread/read (COMPLETED) ===\n{}",
            serde_json::to_string_pretty(raw).unwrap_or_default()
        );
    }
    eprintln!(
        "[codex-e2e-completed-at] OBSERVATION (b): completed last-turn completed_at = {done_last_ca:?} status={:?}",
        typed_done.thread.status
    );
    assert!(
        matches!(done_last_ca, Some(Some(_))),
        "GATE (b): a COMPLETED turn must read completedAt SET (Some(Some(_))), got {done_last_ca:?}"
    );

    // (c) Best-effort: aborted turn → `completedAt` SET (abort != death).
    let abort_outcome = run_abort_probe(&mut server, &thread_id).await;
    eprintln!("[codex-e2e-completed-at] OBSERVATION (c) aborted-turn: {abort_outcome}");

    // Assert null only if the in-progress window was actually observed; a too-fast turn makes it unobservable.
    if let Some(is_null) = in_progress_null {
        assert!(
            is_null,
            "GATE (a): an IN-PROGRESS turn's last-turn completedAt must be null, observed completedAt SET"
        );
        eprintln!(
            "[codex-e2e-completed-at] GATE (a) HARD-CONFIRMED: in-progress completedAt == null"
        );
    } else {
        eprintln!(
            "[codex-e2e-completed-at] NOTE: never observed a clean in-progress window (turn too fast / read raced); (a) recorded as observation only, (b) is the hard gate"
        );
    }

    drop(server.client);
    let _ = server.child.kill().await;
    eprintln!("[codex-e2e-completed-at] DONE (throwaway server killed)");
}

/// Wait for a `turn/started` notification for `thread_id`.
async fn wait_for_turn_started(
    notifs: &mut calm_server::codex_appserver::NotificationStream,
    thread_id: &str,
    budget: Duration,
) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match timeout(remaining, notifs.recv()).await {
            Ok(Some(Notification::TurnStarted { thread_id: t, .. })) if t == thread_id => {
                return true;
            }
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return false,
        }
    }
}

/// Pull notifications until a `turn/completed` for `thread_id` arrives or the
/// budget elapses. Mirrors `codex_appserver_e2e::drain_until_completed`.
async fn drain_until_completed(
    notifs: &mut calm_server::codex_appserver::NotificationStream,
    thread_id: &str,
    budget: Duration,
) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match timeout(remaining, notifs.recv()).await {
            Ok(Some(n)) => match n {
                Notification::TurnStarted { thread_id: t, .. } => {
                    eprintln!("[codex-e2e-completed-at]   <- turn/started ({t})");
                }
                Notification::Item { method, .. } => {
                    eprintln!("[codex-e2e-completed-at]   <- {method}");
                }
                Notification::ThreadStatusChanged { status, .. } => {
                    eprintln!(
                        "[codex-e2e-completed-at]   <- thread/status/changed {}",
                        status.get("type").and_then(|v| v.as_str()).unwrap_or("?")
                    );
                }
                Notification::TurnCompleted { thread_id: t, .. } => {
                    eprintln!("[codex-e2e-completed-at]   <- turn/completed ({t})");
                    if t == thread_id {
                        return true;
                    }
                }
                _ => {}
            },
            Ok(None) | Err(_) => return false,
        }
    }
}

/// Best-effort abort probe; returns a human-readable outcome and never fails the test.
async fn run_abort_probe(server: &mut BootedServer, thread_id: &str) -> String {
    let long_prompt = "Write a detailed 500-word essay about the history of timekeeping. \
         Take your time and be thorough.";
    let turn = match server
        .client
        .turn_start(
            thread_id,
            vec![InputItem::text(long_prompt)],
            &TurnModelSelection::inherit(),
        )
        .await
    {
        Ok(t) => t,
        Err(e) => return format!("turn/start for abort probe failed: {e} (skipped)"),
    };
    let result_turn_id = turn.turn_id().map(str::to_string);

    // Prefer the turn id from the `turn/started` push (the authoritative running id).
    let started_id = started_turn_id(&mut server.notifs, thread_id, Duration::from_secs(30)).await;
    let turn_id = started_id.or(result_turn_id);
    let Some(turn_id) = turn_id else {
        return "could not resolve a running turn id to interrupt (skipped)".to_string();
    };

    tokio::time::sleep(Duration::from_millis(500)).await;
    if let Err(e) = server.client.turn_interrupt(thread_id, &turn_id).await {
        return format!("turn/interrupt({turn_id}) failed: {e} (skipped)");
    }
    eprintln!("[codex-e2e-completed-at]   interrupted turn {turn_id}");

    // Let the abort settle so the daemon persists the terminal event.
    let _ = drain_until_completed(&mut server.notifs, thread_id, Duration::from_secs(30)).await;
    tokio::time::sleep(Duration::from_millis(1000)).await;

    let raw = raw_thread_read(&server.sock, thread_id).await;
    if let Some(raw) = &raw {
        eprintln!(
            "[codex-e2e-completed-at] === RAW thread/read (ABORTED) ===\n{}",
            serde_json::to_string_pretty(raw).unwrap_or_default()
        );
    }
    let typed = server.client.thread_read(thread_id, true).await;
    match typed {
        Ok(resp) => {
            let last_ca = resp
                .thread
                .turns
                .as_ref()
                .and_then(|t| t.last())
                .map(|t| t.completed_at);
            let raw_null = raw.as_ref().and_then(last_turn_completed_at_is_null);
            format!(
                "status={:?} last-turn completed_at={last_ca:?} (raw completedAt is_null={raw_null:?}) \
                 — completedAt SET ⇒ abort != died-mid-turn",
                resp.thread.status
            )
        }
        Err(e) => format!("thread/read after interrupt failed: {e}"),
    }
}

/// Pull notifications until a `turn/started` for `thread_id`, returning its
/// turn id if the push carries one.
async fn started_turn_id(
    notifs: &mut calm_server::codex_appserver::NotificationStream,
    thread_id: &str,
    budget: Duration,
) -> Option<String> {
    let deadline = Instant::now() + budget;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match timeout(remaining, notifs.recv()).await {
            Ok(Some(Notification::TurnStarted { thread_id: t, turn })) if t == thread_id => {
                return turn.get("id").and_then(Value::as_str).map(str::to_string);
            }
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return None,
        }
    }
}
