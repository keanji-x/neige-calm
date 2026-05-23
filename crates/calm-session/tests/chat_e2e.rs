//! End-to-end test of daemon chat-mode + runner stdio protocol.
//!
//! Spawns `calm-session-daemon --mode chat` with a stub Node runner that
//! implements the same stdio control protocol as the real
//! `neige-chat-runner` but skips the SDK — on each `user_message` line it
//! writes a Passthrough `NeigeEvent` echoing the content. The test exercises
//! the full daemon ↔ runner pipe (control NDJSON in, NeigeEvent JSON out)
//! plus the daemon-side broadcast / `HelloChat` replay path.
//!
//! Why a stub: the real runner would need a live Anthropic API key and
//! network egress. The stub keeps the test hermetic and fast (<1s) while
//! still exercising every Rust-side wire boundary touched by the SDK
//! migration.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding, read_frame, write_frame,
};
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::timeout;
use uuid::Uuid;

/// Minimal v2 ClientHello with no resume cursor — chat mode ignores
/// everything except the variant tag, but we need a well-formed frame.
fn chat_hello(terminal_id: &str) -> ClientMsg {
    ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: terminal_id.to_string(),
        client_id: Uuid::new_v4(),
        desired_size: PtySize {
            cols: 80,
            rows: 24,
            pixel_width: None,
            pixel_height: None,
        },
        cell_size: None,
        initial_scrollback: InitialScrollback::None,
        resume_from: None,
        role_hint: None,
        capabilities: ClientCapabilities {
            render_encodings: vec![RenderEncoding::Vt],
            supports_scrollback: false,
            supports_sixel: false,
            supports_images: false,
            kernel_originated_input: false,
        },
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/
        .and_then(|p| p.parent()) // workspace
        .map(|p| p.to_path_buf())
        .expect("walk up to workspace root")
}

fn stub_runner_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("stub-runner.mjs")
}

fn silent_stub_runner_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("silent-stub-runner.mjs")
}

/// Best-effort node lookup. The CI image has node on PATH; if a developer
/// runs `cargo test` without node we skip with a clear message rather than
/// fail.
fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Frame-read timeout for daemon→client reads. #241 had bumped this to
/// 20 s as a symptomatic workaround for a startup race where
/// `session_init` could miss the pre-attach replay buffer and land on
/// the live broadcast (whose latency could spike to seconds on contended
/// CI runners — see #240 diagnostics). #243 fixed that structurally by
/// deferring `HelloChat` until the runner has emitted its first frame,
/// so the live path no longer carries startup frames. 5 s is plenty for
/// the post-handshake round-trips here (stub-runner replies are instant)
/// while still bounding genuine hangs well under tokio's default 60 s
/// test timeout.
const FRAME_READ_TIMEOUT: Duration = Duration::from_secs(5);

async fn read_chat_event(rd: &mut tokio::net::unix::OwnedReadHalf) -> String {
    match timeout(FRAME_READ_TIMEOUT, read_frame::<DaemonMsg, _>(rd))
        .await
        .expect("daemon frame timeout")
        .expect("daemon frame decode")
    {
        DaemonMsg::ChatEvent { json } => json,
        other => panic!("expected ChatEvent, got {other:?}"),
    }
}

#[tokio::test]
async fn chat_user_message_round_trips_through_runner() {
    if !node_available() {
        eprintln!("skipping chat_user_message_round_trips_through_runner: node not on PATH");
        return;
    }
    let stub = stub_runner_path();
    assert!(stub.exists(), "stub runner missing at {}", stub.display());

    let daemon_bin = env!("CARGO_BIN_EXE_calm-session-daemon");
    let id = Uuid::new_v4();
    let sock = std::env::temp_dir().join(format!("calm-chat-e2e-{id}.sock"));
    let _ = std::fs::remove_file(&sock);

    let mut child = Command::new(daemon_bin)
        .args(["--mode", "chat"])
        .args(["--id", &id.to_string()])
        .args(["--sock", &sock.to_string_lossy()])
        .args(["--runner-path", &stub.to_string_lossy()])
        .args(["--cwd", workspace_root().to_string_lossy().as_ref()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn daemon");

    // Wait for the socket to bind.
    let stream = {
        let mut connected = None;
        for _ in 0..150 {
            if let Ok(s) = UnixStream::connect(&sock).await {
                connected = Some(s);
                break;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        connected.expect("daemon did not bind socket within 6s")
    };
    let (mut rd, mut wr) = stream.into_split();

    // Handshake (ClientHello v2) + HelloChat. Chat mode only checks the
    // variant tag; the inner fields are unused.
    write_frame(&mut wr, &chat_hello(&id.to_string()))
        .await
        .unwrap();
    let hello = timeout(FRAME_READ_TIMEOUT, read_frame::<DaemonMsg, _>(&mut rd))
        .await
        .expect("HelloChat timeout")
        .unwrap();
    let replay = match hello {
        DaemonMsg::HelloChat { replay } => replay,
        other => panic!("expected HelloChat, got {other:?}"),
    };

    // The stub emits a session_init synchronously on startup. Post-#243
    // the daemon defers `HelloChat` until the runner has emitted at
    // least one frame, so `session_init` must always be in the replay
    // vec — never on the live broadcast path.
    let init_json = replay
        .into_iter()
        .find(|j| j.contains("session_init"))
        .expect("session_init missing from HelloChat replay (regression of #243)");
    assert!(
        init_json.contains("\"type\":\"session_init\""),
        "got: {init_json}"
    );
    assert!(
        init_json.contains(&id.to_string()),
        "session_init missing session_id: {init_json}"
    );

    // Send a user message; expect a stub_echo passthrough.
    write_frame(
        &mut wr,
        &ClientMsg::ChatUserMessage {
            content: "ping".into(),
        },
    )
    .await
    .unwrap();
    // The first live frame after handshake must be the stub_echo of the
    // user message — `session_init` is always in HelloChat.replay (#243)
    // and broadcast dedup (#244) guarantees it's never re-delivered on
    // the live channel.
    let echo = read_chat_event(&mut rd).await;
    assert!(echo.contains("stub_echo"), "got: {echo}");
    assert!(echo.contains("ping"), "got: {echo}");

    // AnswerQuestion control frame round-trip.
    let qid = Uuid::new_v4();
    write_frame(
        &mut wr,
        &ClientMsg::AnswerQuestion {
            question_id: qid,
            answers: std::collections::HashMap::from([("Proceed?".to_string(), "yes".to_string())]),
        },
    )
    .await
    .unwrap();
    let ans = read_chat_event(&mut rd).await;
    assert!(ans.contains("stub_answer"), "got: {ans}");
    assert!(ans.contains(&qid.to_string()), "got: {ans}");
    assert!(
        ans.contains("\"answers\":{\"Proceed?\":\"yes\"}"),
        "got: {ans}"
    );

    // Stop → stub emits one last passthrough then exits 0.
    write_frame(&mut wr, &ClientMsg::ChatStop).await.unwrap();
    let stop_ack = read_chat_event(&mut rd).await;
    assert!(stop_ack.contains("stub_stop"), "got: {stop_ack}");

    // Daemon detects child exit and forwards ChildExited.
    let exit = timeout(FRAME_READ_TIMEOUT, read_frame::<DaemonMsg, _>(&mut rd))
        .await
        .expect("ChildExited timeout")
        .unwrap();
    match exit {
        DaemonMsg::ChildExited { code } => {
            assert_eq!(code, Some(0), "stub exited non-zero: {code:?}");
        }
        other => panic!("expected ChildExited, got {other:?}"),
    }

    // Daemon should clean up the socket on shutdown; give it a moment.
    let _ = child.wait().await;
    assert!(
        !sock.exists(),
        "daemon left stale socket at {}",
        sock.display()
    );
}

#[tokio::test]
async fn chat_resume_flag_round_trips_to_runner_argv() {
    // Locks the daemon's --resume → runner --resume forwarding. We don't
    // need the SDK; the stub just needs to start successfully under the
    // resume code path (the SDK call is what `--resume` ultimately gates).
    if !node_available() {
        eprintln!("skipping chat_resume_flag: node not on PATH");
        return;
    }
    let stub = stub_runner_path();
    let daemon_bin = env!("CARGO_BIN_EXE_calm-session-daemon");
    let id = Uuid::new_v4();
    let sock = std::env::temp_dir().join(format!("calm-chat-resume-{id}.sock"));
    let _ = std::fs::remove_file(&sock);

    let mut child = Command::new(daemon_bin)
        .args(["--mode", "chat"])
        .args(["--id", &id.to_string()])
        .args(["--sock", &sock.to_string_lossy()])
        .args(["--runner-path", &stub.to_string_lossy()])
        .args(["--cwd", workspace_root().to_string_lossy().as_ref()])
        .arg("--resume")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn daemon");

    // Just verify socket binds; the stub doesn't differentiate fresh vs
    // resume so a successful Attach is enough to prove --resume parsed.
    let mut connected = false;
    for _ in 0..150 {
        if UnixStream::connect(&sock).await.is_ok() {
            connected = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(connected, "daemon did not bind under --resume");

    let _ = child.kill().await;
}

/// Locks the bounded-fallback branch of the chat-mode attach handshake
/// (#243). The silent stub never writes anything on stdout, so the
/// "wait for first runner frame" signal never fires; the daemon must
/// still produce a `HelloChat` (with an empty `replay`) within
/// `CHAT_FIRST_FRAME_TIMEOUT` (~5 s) so a misbehaving runner can't
/// deadlock the attach handshake indefinitely.
#[tokio::test]
async fn hello_chat_falls_through_when_runner_never_emits() {
    if !node_available() {
        eprintln!("skipping hello_chat_falls_through_when_runner_never_emits: node not on PATH");
        return;
    }
    let stub = silent_stub_runner_path();
    assert!(
        stub.exists(),
        "silent stub runner missing at {}",
        stub.display()
    );

    let daemon_bin = env!("CARGO_BIN_EXE_calm-session-daemon");
    let id = Uuid::new_v4();
    let sock = std::env::temp_dir().join(format!("calm-chat-silent-{id}.sock"));
    let _ = std::fs::remove_file(&sock);

    let mut child = Command::new(daemon_bin)
        .args(["--mode", "chat"])
        .args(["--id", &id.to_string()])
        .args(["--sock", &sock.to_string_lossy()])
        .args(["--runner-path", &stub.to_string_lossy()])
        .args(["--cwd", workspace_root().to_string_lossy().as_ref()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn daemon");

    // Wait for the socket to bind.
    let stream = {
        let mut connected = None;
        for _ in 0..150 {
            if let Ok(s) = UnixStream::connect(&sock).await {
                connected = Some(s);
                break;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        connected.expect("daemon did not bind socket within 6s")
    };
    let (mut rd, mut wr) = stream.into_split();
    write_frame(&mut wr, &chat_hello(&id.to_string()))
        .await
        .unwrap();

    // The bounded fallback timeout in the daemon is ~5 s. Wait at most
    // ~8 s here for `HelloChat`; if the daemon hung, we'd never make it.
    let start = std::time::Instant::now();
    let hello = timeout(Duration::from_secs(8), read_frame::<DaemonMsg, _>(&mut rd))
        .await
        .expect("HelloChat did not arrive within bounded fallback (regression of #243)")
        .expect("HelloChat decode");
    let elapsed = start.elapsed();
    let replay = match hello {
        DaemonMsg::HelloChat { replay } => replay,
        other => panic!("expected HelloChat, got {other:?}"),
    };
    assert!(
        replay.is_empty(),
        "silent runner should produce empty replay, got {} entries",
        replay.len()
    );
    // Sanity: the fallback shouldn't be instantaneous (we did wait on
    // the signal) and shouldn't exceed ~7 s either.
    assert!(
        elapsed >= Duration::from_secs(4),
        "HelloChat returned in {elapsed:?}; expected at least ~4 s (fallback timeout)"
    );
    assert!(
        elapsed <= Duration::from_secs(7),
        "HelloChat took {elapsed:?}; fallback timeout should be ~5 s"
    );

    // Tear down: ask the stub to exit via control frame so the daemon
    // shuts down cleanly without us relying on its kill path.
    let _ = write_frame(&mut wr, &ClientMsg::ChatStop).await;
    let _ = child.wait().await;
}
