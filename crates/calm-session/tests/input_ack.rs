//! Acceptance tests for `DaemonMsg::InputAck` + per-connection
//! `input_seq` on `ClientMsg::Input` — the deterministic delivery
//! acknowledgement that lets a kernel-originated transient
//! `DaemonClient` (e.g. `inject_stdin`) wait for the PTY write to
//! complete instead of racing a fixed `tokio::time::sleep` close-grace.
//!
//! The state-machine layer (`TerminalSessionState::on_client_frame`) is
//! protocol-only — it doesn't perform the actual PTY write, it just
//! forwards `input_seq` into the `Effect::WriteToPty` it emits. That
//! forwarding is what these tests cover. The shell-side ack-emission
//! point (the PTY-writer thread firing `DaemonMsg::InputAck` after
//! `write_all` returns) is exercised by the existing e2e tests against
//! the real daemon binary; here we keep the contract testable in pure
//! Rust without spawning processes.
//!
//! Design choice (option (b) — see issue #115): `input_seq == 0` means
//! "no ack requested". The daemon writes the bytes and emits no ack
//! frame. Browser-typing path keeps the wire default at 0 to avoid
//! ack-frame noise on the hot path.
//!
//! Closes part of #115.

use calm_session::terminal_session::{
    Effect, OwnerRegistry, PtyBroadcaster, SessionContext, TerminalSessionState,
};
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding, Role,
};
use uuid::Uuid;

const TID: &str = "terminal-fixture";

fn ctx<'a>(broadcaster: &'a PtyBroadcaster, session_id: Uuid) -> SessionContext<'a> {
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
        is_child_ready: false,
        // PtyBroadcaster pre-dates theming; unknown current colors.
        current_default_fg: None,
        current_default_bg: None,
    }
}

fn hello(client_id: Uuid, kernel_input: bool) -> ClientMsg {
    ClientMsg::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        terminal_id: TID.to_string(),
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
            kernel_originated_input: kernel_input,
        },
    }
}

/// Attach an owner via the state machine and drain its handshake
/// effects so the post-handshake input-path is the only thing under
/// test in the following frames.
fn attach_owner() -> (TerminalSessionState, OwnerRegistry, PtyBroadcaster) {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut registry = OwnerRegistry::new();
    let mut state = TerminalSessionState::new();
    let session_id = Uuid::new_v4();
    let _ = state.on_client_frame(
        hello(Uuid::new_v4(), false),
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    assert_eq!(state.role(), Some(Role::Owner));
    (state, registry, broadcaster)
}

/// `ClientMsg::Input { data, input_seq: N }` (N > 0) — protocol layer
/// must forward `data` AND `input_seq` into the `Effect::WriteToPty`
/// it emits, so the shell can fire `DaemonMsg::InputAck { input_seq:
/// N }` after the PTY write completes. The state machine itself
/// doesn't emit `InputAck` — that's the shell's job — but the
/// `input_seq` MUST round-trip through the effect.
#[test]
fn owner_input_with_nonzero_seq_forwards_seq_into_write_effect() {
    let (mut state, mut registry, broadcaster) = attach_owner();
    let session_id = Uuid::new_v4();

    let effects = state.on_client_frame(
        ClientMsg::Input {
            data: b"hello".to_vec(),
            input_seq: 7,
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    let write = effects
        .iter()
        .find_map(|e| match e {
            Effect::WriteToPty { data, input_seq } => Some((data.clone(), *input_seq)),
            _ => None,
        })
        .expect("expected Effect::WriteToPty");
    assert_eq!(write.0, b"hello");
    assert_eq!(write.1, 7, "input_seq must round-trip into the effect");
}

/// Two consecutive `Input` frames with seqs `N, N+1` produce two
/// `WriteToPty` effects in the same order. The state machine itself
/// doesn't track or reorder — that's a per-call test, but pairing
/// them in one fixture catches an accidental "state captures the last
/// seq" bug.
#[test]
fn owner_input_two_frames_preserve_seq_order() {
    let (mut state, mut registry, broadcaster) = attach_owner();
    let session_id = Uuid::new_v4();

    let first = state.on_client_frame(
        ClientMsg::Input {
            data: b"a".to_vec(),
            input_seq: 42,
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    let second = state.on_client_frame(
        ClientMsg::Input {
            data: b"b".to_vec(),
            input_seq: 43,
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    let extract = |effs: &[Effect]| -> (Vec<u8>, u64) {
        effs.iter()
            .find_map(|e| match e {
                Effect::WriteToPty { data, input_seq } => Some((data.clone(), *input_seq)),
                _ => None,
            })
            .expect("WriteToPty")
    };
    let (d1, s1) = extract(&first);
    let (d2, s2) = extract(&second);
    assert_eq!(d1, b"a");
    assert_eq!(s1, 42);
    assert_eq!(d2, b"b");
    assert_eq!(s2, 43);
}

/// `ClientMsg::Input { data, input_seq: 0 }` (the wire default for
/// browser clients) still produces a `WriteToPty` effect — the daemon
/// still writes the bytes to the PTY. The shell-side post-write check
/// (`input_seq > 0`) is the only thing that suppresses the
/// `DaemonMsg::InputAck`; the protocol layer is uniform.
#[test]
fn owner_input_seq_zero_still_writes() {
    let (mut state, mut registry, broadcaster) = attach_owner();
    let session_id = Uuid::new_v4();

    let effects = state.on_client_frame(
        ClientMsg::Input {
            data: b"silent".to_vec(),
            input_seq: 0,
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    let write = effects
        .iter()
        .find_map(|e| match e {
            Effect::WriteToPty { data, input_seq } => Some((data.clone(), *input_seq)),
            _ => None,
        })
        .expect("expected Effect::WriteToPty for seq-0 frame too");
    assert_eq!(write.0, b"silent");
    assert_eq!(
        write.1, 0,
        "seq must round-trip as 0 — the shell uses this to skip ack emission"
    );
}

/// Backward compat: an older JSON `Input` frame missing the
/// `input_seq` key MUST decode as `input_seq: 0` thanks to
/// `#[serde(default)]`. This is what hand-rolled and pre-#115 callers
/// would have sent; the daemon must accept it and treat it identically
/// to a frame with `input_seq: 0`.
///
/// We synthesize the older payload by hand-rolling JSON (the current
/// `ClientMsg` always serializes both keys), then deserialize through
/// `serde_json` — the same hop the WS bridge uses on the up arm.
#[test]
fn input_decodes_missing_input_seq_as_zero() {
    // Hand-rolled JSON missing the `input_seq` key.
    let raw = serde_json::json!({
        "Input": {
            "data": [104, 105], // "hi"
            // NOTE: no `input_seq` field — `#[serde(default)]` must
            // synthesize 0.
        }
    });
    let decoded: ClientMsg = serde_json::from_value(raw).expect("decode older Input");
    match decoded {
        ClientMsg::Input { data, input_seq } => {
            assert_eq!(data, b"hi");
            assert_eq!(
                input_seq, 0,
                "missing input_seq must decode as 0 (serde default)"
            );
        }
        other => panic!("expected Input, got {other:?}"),
    }
}

/// Tuple-style `{"Input": [..]}` (the pre-#115 wire shape) MUST NOT
/// silently succeed — it's a structurally different payload from the
/// current struct variant, and a silent acceptance would mask a
/// genuine wire-version skew. We assert the JSON decode fails so the
/// daemon will reject such a frame with `unparseable ClientMsg JSON;
/// dropping` at the WS bridge — exactly the same code path that
/// catches any other malformed frame.
#[test]
fn input_tuple_form_no_longer_decodes() {
    let raw = serde_json::json!({
        "Input": [104, 105]
    });
    let decoded: Result<ClientMsg, _> = serde_json::from_value(raw);
    assert!(
        decoded.is_err(),
        "pre-#115 tuple form must not silently round-trip into the new struct \
         variant — got {decoded:?}"
    );
}

/// `DaemonMsg::InputAck { input_seq }` is the wire shape the daemon
/// emits after a successful PTY write. We assert it serializes and
/// deserializes through the JSON hop (the WS bridge) without losing
/// the seq, and that it bincode-round-trips for the kernel↔daemon
/// hop.
#[test]
fn input_ack_round_trips_json_and_bincode() {
    let original = DaemonMsg::InputAck { input_seq: 12345 };

    // JSON (WS bridge hop): browser-facing path.
    let json = serde_json::to_string(&original).expect("serialize");
    let decoded: DaemonMsg = serde_json::from_str(&json).expect("deserialize");
    match decoded {
        DaemonMsg::InputAck { input_seq } => assert_eq!(input_seq, 12345),
        other => panic!("expected InputAck, got {other:?}"),
    }
    // Sanity-check the wire shape so a careless rename of `input_seq`
    // would fail this test (the kernel transient client matches on
    // the field name).
    assert!(
        json.contains("\"input_seq\":12345"),
        "expected `input_seq` key in JSON, got: {json}"
    );

    // Bincode (kernel ↔ daemon hop).
    let bytes = bincode::serde::encode_to_vec(&original, bincode::config::standard())
        .expect("bincode encode");
    let (decoded, _): (DaemonMsg, _) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
            .expect("bincode decode");
    match decoded {
        DaemonMsg::InputAck { input_seq } => assert_eq!(input_seq, 12345),
        other => panic!("expected InputAck, got {other:?}"),
    }
}

/// Observer (no kernel_originated_input) sending Input — even with
/// a non-zero seq — gets `NotOwner` and NO `WriteToPty` effect.
/// Authorization comes before ack mechanics: a forged seq must not
/// induce the daemon to ack an unauthorized write.
#[test]
fn observer_input_with_seq_is_rejected_before_ack() {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut registry = OwnerRegistry::new();
    let session_id = Uuid::new_v4();

    // Pre-register a separate owner so this client attaches as Observer.
    let _ = registry.on_attach(Uuid::new_v4(), None);

    let observer_id = Uuid::new_v4();
    let mut state = TerminalSessionState::new();
    let _ = state.on_client_frame(
        hello(observer_id, false), // kernel_input = false
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    assert_eq!(state.role(), Some(Role::Observer));

    let effects = state.on_client_frame(
        ClientMsg::Input {
            data: b"x".to_vec(),
            input_seq: 99,
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::WriteToPty { .. })),
        "unauthorized Input must NOT produce WriteToPty (or its ack), got {effects:?}"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::SendProtocolError {
                code: calm_session::ProtocolErrorCode::NotOwner,
                ..
            }
        )),
        "expected NotOwner protocol error, got {effects:?}"
    );
}

/// Observer with `kernel_originated_input` capability AND non-zero
/// `input_seq` — the trusted kernel-private path: the protocol layer
/// must produce a `WriteToPty` carrying the seq, so the shell can
/// emit `InputAck` after the write completes. This is the primary
/// happy path for the `DaemonClient::inject_stdin` migration in PR #110.
#[test]
fn kernel_input_observer_input_with_seq_writes_with_seq() {
    let broadcaster = PtyBroadcaster::new(1024);
    let mut registry = OwnerRegistry::new();
    let session_id = Uuid::new_v4();

    // Pre-register a separate owner so the kernel client attaches as Observer.
    let _ = registry.on_attach(Uuid::new_v4(), None);

    let kernel_id = Uuid::new_v4();
    let mut state = TerminalSessionState::new();
    let _ = state.on_client_frame(
        hello(kernel_id, true), // kernel_input = true
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );
    assert_eq!(state.role(), Some(Role::Observer));

    let effects = state.on_client_frame(
        ClientMsg::Input {
            data: b"\r".to_vec(),
            input_seq: 1,
        },
        broadcaster.buffer(),
        &mut registry,
        &ctx(&broadcaster, session_id),
    );

    let write = effects
        .iter()
        .find_map(|e| match e {
            Effect::WriteToPty { data, input_seq } => Some((data.clone(), *input_seq)),
            _ => None,
        })
        .expect("kernel-input observer with seq must still produce WriteToPty");
    assert_eq!(write.0, b"\r");
    assert_eq!(write.1, 1);
    // ...and must NOT raise NotOwner.
    assert!(
        !effects.iter().any(|e| matches!(
            e,
            Effect::SendProtocolError {
                code: calm_session::ProtocolErrorCode::NotOwner,
                ..
            }
        )),
        "kernel-input authorized path must not raise NotOwner"
    );
}
