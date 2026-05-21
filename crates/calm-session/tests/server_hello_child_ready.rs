//! Acceptance tests for `DaemonMsg::ServerHello.is_child_ready` — the
//! deterministic snapshot of child-readiness that late-joining transient
//! connections (the kernel's input-injection `DaemonClient`) use in place
//! of the previous 600ms `tokio::time::sleep` heuristic.
//!
//! These tests pair the protocol state machine
//! (`TerminalSessionState::on_client_frame`) with a real
//! [`RenderPlane`] driven under virtual time so we can deterministically
//! place the `ChildReady` one-shot before or after the `ClientHello`
//! arrives, and assert what `ServerHello.is_child_ready` reflects in each
//! case. The one-shot semantic of `detect_ready` is exercised separately
//! in `tests/child_ready.rs`; this file only asserts the snapshot
//! accessor's relationship to `ServerHello`.
//!
//! Closes part of #115.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use calm_session::terminal_session::{
    CHILD_READY_QUIESCENT_MS, Effect, OwnerRegistry, RenderPlane, SessionContext,
    TerminalSessionState,
};
use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding,
};
use uuid::Uuid;

const TID: &str = "terminal-fixture";

fn mock_clock() -> (Arc<AtomicU64>, Box<dyn Fn() -> Instant + Send + Sync>) {
    let counter = Arc::new(AtomicU64::new(0));
    let base = Instant::now();
    let c = counter.clone();
    let f: Box<dyn Fn() -> Instant + Send + Sync> =
        Box::new(move || base + Duration::from_millis(c.load(Ordering::SeqCst)));
    (counter, f)
}

fn hello(client_id: Uuid) -> ClientMsg {
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
            kernel_originated_input: false,
        },
    }
}

fn ctx_from_plane<'a>(plane: &RenderPlane, session_id: Uuid) -> SessionContext<'a> {
    SessionContext {
        terminal_id: TID,
        session_id,
        pty_size: plane.current_size(),
        pty_seq_head: plane.pty_seq_head(),
        pty_seq_tail: plane.pty_seq(),
        render_rev: plane.render_rev(),
        is_child_ready: plane.child_ready_fired(),
    }
}

fn extract_is_child_ready(effects: &[Effect]) -> bool {
    effects
        .iter()
        .find_map(|e| match e {
            Effect::SendToClient(DaemonMsg::ServerHello { is_child_ready, .. }) => {
                Some(*is_child_ready)
            }
            _ => None,
        })
        .expect("expected SendToClient(ServerHello) in handshake effects")
}

/// Late joiner whose `ClientHello` lands BEFORE the daemon's
/// `RenderPlane::detect_ready` poll has fired must see
/// `is_child_ready: false`.
#[test]
fn server_hello_is_child_ready_false_before_child_ready_fires() {
    let (_counter, clock) = mock_clock();
    let plane = RenderPlane::with_clock(80, 24, 1024, 100, clock);
    // No chunks fed → detector can't fire → snapshot must be false.
    assert!(!plane.child_ready_fired());

    let mut registry = OwnerRegistry::new();
    let mut state = TerminalSessionState::new();
    let session_id = Uuid::new_v4();
    let client_id = Uuid::new_v4();

    let effects = state.on_client_frame(
        hello(client_id),
        plane.transcript(),
        &mut registry,
        &ctx_from_plane(&plane, session_id),
    );

    assert!(!extract_is_child_ready(&effects));
}

/// Late joiner whose `ClientHello` lands AFTER `ChildReady` has already
/// fired must see `is_child_ready: true` even though the broadcast itself
/// is one-shot and won't be re-emitted.
#[test]
fn server_hello_is_child_ready_true_after_child_ready_fires() {
    let (counter, clock) = mock_clock();
    let mut plane = RenderPlane::with_clock(80, 24, 1024, 100, clock);

    // Drive the plane through one PTY chunk + the quiescent window so
    // `detect_ready` fires exactly once.
    plane.on_pty_chunk(b"$ ".to_vec());
    counter.store(CHILD_READY_QUIESCENT_MS + 1, Ordering::SeqCst);
    let eff = plane.detect_ready();
    assert!(
        matches!(eff, Some(Effect::Broadcast(DaemonMsg::ChildReady { .. }))),
        "fixture precondition: ChildReady should fire after the quiescent window"
    );
    // Snapshot accessor must reflect the fired state without re-firing.
    assert!(plane.child_ready_fired());
    // And `detect_ready` must remain one-shot — calling it again returns
    // `None`. This is the trap the task description called out.
    assert!(
        plane.detect_ready().is_none(),
        "child_ready_fired() must not consume the one-shot"
    );
    assert!(plane.child_ready_fired());

    let mut registry = OwnerRegistry::new();
    let mut state = TerminalSessionState::new();
    let session_id = Uuid::new_v4();
    let client_id = Uuid::new_v4();

    let effects = state.on_client_frame(
        hello(client_id),
        plane.transcript(),
        &mut registry,
        &ctx_from_plane(&plane, session_id),
    );

    assert!(extract_is_child_ready(&effects));
}

/// Backward-compat: a `ServerHello` deserialized from a payload that
/// predates `is_child_ready` (i.e. older daemons that never serialized
/// the field) MUST decode with `is_child_ready: false` thanks to
/// `#[serde(default)]`.
///
/// We can't synthesize a "before" wire payload from the current
/// `DaemonMsg` (it always serializes the field), but we can drop the key
/// from a JSON-roundtripped value to simulate an older serializer.
#[test]
fn server_hello_decodes_missing_is_child_ready_as_false() {
    // Hand-rolled JSON missing the `is_child_ready` key — what an older
    // daemon (pre-#115) would have emitted. The `RenderSnapshot` schema
    // hasn't changed, so we can write it inline.
    let raw = serde_json::json!({
        "ServerHello": {
            "protocol_version": PROTOCOL_VERSION,
            "terminal_id": TID,
            "session_id": Uuid::new_v4(),
            "client_role": "Owner",
            "owner_client_id": null,
            "pty_size": {
                "cols": 80,
                "rows": 24,
                "pixel_width": null,
                "pixel_height": null,
            },
            "pty_seq_head": 0,
            "pty_seq_tail": 0,
            "render_rev": 0,
            "snapshot": {
                "render_rev": 0,
                "pty_seq": 0,
                "cols": 80,
                "rows": 24,
                "encoding": "Vt",
                "data": [],
                "scrollback": null,
            },
            "history_gap": null,
            // NOTE: no `is_child_ready` field — `#[serde(default)]` must
            // synthesize `false`.
        }
    });
    let decoded: DaemonMsg = serde_json::from_value(raw).expect("decode older ServerHello");
    match decoded {
        DaemonMsg::ServerHello { is_child_ready, .. } => {
            assert!(
                !is_child_ready,
                "older payload missing the field must decode as false"
            );
        }
        other => panic!("expected ServerHello, got {other:?}"),
    }
}
