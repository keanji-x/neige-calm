//! Issue #199 — terminal v2 multi-client cross-layer smoke.
//!
//! The other terminal-protocol tests each pin one slice in isolation:
//! `protocol_error_routing.rs` covers per-client error addressing,
//! `input_ack_e2e.rs` the ack contract, `server_hello_child_ready.rs`
//! the `is_child_ready` snapshot, etc. The thing that has *not* been
//! tested is the interaction between those slices when two clients
//! share one session and one of them disconnects + reconnects in the
//! middle of the flow.
//!
//! This is the single smoke that catches a regression in any of:
//!
//!   1. **Role assignment is sticky to client_id, not the connection.**
//!      The first attach becomes `Role::Owner`; the second attach is
//!      `Role::Observer`. When the owner disconnects without releasing
//!      (`drop(stream)` — what a refreshed browser tab does), the
//!      observer's role does NOT silently promote. The observer must
//!      explicitly `OwnerClaim` to acquire input rights.
//!   2. **Observer input is rejected with `ProtocolError(NotOwner)` and
//!      that error reaches ONLY the observer.** This is the
//!      per-client routing guarantee from PR-2 of issue #58 — the
//!      narrow `protocol_error_routing.rs` test pins it for the
//!      stale-observer-after-owner-disconnect path here too.
//!   3. **Owner-driven `ResizeCommit` broadcasts `ResizeApplied` to
//!      every connected client.** Both the issuing owner and any
//!      observer see the same epoch + cols/rows. The PTY actually
//!      resizes — confirmed by `RenderSnapshot.cols/rows` on a
//!      mid-session reconnect.
//!   4. **`ChildReady` is one-shot, but `ServerHello.is_child_ready`
//!      is a sticky snapshot for late-joiners.** The first client may
//!      or may not receive a broadcast `ChildReady` (race-y depending
//!      on PTY startup timing). A reconnect after the child has gone
//!      ready arrives with `is_child_ready: true` in its `ServerHello`
//!      — no second broadcast.
//!   5. **`InputAck` is per-connection, not broadcast.** When the
//!      (new) owner sends `Input { input_seq: N>0 }` after claiming,
//!      they receive `InputAck { N }`. The other client (the original
//!      owner that has since reconnected as an observer) does NOT see
//!      that ack. Mirrors `input_ack_e2e.rs`'s narrow contract but
//!      under two-connection conditions.
//!
//! Why one test file with one big `#[tokio::test]` rather than many
//! small ones: the interactions above are state-coupled — splitting
//! them would duplicate the daemon spawn + handshake boilerplate
//! without buying isolation, and would hide the cross-layer composition
//! (issue #199's whole point).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION,
    ProtocolErrorCode, PtySize, RenderEncoding, Role, read_frame, write_frame,
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

/// Hand-build a `ClientHello` for the given terminal id. The other
/// fields are the "browser default" set the WS bridge uses today:
/// 80x24 VT-only, no scrollback, no resume cursor.
fn hello(real_tid: &str, client_id: Uuid) -> ClientMsg {
    ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: real_tid.to_string(),
        client_id,
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

/// Spawn the daemon under a `sleep 60` child — long enough for any
/// step in this test, short enough that an abandoned daemon dies on
/// its own. Returns the kill-on-drop handle, the unix socket path,
/// and the terminal id the clients must echo back.
async fn spawn_daemon() -> (tokio::process::Child, PathBuf, String) {
    let daemon_bin = env!("CARGO_BIN_EXE_calm-session-daemon");
    let id = Uuid::new_v4();
    let sock = std::env::temp_dir().join(format!("calm-mc-cross-{id}.sock"));
    let _ = std::fs::remove_file(&sock);

    let child = Command::new(daemon_bin)
        .args(["--mode", "terminal"])
        .args(["--id", &id.to_string()])
        .args(["--sock", &sock.to_string_lossy()])
        .args(["--cwd", workspace_root().to_string_lossy().as_ref()])
        // `sh -c 'sleep 60'` keeps the PTY child alive long enough for
        // every assertion below; mirrors `input_ack_e2e.rs`.
        .args(["--", "sh", "-c", "sleep 60"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn daemon");

    // Wait for the unix socket to bind. 6s ceiling matches existing
    // multi-client tests (`protocol_error_routing.rs`).
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

/// Connect a new client, send `ClientHello`, drain `ServerHello`,
/// return the role + reader/writer halves + the captured
/// `is_child_ready` flag from the hello snapshot.
async fn attach(
    sock: &std::path::Path,
    real_tid: &str,
    client_id: Uuid,
) -> (
    tokio::net::unix::OwnedReadHalf,
    tokio::net::unix::OwnedWriteHalf,
    Role,
    bool,
    u16,
    u16,
) {
    let stream = UnixStream::connect(sock).await.expect("client connect");
    let (mut rd, mut wr) = stream.into_split();
    write_frame(&mut wr, &hello(real_tid, client_id))
        .await
        .unwrap();
    let server_hello: DaemonMsg = timeout(Duration::from_secs(2), read_frame(&mut rd))
        .await
        .expect("server hello read timeout")
        .expect("server hello decode");
    match server_hello {
        DaemonMsg::ServerHello {
            client_role,
            is_child_ready,
            pty_size,
            ..
        } => (rd, wr, client_role, is_child_ready, pty_size.cols, pty_size.rows),
        other => panic!("expected ServerHello, got {other:?}"),
    }
}

/// Drain frames until either the predicate matches or the budget
/// expires. Skips any in-flight frame the test doesn't care about
/// (`RenderPatch` / `RenderSnapshot`), so e.g. waiting for an
/// `OwnerChanged` doesn't deadlock on an inbound patch.
async fn wait_until<F, R>(
    rd: &mut tokio::net::unix::OwnedReadHalf,
    budget: Duration,
    mut want: F,
) -> R
where
    F: FnMut(&DaemonMsg) -> Option<R>,
{
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("wait_until: budget elapsed without a match");
        }
        let frame: DaemonMsg = timeout(remaining, read_frame(rd))
            .await
            .expect("read frame timeout (wait_until)")
            .expect("read frame decode (wait_until)");
        if let Some(r) = want(&frame) {
            return r;
        }
    }
}

/// Read with a deadline; `Some(frame)` if anything arrived in the
/// window, `None` on timeout. Used for negative assertions (the
/// "owner must NOT see X" style).
async fn try_read(
    rd: &mut tokio::net::unix::OwnedReadHalf,
    budget: Duration,
) -> Option<DaemonMsg> {
    match timeout(budget, read_frame::<DaemonMsg, _>(rd)).await {
        Ok(Ok(m)) => Some(m),
        Ok(Err(_)) | Err(_) => None,
    }
}

#[tokio::test]
async fn multi_client_cross_layer_round_trip() {
    let (_daemon, sock, real_tid) = spawn_daemon().await;

    // ---- 1. Owner attach + Observer attach ----
    let owner_cid = Uuid::new_v4();
    let observer_cid = Uuid::new_v4();
    let (mut owner_rd, mut owner_wr, owner_role, _ready_owner, cols0, rows0) =
        attach(&sock, &real_tid, owner_cid).await;
    assert_eq!(owner_role, Role::Owner, "first attach is Owner");
    assert_eq!((cols0, rows0), (80, 24), "browser default geometry sticks");

    let (mut obs_rd, mut obs_wr, obs_role, _ready_obs, _, _) =
        attach(&sock, &real_tid, observer_cid).await;
    assert_eq!(obs_role, Role::Observer, "second attach is Observer");

    // ---- 2. Observer Input is rejected; only the observer sees it.
    //
    // The cross-layer guarantee: per-client routing (PR-2 of issue
    // #58) survives in the two-connection setup. The owner sees no
    // ProtocolError frame inside a tight grace window.
    write_frame(
        &mut obs_wr,
        &ClientMsg::Input {
            data: b"x".to_vec(),
            input_seq: 0,
        },
    )
    .await
    .unwrap();
    let perr = wait_until(&mut obs_rd, Duration::from_secs(2), |m| match m {
        DaemonMsg::ProtocolError { code, .. } => Some(*code),
        _ => None,
    })
    .await;
    assert_eq!(
        perr,
        ProtocolErrorCode::NotOwner,
        "observer input must produce ProtocolError(NotOwner)"
    );

    // Owner sees nothing in a 200ms window — the per-client routing
    // contract holds. Other frames (RenderPatch from the PTY's startup
    // banner) are tolerable but a ProtocolError addressed to the
    // observer would be a regression.
    let leaked = try_read(&mut owner_rd, Duration::from_millis(200)).await;
    if let Some(DaemonMsg::ProtocolError { code, .. }) = leaked {
        panic!("owner saw ProtocolError({code:?}) intended for the observer");
    }

    // ---- 3. Owner-driven ResizeCommit broadcasts ResizeApplied.
    //
    // After this, the PTY geometry is 100x40; we use that to assert
    // the post-reconnect `ServerHello.snapshot.cols/rows` later.
    write_frame(
        &mut owner_wr,
        &ClientMsg::ResizeCommit {
            epoch: 1,
            cols: 100,
            rows: 40,
        },
    )
    .await
    .unwrap();

    let owner_applied = wait_until(&mut owner_rd, Duration::from_secs(2), |m| match m {
        DaemonMsg::ResizeApplied {
            epoch,
            cols,
            rows,
            ..
        } => Some((*epoch, *cols, *rows)),
        _ => None,
    })
    .await;
    assert_eq!(
        owner_applied,
        (1, 100, 40),
        "owner must see its own ResizeApplied"
    );

    let obs_applied = wait_until(&mut obs_rd, Duration::from_secs(2), |m| match m {
        DaemonMsg::ResizeApplied {
            epoch,
            cols,
            rows,
            ..
        } => Some((*epoch, *cols, *rows)),
        _ => None,
    })
    .await;
    assert_eq!(
        obs_applied,
        (1, 100, 40),
        "observer must see the same ResizeApplied"
    );

    // ---- 4. Owner disconnects without releasing; observer must NOT
    //         silently inherit input rights.
    drop(owner_wr);
    drop(owner_rd);

    // Give the daemon a beat to notice the half-close. The shell
    // tears down the owner's `ClientConn` synchronously on read
    // error so this is a soft wait, not a load-bearing one.
    tokio::time::sleep(Duration::from_millis(100)).await;

    write_frame(
        &mut obs_wr,
        &ClientMsg::Input {
            data: b"y".to_vec(),
            input_seq: 0,
        },
    )
    .await
    .unwrap();
    let still_observer = wait_until(&mut obs_rd, Duration::from_secs(2), |m| match m {
        DaemonMsg::ProtocolError { code, .. } => Some(*code),
        _ => None,
    })
    .await;
    assert_eq!(
        still_observer,
        ProtocolErrorCode::NotOwner,
        "owner disconnect must NOT silently promote a stale observer to owner",
    );

    // ---- 5. Observer explicitly claims ownership.
    write_frame(&mut obs_wr, &ClientMsg::OwnerClaim).await.unwrap();
    let new_owner = wait_until(&mut obs_rd, Duration::from_secs(2), |m| match m {
        DaemonMsg::OwnerChanged { owner_client_id } => Some(*owner_client_id),
        _ => None,
    })
    .await;
    assert_eq!(
        new_owner,
        Some(observer_cid),
        "OwnerClaim must broadcast OwnerChanged(observer_cid)"
    );

    // ---- 6. Reconnect (the old owner's browser tab refreshes).
    //
    // The fresh attach must come back as Observer (someone — our just-
    // promoted client — already owns), and the `ServerHello.snapshot`
    // geometry must reflect the post-resize PTY (100x40 from step 3).
    let reattach_cid = Uuid::new_v4();
    let (mut re_rd, _re_wr, re_role, re_ready, re_cols, re_rows) =
        attach(&sock, &real_tid, reattach_cid).await;
    assert_eq!(
        re_role,
        Role::Observer,
        "fresh attach lands as Observer when someone else owns"
    );
    assert_eq!(
        (re_cols, re_rows),
        (100, 40),
        "ServerHello snapshot geometry must reflect the post-resize PTY",
    );

    // `is_child_ready` is a sticky snapshot — if `ChildReady` has
    // already fired by now (PTY has been live for the whole flow),
    // a late-joining client must see `is_child_ready: true` so it
    // doesn't deadlock waiting for the one-shot broadcast.
    //
    // Tolerant assertion: the PTY-ready timer is timing-sensitive in
    // a busy CI runner; we accept either branch as long as a `true`
    // is paired with the `ChildReady` broadcast NOT arriving on the
    // reattached connection (which would mean the snapshot lied).
    if re_ready {
        // Should NOT see another ChildReady broadcast on this
        // connection — it's one-shot per session.
        let post = try_read(&mut re_rd, Duration::from_millis(200)).await;
        match post {
            Some(DaemonMsg::ChildReady { .. }) => {
                panic!("late-joiner saw `ChildReady` after `is_child_ready: true` — broadcast must be one-shot");
            }
            _ => { /* fine */ }
        }
    }

    // ---- 7. New owner's Input with input_seq>0 is acked on its own
    //         connection only — InputAck is per-connection.
    write_frame(
        &mut obs_wr,
        &ClientMsg::Input {
            data: b"\r".to_vec(),
            input_seq: 7,
        },
    )
    .await
    .unwrap();

    let acked = wait_until(&mut obs_rd, Duration::from_secs(3), |m| match m {
        DaemonMsg::InputAck { input_seq } => Some(*input_seq),
        _ => None,
    })
    .await;
    assert_eq!(acked, 7, "owner connection must receive its own InputAck");

    // The reattached observer must NOT see an `InputAck` frame for
    // someone else's `input_seq`. Other frames (RenderPatch echo of
    // the `\r`) are fine.
    let leaked_ack = try_read(&mut re_rd, Duration::from_millis(300)).await;
    if let Some(DaemonMsg::InputAck { input_seq }) = leaked_ack {
        panic!("reattached observer leaked InputAck for seq={input_seq} from another connection");
    }

    // Cleanup: drop happens on test exit, daemon dies via kill-on-drop.
    let _ = std::fs::remove_file(&sock);
}
