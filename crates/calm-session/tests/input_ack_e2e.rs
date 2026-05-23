//! End-to-end test for `DaemonMsg::InputAck` against a real daemon
//! binary.
//!
//! Spawns the daemon under `sleep 60`, opens one client connection with
//! `kernel_originated_input: true` (the trusted path used by
//! `DaemonClient::inject_stdin`), and verifies:
//!
//! 1. An `Input` frame with `input_seq: N > 0` produces a
//!    `DaemonMsg::InputAck { input_seq: N }` on the same connection
//!    after the daemon's PTY-writer thread has flushed the bytes to the
//!    PTY master.
//! 2. Two consecutive Inputs with seqs `N, N+1` produce two acks in
//!    that order.
//! 3. An `Input` frame with `input_seq: 0` (the browser-typing default
//!    — "no ack requested", option (b) from issue #115) produces NO
//!    ack — only the existing `RenderPatch` echo bytes.
//!
//! These are the load-bearing wire-level guarantees for the migration
//! of PR #110's `inject_stdin` from a 50ms close-grace
//! `tokio::time::sleep` to `await InputAck`.

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

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .expect("walk up to workspace root")
}

/// Connect a single client to the daemon socket and complete the
/// handshake. Returns the split reader/writer plus the assigned role
/// (which is `Owner` for the first attach to a fresh daemon).
async fn connect_and_attach(
    sock: &std::path::Path,
    real_tid: &str,
    kernel_input: bool,
) -> (
    tokio::net::unix::OwnedReadHalf,
    tokio::net::unix::OwnedWriteHalf,
) {
    let stream = UnixStream::connect(sock).await.expect("connect");
    let (mut rd, mut wr) = stream.into_split();
    let hello = ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: real_tid.to_string(),
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
            kernel_originated_input: kernel_input,
        },
    };
    write_frame(&mut wr, &hello).await.unwrap();
    // Drain ServerHello so subsequent reads start clean.
    let _server_hello: DaemonMsg = timeout(Duration::from_secs(2), read_frame(&mut rd))
        .await
        .expect("server hello timeout")
        .expect("server hello decode");
    (rd, wr)
}

/// Spawn a daemon under `sleep 60` (long enough for any test sequence)
/// and wait for its unix socket to bind.
async fn spawn_daemon() -> (tokio::process::Child, std::path::PathBuf, String) {
    let daemon_bin = env!("CARGO_BIN_EXE_calm-session-daemon");
    let id = Uuid::new_v4();
    let sock = std::env::temp_dir().join(format!("calm-iack-{id}.sock"));
    let _ = std::fs::remove_file(&sock);

    let child = Command::new(daemon_bin)
        .args(["--mode", "terminal"])
        .args(["--id", &id.to_string()])
        .args(["--sock", &sock.to_string_lossy()])
        // #177 PR2: terminal-mode daemon now requires theme RGB. The
        // values are placeholders — this test exercises input-ack,
        // not OSC replies. See `tests/daemon_cli_theme.rs` for the
        // theme-specific coverage.
        .args(["--terminal-fg", "216,219,226"])
        .args(["--terminal-bg", "15,20,24"])
        .args(["--cwd", workspace_root().to_string_lossy().as_ref()])
        .args(["--", "sh", "-c", "sleep 60"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn daemon");

    // Wait for the socket to bind.
    let mut bound = false;
    for _ in 0..150 {
        if UnixStream::connect(&sock).await.is_ok() {
            bound = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(bound, "daemon did not bind socket within 6s");
    (child, sock, id.to_string())
}

/// Walk the down stream looking for the next `DaemonMsg::InputAck`.
/// Skips `RenderPatch` / `RenderSnapshot` / `ChildReady` etc. that may
/// be interleaved. Returns the matched ack's seq.
async fn next_input_ack<R>(rd: &mut R, budget: Duration) -> u64
where
    R: tokio::io::AsyncRead + Unpin,
{
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("timed out waiting for InputAck");
        }
        let frame: DaemonMsg = timeout(remaining, read_frame(rd))
            .await
            .expect("read frame timeout")
            .expect("read frame decode");
        if let DaemonMsg::InputAck { input_seq } = frame {
            return input_seq;
        }
        // Skip non-ack frames (RenderPatch echo, ChildReady, etc.).
    }
}

/// Collect every frame that arrives within `window`. Used to assert
/// the *absence* of `InputAck` on the seq-0 path — we keep reading
/// until either the window elapses or the connection closes, then
/// inspect the collected frames.
async fn collect_frames<R>(rd: &mut R, window: Duration) -> Vec<DaemonMsg>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let deadline = tokio::time::Instant::now() + window;
    let mut out = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match timeout(remaining, read_frame::<DaemonMsg, _>(rd)).await {
            Ok(Ok(m)) => out.push(m),
            Ok(Err(_)) | Err(_) => break,
        }
    }
    out
}

#[tokio::test]
async fn input_ack_arrives_after_pty_write() {
    let (_child, sock, real_tid) = spawn_daemon().await;
    let (mut rd, mut wr) = connect_and_attach(&sock, &real_tid, true).await;

    // Send Input with seq=42. The daemon must emit InputAck{42} after
    // the PTY write completes. We give a generous budget — the actual
    // write is fast, but RenderPatch echoes may arrive first and we
    // skip them.
    write_frame(
        &mut wr,
        &ClientMsg::Input {
            data: b"\r".to_vec(), // bare \r is benign to `sh -c 'sleep 60'`
            input_seq: 42,
        },
    )
    .await
    .unwrap();

    let acked_seq = next_input_ack(&mut rd, Duration::from_secs(3)).await;
    assert_eq!(acked_seq, 42, "ack must echo the request's seq verbatim");
}

#[tokio::test]
async fn input_ack_preserves_seq_order() {
    let (_child, sock, real_tid) = spawn_daemon().await;
    let (mut rd, mut wr) = connect_and_attach(&sock, &real_tid, true).await;

    // Two consecutive Inputs with seqs N, N+1 → InputAck N, then
    // InputAck N+1. Per-connection PTY-writer is FIFO so the ack
    // order matches the send order.
    write_frame(
        &mut wr,
        &ClientMsg::Input {
            data: b"".to_vec(),
            input_seq: 100,
        },
    )
    .await
    .unwrap();
    write_frame(
        &mut wr,
        &ClientMsg::Input {
            data: b"".to_vec(),
            input_seq: 101,
        },
    )
    .await
    .unwrap();

    let first = next_input_ack(&mut rd, Duration::from_secs(3)).await;
    let second = next_input_ack(&mut rd, Duration::from_secs(3)).await;
    assert_eq!(first, 100);
    assert_eq!(second, 101);
}

#[tokio::test]
async fn input_seq_zero_produces_no_ack() {
    let (_child, sock, real_tid) = spawn_daemon().await;
    let (mut rd, mut wr) = connect_and_attach(&sock, &real_tid, true).await;

    // Send Input with seq=0 — the wire default for browser typing
    // ("no ack requested" — option (b) from issue #115). The daemon
    // must perform the PTY write but emit NO InputAck.
    write_frame(
        &mut wr,
        &ClientMsg::Input {
            data: b"silent\r".to_vec(),
            input_seq: 0,
        },
    )
    .await
    .unwrap();

    // Collect for a window long enough to catch any "delayed" ack
    // (the daemon's PTY-writer thread is synchronous; 500ms is well
    // beyond any plausible coalescing). Then assert no InputAck
    // shows up.
    let frames = collect_frames(&mut rd, Duration::from_millis(500)).await;
    let acks: Vec<_> = frames
        .iter()
        .filter(|m| matches!(m, DaemonMsg::InputAck { .. }))
        .collect();
    assert!(
        acks.is_empty(),
        "seq-0 frame must not generate any InputAck, got {acks:?}"
    );
}
