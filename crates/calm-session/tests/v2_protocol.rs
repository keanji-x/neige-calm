//! Unit tests for the v2 terminal-mode protocol state machine.
//!
//! Each case feeds one or more frames into a fresh
//! [`TerminalSessionState`] + [`OwnerRegistry`] and asserts on the
//! [`Effect`] list returned. No PTY, no tokio runtime, no socket — these
//! complete in microseconds.

use calm_session::terminal_session::{
    ByteRing, Effect, OwnerRegistry, PtyBroadcaster, SessionContext, TerminalSessionState,
};
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION,
    ProtocolErrorCode, PtySize, RenderEncoding, Role,
};
use uuid::Uuid;

const TID: &str = "terminal-fixture";

fn ctx<'a>(broadcaster: &PtyBroadcaster, session_id: Uuid) -> SessionContext<'a> {
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
        // PtyBroadcaster doesn't track child-readiness — the legacy
        // unit-test fixture defaults to `false`, matching the safe
        // wait-for-ready posture an older serializer would produce.
        is_child_ready: false,
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

/// Drive a fresh client through a successful handshake and return its
/// state + the broadcaster used to seed it. Used by tests that want to
/// assert post-handshake behaviour.
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
    // Sanity: handshake must have succeeded with ResizePty + ServerHello.
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SendToClient(DaemonMsg::ServerHello { .. }))),
        "expected ServerHello in handshake effects, got {effects:?}"
    );
    assert!(state.is_attached());
    (state, broadcaster)
}

// ---- Handshake ---------------------------------------------------------

#[test]
fn client_hello_returns_server_hello_with_snapshot() {
    let mut broadcaster = PtyBroadcaster::new(1024);
    // Seed some PTY output so the snapshot has data.
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
    // ResizePty drives the daemon master to the desired client viewport.
    let resize = effects
        .iter()
        .find(|e| matches!(e, Effect::ResizePty { .. }))
        .expect("expected ResizePty");
    assert!(matches!(
        resize,
        Effect::ResizePty {
            cols: 132,
            rows: 50
        }
    ));
    // ServerHello carries the seeded snapshot bytes.
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
fn first_frame_not_client_hello_yields_protocol_error_bad_handshake() {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut registry = OwnerRegistry::new();
    let mut state = TerminalSessionState::new();
    let session_id = Uuid::new_v4();

    let effects = state.on_client_frame(
        ClientMsg::Input(b"oops".to_vec()),
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
    let _ = observer_state.on_client_frame(
        hello(observer_id, TID),
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    assert_eq!(observer_state.role(), Some(Role::Observer));
    // Registry still points at the original owner.
    assert_eq!(registry.current_owner(), Some(owner_id));
}

// ---- Role enforcement --------------------------------------------------

#[test]
fn observer_input_yields_not_owner() {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut registry = OwnerRegistry::new();
    let session_id = Uuid::new_v4();

    // Pre-register an owner so the next attach defaults to Observer.
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
        ClientMsg::Input(b"keys".to_vec()),
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
    // PR-1 review nit #3: rejection must NOT also fire any of the
    // side-effecting effects — observers' input/resize/kill must be inert.
    assert!(
        !effects.iter().any(|e| matches!(
            e,
            Effect::WriteToPty(_) | Effect::ResizePty { .. } | Effect::KillChild
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
            Effect::WriteToPty(_) | Effect::ResizePty { .. } | Effect::KillChild
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
            Effect::WriteToPty(_) | Effect::ResizePty { .. } | Effect::KillChild
        )),
        "observer Kill must not emit IO effects, got {effects:?}"
    );
}

// ---- Ownership transitions --------------------------------------------

#[test]
fn owner_claim_changes_role_and_broadcasts() {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut registry = OwnerRegistry::new();
    let session_id = Uuid::new_v4();
    let original_owner = Uuid::new_v4();
    let _ = registry.on_attach(original_owner, None);

    // Observer claims.
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

// ---- Resize epoch -----------------------------------------------------

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

    // Bump epoch to 5.
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

    // Stale epoch=3 must be silently dropped.
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

// ---- PtyBroadcaster v2 shape ------------------------------------------

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
    // Append 60 bytes then 80 bytes into a 100-byte budget: the first chunk
    // is dropped, snapshot is just the second.
    let mut ring = ByteRing::new(100);
    ring.append(vec![b'a'; 60]);
    ring.append(vec![b'b'; 80]);

    let snap = ring.snapshot();
    assert!(snap.len() <= 100, "snapshot was {} bytes", snap.len());
    assert_eq!(snap, vec![b'b'; 80]);
}

// ---- kernel_originated_input capability --------------------------------
//
// PR-2.5: an observer that advertises `kernel_originated_input = true` in
// its ClientHello is allowed to send `Input` frames as if it were owner.
// Other owner-gated frames (ResizeCommit, Kill) stay owner-only.

/// Drive a fresh state through a handshake as Observer, with the
/// `kernel_originated_input` flag turned on. Returns the attached state
/// + the broadcaster used. Caller must have already pre-registered an
///   owner in `registry` so this attach defaults to Observer.
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
    // Pre-register the original owner so the kernel client attaches as
    // Observer.
    let _ = registry.on_attach(Uuid::new_v4(), None);

    let (mut state, broadcaster) =
        attached_observer_with_kernel_input(&mut registry, Uuid::new_v4());
    assert_eq!(state.role(), Some(Role::Observer));
    let session_id = Uuid::new_v4();

    let effects = state.on_client_frame(
        ClientMsg::Input(b"keys".to_vec()),
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    // MUST route the bytes to the PTY...
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::WriteToPty(b) if b == b"keys")),
        "kernel-input observer Input should produce WriteToPty, got {effects:?}"
    );
    // ...and MUST NOT emit a NotOwner error.
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
