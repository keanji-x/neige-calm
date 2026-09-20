//! Unit tests for the v2 terminal-mode protocol state machine: frames in, [`Effect`] list out. No PTY, no tokio, no socket.

use calm_session::terminal_session::{
    ByteRing, Effect, OwnerRegistry, PtyBroadcaster, SessionContext, TerminalSessionState,
};
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION,
    ProtocolErrorCode, PtySize, RenderEncoding, Role,
};
use uuid::Uuid;

const TID: &str = "terminal-fixture";

#[path = "cases/owner_leases.rs"]
mod owner_leases;

fn ctx<'a>(broadcaster: &PtyBroadcaster, session_id: Uuid) -> SessionContext<'a> {
    ctx_with_colors(broadcaster, session_id, None, None)
}

/// Like [`ctx`] but lets a test pin the daemon's "current" OSC 10/11 colors.
fn ctx_with_colors<'a>(
    broadcaster: &PtyBroadcaster,
    session_id: Uuid,
    current_default_fg: Option<(u8, u8, u8)>,
    current_default_bg: Option<(u8, u8, u8)>,
) -> SessionContext<'a> {
    SessionContext {
        terminal_id: TID,
        session_id,
        pty_size: PtySize {
            cols: 80,
            rows: 24,
            pixel_width: None,
            pixel_height: None,
        },
        pty_seq_head: broadcaster.pty_seq_head(),
        pty_seq_tail: broadcaster.pty_seq(),
        render_rev: broadcaster.render_rev(),
        // PtyBroadcaster doesn't track child-readiness; `false` is the safe wait-for-ready posture.
        is_child_ready: false,
        current_default_fg,
        current_default_bg,
    }
}

fn hello(client_id: Uuid, terminal_id: &str) -> ClientMsg {
    ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: terminal_id.to_string(),
        client_id,
        desired_size: PtySize {
            cols: 132,
            rows: 50,
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

fn hello_with<F: FnOnce(&mut ClientMsg)>(client_id: Uuid, f: F) -> ClientMsg {
    let mut h = hello(client_id, TID);
    f(&mut h);
    h
}

/// Drive a fresh client through a successful handshake and return its state + the broadcaster used to seed it.
fn attached_owner(
    registry: &mut OwnerRegistry,
    client_id: Uuid,
) -> (TerminalSessionState, PtyBroadcaster) {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut state = TerminalSessionState::new();
    let session_id = Uuid::new_v4();
    let effects = state.on_client_frame(
        hello(client_id, TID),
        broadcaster.buffer(),
        registry,
        &ctx(&broadcaster, session_id),
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SendToClient(DaemonMsg::ServerHello { .. }))),
        "expected ServerHello in handshake effects, got {effects:?}"
    );
    assert!(state.is_attached());
    (state, broadcaster)
}

#[test]
fn client_hello_returns_server_hello_with_snapshot() {
    let mut broadcaster = PtyBroadcaster::new(1024);
    let _ = broadcaster.on_pty_chunk(b"prior output".to_vec());

    let mut registry = OwnerRegistry::new();
    let mut state = TerminalSessionState::new();
    let session_id = Uuid::new_v4();
    let client_id = Uuid::new_v4();

    let effects = state.on_client_frame(
        hello(client_id, TID),
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    assert!(state.is_attached());
    assert_eq!(state.role(), Some(Role::Owner));
    assert_eq!(state.client_id(), Some(client_id));
    assert_eq!(registry.current_owner(), Some(client_id));
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::ResizePty { .. })),
        "ClientHello must not destructively resize before recovery snapshot"
    );
    let server_hello = effects
        .iter()
        .find_map(|e| match e {
            Effect::SendToClient(DaemonMsg::ServerHello { snapshot, .. }) => Some(snapshot),
            _ => None,
        })
        .expect("expected SendToClient(ServerHello)");
    assert_eq!(server_hello.data, b"prior output");
    assert_eq!(server_hello.encoding, RenderEncoding::Vt);
}

#[test]
fn observer_client_hello_does_not_resize_pty() {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut registry = OwnerRegistry::new();
    let mut state = TerminalSessionState::new();
    let session_id = Uuid::new_v4();
    let observer_id = Uuid::new_v4();

    let msg = hello_with(observer_id, |m| {
        if let ClientMsg::ClientHello { role_hint, .. } = m {
            *role_hint = Some(Role::Observer);
        }
    });
    let effects = state.on_client_frame(
        msg,
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    assert_eq!(state.role(), Some(Role::Observer));
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::ResizePty { .. })),
        "observer handshake must not resize the shared PTY, got {effects:?}"
    );
    let server_hello_pty_size = effects
        .iter()
        .find_map(|e| match e {
            Effect::SendToClient(DaemonMsg::ServerHello { pty_size, .. }) => Some(*pty_size),
            _ => None,
        })
        .expect("expected SendToClient(ServerHello)");
    assert_eq!(
        server_hello_pty_size,
        PtySize {
            cols: 80,
            rows: 24,
            pixel_width: None,
            pixel_height: None,
        },
        "observers should see the daemon's current PTY size"
    );
    assert_eq!(registry.current_owner(), None);
}

#[test]
fn first_frame_not_client_hello_yields_protocol_error_bad_handshake() {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut registry = OwnerRegistry::new();
    let mut state = TerminalSessionState::new();
    let session_id = Uuid::new_v4();

    let effects = state.on_client_frame(
        ClientMsg::Input {
            data: b"oops".to_vec(),
            input_seq: 0,
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    assert!(!state.is_attached());
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SendProtocolError {
                code: ProtocolErrorCode::BadHandshake,
                ..
            }
        )),
        "expected BadHandshake, got {effects:?}"
    );
    assert!(effects.iter().any(|e| matches!(e, Effect::CloseConnection)));
}

#[test]
fn client_hello_wrong_protocol_version_yields_unsupported_version() {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut registry = OwnerRegistry::new();
    let mut state = TerminalSessionState::new();
    let session_id = Uuid::new_v4();

    let client_id = Uuid::new_v4();
    let msg = hello_with(client_id, |m| {
        if let ClientMsg::ClientHello {
            protocol_version, ..
        } = m
        {
            *protocol_version = 999;
        }
    });

    let effects = state.on_client_frame(
        msg,
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    assert!(!state.is_attached());
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SendProtocolError {
                code: ProtocolErrorCode::UnsupportedVersion,
                expected_version: Some(v),
                ..
            } if *v == PROTOCOL_VERSION
        )),
        "expected UnsupportedVersion w/ expected_version, got {effects:?}"
    );
}

#[test]
fn client_hello_wrong_terminal_id_yields_bad_handshake() {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut registry = OwnerRegistry::new();
    let mut state = TerminalSessionState::new();
    let session_id = Uuid::new_v4();

    let msg = hello(Uuid::new_v4(), "some-other-terminal");
    let effects = state.on_client_frame(
        msg,
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    assert!(!state.is_attached());
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SendProtocolError {
                code: ProtocolErrorCode::BadHandshake,
                ..
            }
        )),
        "expected BadHandshake on terminal_id mismatch, got {effects:?}"
    );
}

#[test]
fn client_hello_missing_vt_encoding_yields_unsupported_encoding() {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut registry = OwnerRegistry::new();
    let mut state = TerminalSessionState::new();
    let session_id = Uuid::new_v4();

    let msg = hello_with(Uuid::new_v4(), |m| {
        if let ClientMsg::ClientHello { capabilities, .. } = m {
            capabilities.render_encodings.clear();
        }
    });

    let effects = state.on_client_frame(
        msg,
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    assert!(!state.is_attached());
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SendProtocolError {
                code: ProtocolErrorCode::UnsupportedEncoding,
                ..
            }
        )),
        "expected UnsupportedEncoding, got {effects:?}"
    );
}

#[test]
fn second_client_becomes_observer_by_default() {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut registry = OwnerRegistry::new();
    let session_id = Uuid::new_v4();

    let owner_id = Uuid::new_v4();
    let mut owner_state = TerminalSessionState::new();
    let _ = owner_state.on_client_frame(
        hello(owner_id, TID),
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    assert_eq!(owner_state.role(), Some(Role::Owner));

    let observer_id = Uuid::new_v4();
    let mut observer_state = TerminalSessionState::new();
    let observer_effects = observer_state.on_client_frame(
        hello(observer_id, TID),
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    assert_eq!(observer_state.role(), Some(Role::Observer));
    assert!(
        !observer_effects
            .iter()
            .any(|e| matches!(e, Effect::ResizePty { .. })),
        "second attach observer must not resize the shared PTY, got {observer_effects:?}"
    );
    assert_eq!(registry.current_owner(), Some(owner_id));
}

#[test]
fn observer_input_yields_not_owner() {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut registry = OwnerRegistry::new();
    let session_id = Uuid::new_v4();

    let _ = registry.on_attach(Uuid::new_v4(), None);

    let observer_id = Uuid::new_v4();
    let mut state = TerminalSessionState::new();
    let _ = state.on_client_frame(
        hello(observer_id, TID),
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    assert_eq!(state.role(), Some(Role::Observer));

    let effects = state.on_client_frame(
        ClientMsg::Input {
            data: b"keys".to_vec(),
            input_seq: 0,
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SendProtocolError {
                code: ProtocolErrorCode::NotOwner,
                ..
            }
        )),
        "expected NotOwner, got {effects:?}"
    );
    // Rejection must NOT also fire any side-effecting effect — observers' input/resize/kill must be inert.
    assert!(
        !effects.iter().any(|e| matches!(
            e,
            Effect::WriteToPty { .. } | Effect::ResizePty { .. } | Effect::KillChild
        )),
        "observer rejection must not emit IO effects, got {effects:?}"
    );
}

#[test]
fn observer_resize_commit_yields_not_owner() {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut registry = OwnerRegistry::new();
    let session_id = Uuid::new_v4();
    let _ = registry.on_attach(Uuid::new_v4(), None);

    let mut state = TerminalSessionState::new();
    let _ = state.on_client_frame(
        hello(Uuid::new_v4(), TID),
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    let effects = state.on_client_frame(
        ClientMsg::ResizeCommit {
            epoch: 1,
            cols: 100,
            rows: 30,
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SendProtocolError {
                code: ProtocolErrorCode::NotOwner,
                ..
            }
        )),
        "expected NotOwner for observer resize, got {effects:?}"
    );
    assert!(
        !effects.iter().any(|e| matches!(
            e,
            Effect::WriteToPty { .. } | Effect::ResizePty { .. } | Effect::KillChild
        )),
        "observer ResizeCommit must not emit IO effects, got {effects:?}"
    );
}

#[test]
fn observer_kill_yields_not_owner() {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut registry = OwnerRegistry::new();
    let session_id = Uuid::new_v4();
    let _ = registry.on_attach(Uuid::new_v4(), None);

    let mut state = TerminalSessionState::new();
    let _ = state.on_client_frame(
        hello(Uuid::new_v4(), TID),
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    let effects = state.on_client_frame(
        ClientMsg::Kill,
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SendProtocolError {
                code: ProtocolErrorCode::NotOwner,
                ..
            }
        )),
        "expected NotOwner for observer kill, got {effects:?}"
    );
    assert!(
        !effects.iter().any(|e| matches!(
            e,
            Effect::WriteToPty { .. } | Effect::ResizePty { .. } | Effect::KillChild
        )),
        "observer Kill must not emit IO effects, got {effects:?}"
    );
}

#[test]
fn owner_claim_changes_role_and_broadcasts() {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut registry = OwnerRegistry::new();
    let session_id = Uuid::new_v4();
    let original_owner = Uuid::new_v4();
    let _ = registry.on_attach(original_owner, None);

    let claimant = Uuid::new_v4();
    let mut state = TerminalSessionState::new();
    let _ = state.on_client_frame(
        hello(claimant, TID),
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    assert_eq!(state.role(), Some(Role::Observer));

    let effects = state.on_client_frame(
        ClientMsg::OwnerClaim,
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    assert_eq!(state.role(), Some(Role::Owner));
    assert_eq!(registry.current_owner(), Some(claimant));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::AssignOwner(Some(cid)) if *cid == claimant)),
        "expected AssignOwner(Some(claimant)), got {effects:?}"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::BroadcastOwnerChanged(Some(cid)) if *cid == claimant)),
        "expected BroadcastOwnerChanged(Some(claimant)), got {effects:?}"
    );
}

#[test]
fn owner_release_clears_owner() {
    let mut registry = OwnerRegistry::new();
    let owner_id = Uuid::new_v4();
    let (mut state, broadcaster) = attached_owner(&mut registry, owner_id);
    let session_id = Uuid::new_v4();

    let effects = state.on_client_frame(
        ClientMsg::OwnerRelease,
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    assert_eq!(state.role(), Some(Role::Observer));
    assert_eq!(registry.current_owner(), None);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::BroadcastOwnerChanged(None))),
        "expected BroadcastOwnerChanged(None), got {effects:?}"
    );
}

#[test]
fn resize_commit_increments_epoch_and_yields_resize_applied() {
    let mut registry = OwnerRegistry::new();
    let (mut state, broadcaster) = attached_owner(&mut registry, Uuid::new_v4());
    let session_id = Uuid::new_v4();

    let effects = state.on_client_frame(
        ClientMsg::ResizeCommit {
            epoch: 1,
            cols: 100,
            rows: 30,
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    assert_eq!(state.resize_epoch(), 1);
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::ResizePty {
                cols: 100,
                rows: 30
            }
        )),
        "expected ResizePty, got {effects:?}"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::Broadcast(DaemonMsg::ResizeApplied {
                epoch: 1,
                cols: 100,
                rows: 30,
                ..
            })
        )),
        "expected Broadcast(ResizeApplied), got {effects:?}"
    );
}

#[test]
fn stale_resize_epoch_ignored() {
    let mut registry = OwnerRegistry::new();
    let (mut state, broadcaster) = attached_owner(&mut registry, Uuid::new_v4());
    let session_id = Uuid::new_v4();

    let _ = state.on_client_frame(
        ClientMsg::ResizeCommit {
            epoch: 5,
            cols: 100,
            rows: 30,
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    assert_eq!(state.resize_epoch(), 5);

    let effects = state.on_client_frame(
        ClientMsg::ResizeCommit {
            epoch: 3,
            cols: 999,
            rows: 999,
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    assert!(
        effects.is_empty(),
        "expected no effects for stale epoch, got {effects:?}"
    );
    assert_eq!(state.resize_epoch(), 5);

    // Equal epoch is also stale (strict >).
    let effects = state.on_client_frame(
        ClientMsg::ResizeCommit {
            epoch: 5,
            cols: 999,
            rows: 999,
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    assert!(
        effects.is_empty(),
        "expected no effects for equal epoch, got {effects:?}"
    );
}

#[test]
fn pty_chunk_broadcasts_render_patch_with_seq_and_rev() {
    let mut pb = PtyBroadcaster::new(1024);

    let effects = pb.on_pty_chunk(b"abc".to_vec());

    assert_eq!(pb.pty_seq(), 1);
    assert_eq!(pb.render_rev(), 1);
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::Broadcast(DaemonMsg::RenderPatch(p))
                if p.pty_seq == 1
                && p.render_rev == 1
                && p.prev_render_rev == 0
                && p.encoding == RenderEncoding::Vt
                && p.data == b"abc"
        )),
        "expected RenderPatch v2, got {effects:?}"
    );
}

#[test]
fn child_exit_broadcasts_terminal_exited_with_cursors() {
    let mut pb = PtyBroadcaster::new(1024);
    let _ = pb.on_pty_chunk(b"out".to_vec());
    let effects = pb.on_child_exit(Some(7));

    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::Broadcast(DaemonMsg::TerminalExited {
                code: Some(7),
                pty_seq: 1,
                render_rev: 1,
            })
        )),
        "expected TerminalExited carrying cursors, got {effects:?}"
    );
}

#[test]
fn byte_ring_evicts_oldest_chunk_when_over_budget() {
    let mut ring = ByteRing::new(100);
    ring.append(vec![b'a'; 60]);
    ring.append(vec![b'b'; 80]);

    let snap = ring.snapshot();
    assert!(snap.len() <= 100, "snapshot was {} bytes", snap.len());
    assert_eq!(snap, vec![b'b'; 80]);
}

/// Handshake as Observer with `kernel_originated_input` on; the caller must have pre-registered an owner in `registry`.
fn attached_observer_with_kernel_input(
    registry: &mut OwnerRegistry,
    client_id: Uuid,
) -> (TerminalSessionState, PtyBroadcaster) {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut state = TerminalSessionState::new();
    let session_id = Uuid::new_v4();
    let hello = hello_with(client_id, |m| {
        if let ClientMsg::ClientHello { capabilities, .. } = m {
            capabilities.kernel_originated_input = true;
        }
    });
    let effects = state.on_client_frame(
        hello,
        broadcaster.buffer(),
        registry,
        &ctx(&broadcaster, session_id),
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SendToClient(DaemonMsg::ServerHello { .. }))),
        "expected ServerHello in handshake effects, got {effects:?}"
    );
    assert!(state.is_attached());
    (state, broadcaster)
}

#[test]
fn observer_with_kernel_input_capability_can_send_input() {
    let mut registry = OwnerRegistry::new();
    let _ = registry.on_attach(Uuid::new_v4(), None);

    let (mut state, broadcaster) =
        attached_observer_with_kernel_input(&mut registry, Uuid::new_v4());
    assert_eq!(state.role(), Some(Role::Observer));
    let session_id = Uuid::new_v4();

    let effects = state.on_client_frame(
        ClientMsg::Input {
            data: b"keys".to_vec(),
            input_seq: 0,
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::WriteToPty { data, .. } if data == b"keys")),
        "kernel-input observer Input should produce WriteToPty, got {effects:?}"
    );
    assert!(
        !effects.iter().any(|e| matches!(
            e,
            Effect::SendProtocolError {
                code: ProtocolErrorCode::NotOwner,
                ..
            }
        )),
        "kernel-input observer Input must not raise NotOwner, got {effects:?}"
    );
}

#[test]
fn observer_with_kernel_input_capability_still_blocked_on_resize_commit() {
    let mut registry = OwnerRegistry::new();
    let _ = registry.on_attach(Uuid::new_v4(), None);

    let (mut state, broadcaster) =
        attached_observer_with_kernel_input(&mut registry, Uuid::new_v4());
    let session_id = Uuid::new_v4();

    let effects = state.on_client_frame(
        ClientMsg::ResizeCommit {
            epoch: 1,
            cols: 100,
            rows: 30,
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SendProtocolError {
                code: ProtocolErrorCode::NotOwner,
                ..
            }
        )),
        "ResizeCommit with kernel_originated_input must still raise NotOwner, got {effects:?}"
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::ResizePty { .. })),
        "ResizeCommit with kernel_originated_input must not actually resize, got {effects:?}"
    );
}

#[test]
fn observer_with_kernel_input_capability_still_blocked_on_kill() {
    let mut registry = OwnerRegistry::new();
    let _ = registry.on_attach(Uuid::new_v4(), None);

    let (mut state, broadcaster) =
        attached_observer_with_kernel_input(&mut registry, Uuid::new_v4());
    let session_id = Uuid::new_v4();

    let effects = state.on_client_frame(
        ClientMsg::Kill,
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SendProtocolError {
                code: ProtocolErrorCode::NotOwner,
                ..
            }
        )),
        "Kill with kernel_originated_input must still raise NotOwner, got {effects:?}"
    );
    assert!(
        !effects.iter().any(|e| matches!(e, Effect::KillChild)),
        "Kill with kernel_originated_input must not actually KillChild, got {effects:?}"
    );
}

#[test]
fn owner_terminal_theme_update_yields_terminal_theme_update_effect() {
    let mut registry = OwnerRegistry::new();
    let (mut state, broadcaster) = attached_owner(&mut registry, Uuid::new_v4());
    let session_id = Uuid::new_v4();

    let effects = state.on_client_frame(
        ClientMsg::TerminalThemeUpdate {
            fg: (216, 219, 226),
            bg: (15, 20, 24),
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    assert_eq!(
        effects.len(),
        1,
        "expected exactly one effect, got {effects:?}"
    );
    match &effects[0] {
        Effect::TerminalThemeUpdate { fg, bg } => {
            assert_eq!(*fg, (216, 219, 226));
            assert_eq!(*bg, (15, 20, 24));
        }
        other => panic!("expected TerminalThemeUpdate effect, got {other:?}"),
    }
}

#[test]
fn owner_terminal_theme_update_matching_current_colors_is_suppressed() {
    // The New-terminal mount always re-POSTs the host theme the daemon was already spawned with; a matching update must emit NOTHING.
    let mut registry = OwnerRegistry::new();
    let (mut state, broadcaster) = attached_owner(&mut registry, Uuid::new_v4());
    let session_id = Uuid::new_v4();

    let fg = (216, 219, 226);
    let bg = (15, 20, 24);
    let effects = state.on_client_frame(
        ClientMsg::TerminalThemeUpdate { fg, bg },
        broadcaster.buffer(),
        &mut registry,
        &ctx_with_colors(&broadcaster, session_id, Some(fg), Some(bg)),
    );

    assert!(
        effects.is_empty(),
        "matching-color TerminalThemeUpdate must produce no effect, got {effects:?}"
    );
}

#[test]
fn owner_terminal_theme_update_differing_colors_still_emits() {
    let mut registry = OwnerRegistry::new();
    let (mut state, broadcaster) = attached_owner(&mut registry, Uuid::new_v4());
    let session_id = Uuid::new_v4();

    let current_fg = (216, 219, 226);
    let current_bg = (15, 20, 24);
    let new_fg = (42, 47, 58);
    let new_bg = (252, 254, 255);
    let effects = state.on_client_frame(
        ClientMsg::TerminalThemeUpdate {
            fg: new_fg,
            bg: new_bg,
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx_with_colors(&broadcaster, session_id, Some(current_fg), Some(current_bg)),
    );

    assert_eq!(effects.len(), 1, "expected one effect, got {effects:?}");
    match &effects[0] {
        Effect::TerminalThemeUpdate { fg, bg } => {
            assert_eq!(*fg, new_fg);
            assert_eq!(*bg, new_bg);
        }
        other => panic!("expected TerminalThemeUpdate effect, got {other:?}"),
    }
}

#[test]
fn owner_terminal_theme_update_emits_when_current_colors_unknown() {
    // With the daemon's current colors unknown (`None`), the update always flows through so a real toggle is never swallowed.
    let mut registry = OwnerRegistry::new();
    let (mut state, broadcaster) = attached_owner(&mut registry, Uuid::new_v4());
    let session_id = Uuid::new_v4();

    let fg = (216, 219, 226);
    let bg = (15, 20, 24);
    let effects = state.on_client_frame(
        ClientMsg::TerminalThemeUpdate { fg, bg },
        broadcaster.buffer(),
        &mut registry,
        &ctx_with_colors(&broadcaster, session_id, None, None),
    );

    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::TerminalThemeUpdate { .. })),
        "unknown-current-color TerminalThemeUpdate must still emit, got {effects:?}"
    );
}

#[test]
fn observer_terminal_theme_update_yields_not_owner() {
    let mut registry = OwnerRegistry::new();
    let _ = registry.on_attach(Uuid::new_v4(), None);

    let broadcaster = PtyBroadcaster::new(1024);
    let mut state = TerminalSessionState::new();
    let session_id = Uuid::new_v4();
    let _ = state.on_client_frame(
        hello(Uuid::new_v4(), TID),
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    assert_eq!(state.role(), Some(Role::Observer));

    let effects = state.on_client_frame(
        ClientMsg::TerminalThemeUpdate {
            fg: (1, 2, 3),
            bg: (4, 5, 6),
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SendProtocolError {
                code: ProtocolErrorCode::NotOwner,
                ..
            }
        )),
        "expected NotOwner for observer TerminalThemeUpdate, got {effects:?}"
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::TerminalThemeUpdate { .. })),
        "observer TerminalThemeUpdate must not produce the actual effect, got {effects:?}"
    );
}

#[test]
fn observer_terminal_theme_update_matching_current_colors_is_suppressed() {
    let mut registry = OwnerRegistry::new();
    let _ = registry.on_attach(Uuid::new_v4(), None);

    let broadcaster = PtyBroadcaster::new(1024);
    let mut state = TerminalSessionState::new();
    let session_id = Uuid::new_v4();
    let _ = state.on_client_frame(
        hello(Uuid::new_v4(), TID),
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    assert_eq!(state.role(), Some(Role::Observer));

    let fg = (216, 219, 226);
    let bg = (15, 20, 24);
    let effects = state.on_client_frame(
        ClientMsg::TerminalThemeUpdate { fg, bg },
        broadcaster.buffer(),
        &mut registry,
        &ctx_with_colors(&broadcaster, session_id, Some(fg), Some(bg)),
    );

    // An observer's benign host-theme re-POST matching the daemon's colors is dropped silently instead of surfacing NotOwner.
    assert!(
        effects.is_empty(),
        "matching-color observer TerminalThemeUpdate must produce no effect, got {effects:?}"
    );
}

#[test]
fn observer_terminal_theme_update_differing_colors_still_yields_not_owner() {
    let mut registry = OwnerRegistry::new();
    let _ = registry.on_attach(Uuid::new_v4(), None);

    let broadcaster = PtyBroadcaster::new(1024);
    let mut state = TerminalSessionState::new();
    let session_id = Uuid::new_v4();
    let _ = state.on_client_frame(
        hello(Uuid::new_v4(), TID),
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    assert_eq!(state.role(), Some(Role::Observer));

    let current_fg = (216, 219, 226);
    let current_bg = (15, 20, 24);
    let requested_fg = (42, 47, 58);
    let requested_bg = (252, 254, 255);
    let effects = state.on_client_frame(
        ClientMsg::TerminalThemeUpdate {
            fg: requested_fg,
            bg: requested_bg,
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx_with_colors(&broadcaster, session_id, Some(current_fg), Some(current_bg)),
    );

    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SendProtocolError {
                code: ProtocolErrorCode::NotOwner,
                ..
            }
        )),
        "expected NotOwner for differing-color observer TerminalThemeUpdate, got {effects:?}"
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::TerminalThemeUpdate { .. })),
        "differing-color observer TerminalThemeUpdate must not produce the actual effect, got {effects:?}"
    );
}

#[test]
fn kernel_input_observer_terminal_theme_update_allowed() {
    // Kernel-private clients carry the same trust posture for theme as they do for Input.
    let mut registry = OwnerRegistry::new();
    let _ = registry.on_attach(Uuid::new_v4(), None);

    let (mut state, broadcaster) =
        attached_observer_with_kernel_input(&mut registry, Uuid::new_v4());
    let session_id = Uuid::new_v4();

    let effects = state.on_client_frame(
        ClientMsg::TerminalThemeUpdate {
            fg: (1, 2, 3),
            bg: (4, 5, 6),
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::TerminalThemeUpdate { .. })),
        "kernel-input observer TerminalThemeUpdate must produce the effect, got {effects:?}"
    );
}

#[test]
fn terminal_theme_update_bincode_roundtrips() {
    // Lock the bincode wire shape so a missed schema bump fails loudly in CI rather than as a silent payload mismatch.
    let original = ClientMsg::TerminalThemeUpdate {
        fg: (1, 2, 3),
        bg: (4, 5, 6),
    };
    let bytes =
        bincode::serde::encode_to_vec(&original, bincode::config::standard()).expect("encode");
    let (decoded, _): (ClientMsg, _) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).expect("decode");
    assert_eq!(decoded, original);
}
