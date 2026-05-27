//! End-to-end regression test for issue #359. When an observer mounts it
//! may re-POST the daemon's current spawn-time terminal theme from #177;
//! that matching `TerminalThemeUpdate` is benign and must be silently
//! dropped instead of producing `ProtocolError(NotOwner)` or tearing down
//! the observer connection.

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

#[tokio::test]
async fn observer_matching_theme_update_is_silently_dropped() {
    let daemon_bin = env!("CARGO_BIN_EXE_calm-session-daemon");
    let id = Uuid::new_v4();
    let sock = std::env::temp_dir().join(format!("calm-observer-theme-{id}.sock"));
    let _ = std::fs::remove_file(&sock);

    // Spawn the daemon under `sleep 60` so the PTY child sticks around
    // long enough for both clients to exchange frames.
    let mut child = Command::new(daemon_bin)
        .args(["--mode", "terminal"])
        .args(["--id", &id.to_string()])
        .args(["--sock", &sock.to_string_lossy()])
        // Per the daemon's own CLI semantics, `--id` is also re-used as
        // the terminal_id by clients. We assert TID == id.to_string()
        // by sending a matching ClientHello.
        // #177 PR2: terminal-mode daemon now requires theme RGB.
        // Placeholders — this test doesn't exercise OSC replies.
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

    // The handshake's terminal_id must equal the daemon's `--id`.
    let real_tid = id.to_string();

    // First client: becomes Owner (first attach with no prior owner).
    let owner_stream = UnixStream::connect(&sock).await.expect("owner connect");
    let (mut owner_rd, mut owner_wr) = owner_stream.into_split();
    let owner_cid = Uuid::new_v4();
    let owner_hello = ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: real_tid.clone(),
        client_id: owner_cid,
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
    };
    write_frame(&mut owner_wr, &owner_hello).await.unwrap();
    let server_hello_owner: DaemonMsg = timeout(Duration::from_secs(2), read_frame(&mut owner_rd))
        .await
        .expect("owner hello read timeout")
        .expect("owner hello decode");
    let owner_role = match server_hello_owner {
        DaemonMsg::ServerHello { client_role, .. } => client_role,
        other => panic!("expected ServerHello from owner connect, got {other:?}"),
    };
    assert_eq!(owner_role, Role::Owner);

    // Second client: defaults to Observer.
    let observer_stream = UnixStream::connect(&sock).await.expect("observer connect");
    let (mut observer_rd, mut observer_wr) = observer_stream.into_split();
    let observer_cid = Uuid::new_v4();
    let observer_hello = ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: real_tid.clone(),
        client_id: observer_cid,
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
    };
    write_frame(&mut observer_wr, &observer_hello)
        .await
        .unwrap();
    let server_hello_observer: DaemonMsg =
        timeout(Duration::from_secs(2), read_frame(&mut observer_rd))
            .await
            .expect("observer hello read timeout")
            .expect("observer hello decode");
    let observer_role = match server_hello_observer {
        DaemonMsg::ServerHello { client_role, .. } => client_role,
        other => panic!("expected ServerHello from observer connect, got {other:?}"),
    };
    assert_eq!(observer_role, Role::Observer);

    // Observer sends the benign #177 mount-time theme re-POST. Because
    // it matches the daemon's current spawn colors, issue #359 requires
    // the daemon to drop it silently even though the client is observer.
    write_frame(
        &mut observer_wr,
        &ClientMsg::TerminalThemeUpdate {
            fg: (216, 219, 226),
            bg: (15, 20, 24),
        },
    )
    .await
    .unwrap();

    let observer_theme_read = timeout(
        Duration::from_millis(400),
        read_frame::<DaemonMsg, _>(&mut observer_rd),
    )
    .await;
    match observer_theme_read {
        Err(_) => { /* timeout — no frame arrived; pass */ }
        Ok(Ok(DaemonMsg::ProtocolError { code, message, .. })) => {
            panic!(
                "issue #359 regression: observer matching TerminalThemeUpdate produced \
                 ProtocolError({code:?}): {message}"
            );
        }
        Ok(Ok(other)) => {
            eprintln!("(observer saw unrelated frame {other:?} — not a #359 ProtocolError)");
        }
        Ok(Err(e)) => {
            eprintln!("(observer read errored after matching theme update: {e:?})");
        }
    }

    // Prove the observer connection is still live and ownership checks
    // still apply for real mutations.
    write_frame(
        &mut observer_wr,
        &ClientMsg::Input {
            data: b"x".to_vec(),
            input_seq: 0,
        },
    )
    .await
    .unwrap();

    let observer_frame: DaemonMsg = timeout(Duration::from_secs(2), read_frame(&mut observer_rd))
        .await
        .expect("observer frame read timeout after illegal Input")
        .expect("observer frame decode after illegal Input");
    match observer_frame {
        DaemonMsg::ProtocolError { code, message, .. } => {
            assert_eq!(code, ProtocolErrorCode::NotOwner);
            assert!(
                message.contains("Input requires owner role"),
                "expected NotOwner from illegal Input, got message: {message}"
            );
        }
        other => panic!("expected ProtocolError(NotOwner) on observer Input, got {other:?}"),
    }

    // Owner must NOT receive a ProtocolError aimed at the observer.
    let owner_read = timeout(
        Duration::from_millis(300),
        read_frame::<DaemonMsg, _>(&mut owner_rd),
    )
    .await;
    match owner_read {
        Err(_) => { /* timeout — no frame arrived; pass */ }
        Ok(Ok(DaemonMsg::ProtocolError { .. })) => {
            panic!("owner saw a ProtocolError aimed at the observer (leak)");
        }
        Ok(Ok(other)) => {
            eprintln!("(owner saw unrelated frame {other:?} — not the leak we test)");
        }
        Ok(Err(e)) => {
            eprintln!("(owner read errored: {e:?} — not the leak)");
        }
    }

    // Clean up.
    let _ = child.kill().await;
    let _ = std::fs::remove_file(&sock);
}
