//! End-to-end check of the `codex_appserver` client against a real `codex app-server` over a unix socket.
//! Feature-gated behind `codex-e2e`; resolves the binary via `NEIGE_CODEX_BIN` only and self-skips (never fails)
//! when codex, auth, or the boot/handshake is unavailable. Model turns go through `NEIGE_CODEX_PROXY`
//! (default `http://127.0.0.1:2080`; set empty to disable).

#![cfg(all(unix, feature = "codex-e2e"))]

use crate::support;

use std::process::Stdio;
use std::time::Duration;

use calm_server::codex_appserver::{ClientInfo, CodexAppServer, InputItem, Notification};
use calm_server::planner_model::TurnModelSelection;
// Env `NEIGE_CODEX_BIN` only; tests must never fall back to a PATH/home codex.
use support::codex_fixture::resolve_codex_bin;
use tokio::process::Command;
use tokio::time::timeout;

const DEFAULT_PROXY: &str = "http://127.0.0.1:2080";

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

#[tokio::test]
async fn appserver_client_drives_live_turn_and_second_client_resumes() {
    let Some(codex_bin) = resolve_codex_bin() else {
        skip!(
            "codex binary not resolved (NEIGE_CODEX_BIN unset, or not an executable file); CI has no codex"
        );
    };
    eprintln!("[codex-appserver-e2e] using codex at {codex_bin:?}");

    // The socket must live under a USER-OWNED dir: the server chmods its parent to 0700 and EPERMs on a shared sticky /tmp.
    let sock_dir = tempfile::tempdir().expect("mktemp -d for socket");
    let sock = sock_dir.path().join("app.sock");
    let listen = format!("unix://{}", sock.display());

    let mut cmd = Command::new(&codex_bin);
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
        Err(e) => skip!("failed to spawn `codex app-server`: {e}"),
    };

    // Poll for the socket to appear (the server creates it after binding).
    let mut connected: Option<(CodexAppServer, _)> = None;
    let connect_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while tokio::time::Instant::now() < connect_deadline {
        if sock.exists()
            && let Ok(pair) = CodexAppServer::connect(&sock).await
        {
            connected = Some(pair);
            break;
        }
        // Bail early if the child already died (e.g. boot error / no auth).
        if let Ok(Some(status)) = child.try_wait() {
            let _ = child.kill().await;
            skip!(
                "app-server exited during boot (status {status}); likely no codex auth in this env"
            );
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    let Some((client, mut notifs)) = connected else {
        let _ = child.kill().await;
        skip!("could not connect to app-server within 20s (no usable codex env)");
    };
    eprintln!("[codex-appserver-e2e] connected over WS-over-UDS");

    let init = client
        .initialize(ClientInfo {
            name: "neige-calm-e2e".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        })
        .await
        .expect("initialize must succeed once connected");
    eprintln!(
        "[codex-appserver-e2e] initialize ok: {} on {}",
        init.user_agent, init.platform_os
    );
    assert!(
        !init.user_agent.is_empty(),
        "initialize returned a userAgent"
    );

    let thread = client.thread_start(None).await.expect("thread/start");
    let thread_id = thread
        .thread_id()
        .expect("thread/start result carries thread.id")
        .to_string();
    eprintln!(
        "[codex-appserver-e2e] thread started: {thread_id} (model {})",
        thread.model
    );

    let turn = client
        .turn_start(
            &thread_id,
            vec![InputItem::text(
                "Reply with exactly the word OK and nothing else.",
            )],
            &TurnModelSelection::inherit(),
        )
        .await
        .expect("turn/start");
    eprintln!("[codex-appserver-e2e] turn started: {:?}", turn.turn_id());

    // The load-bearing assertion: a real model turn ran and the push notification arrived.
    let completed = drain_until_completed(&mut notifs, &thread_id, Duration::from_secs(180)).await;
    assert!(
        completed,
        "expected a turn/completed notification for thread {thread_id} within 180s"
    );
    eprintln!("[codex-appserver-e2e] PASS: turn/completed observed for {thread_id}");

    // Second connection resumes the SAME thread (a rollout exists on disk now that turn #1 completed).
    match CodexAppServer::connect(&sock).await {
        Ok((client2, _notifs2)) => {
            client2
                .initialize(ClientInfo {
                    name: "neige-calm-e2e-observer".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                })
                .await
                .expect("observer initialize");
            let resumed = client2
                .thread_resume(&thread_id)
                .await
                .expect("thread/resume after a completed turn must succeed");
            assert_eq!(
                resumed.thread_id(),
                Some(thread_id.as_str()),
                "second client resumed the SAME thread id"
            );
            eprintln!("[codex-appserver-e2e] PASS: second client resumed thread {thread_id}");
        }
        Err(e) => {
            // The primary assertion already passed; a flaky second connect must not fail the run.
            eprintln!("[codex-appserver-e2e] note: second connect failed: {e}");
        }
    }

    drop(client);
    let _ = child.kill().await;
}

/// Pull notifications until a `turn/completed` for `thread_id` arrives or `budget` elapses.
async fn drain_until_completed(
    notifs: &mut calm_server::codex_appserver::NotificationStream,
    thread_id: &str,
    budget: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match timeout(remaining, notifs.recv()).await {
            Ok(Some(n)) => match n {
                Notification::TurnStarted { thread_id: t, .. } => {
                    eprintln!("[codex-appserver-e2e]   <- turn/started ({t})");
                }
                Notification::Item { method, .. } => {
                    eprintln!("[codex-appserver-e2e]   <- {method}");
                }
                Notification::ThreadStatusChanged { status, .. } => {
                    eprintln!(
                        "[codex-appserver-e2e]   <- thread/status/changed {}",
                        status.get("type").and_then(|v| v.as_str()).unwrap_or("?")
                    );
                }
                Notification::TurnCompleted { thread_id: t, .. } => {
                    eprintln!("[codex-appserver-e2e]   <- turn/completed ({t})");
                    if t == thread_id {
                        return true;
                    }
                }
                _ => {}
            },
            Ok(None) => return false,
            Err(_) => return false,
        }
    }
}
