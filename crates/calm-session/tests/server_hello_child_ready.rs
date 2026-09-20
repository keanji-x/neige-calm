//! Acceptance tests for `DaemonMsg::ServerHello.is_child_ready`: the state machine paired with a real
//! [`RenderPlane`] under virtual time, placing the `ChildReady` one-shot before or after the `ClientHello`.

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
        current_default_fg: plane.default_fg(),
        current_default_bg: plane.default_bg(),
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

#[test]
fn server_hello_is_child_ready_false_before_child_ready_fires() {
    let (_counter, clock) = mock_clock();
    let plane = RenderPlane::with_clock(80, 24, 1024, 100, clock);
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

/// The broadcast itself is one-shot and won't be re-emitted, so the snapshot must carry the fired state.
#[test]
fn server_hello_is_child_ready_true_after_child_ready_fires() {
    let (counter, clock) = mock_clock();
    let mut plane = RenderPlane::with_clock(80, 24, 1024, 100, clock);

    plane.on_pty_chunk(b"$ ".to_vec());
    counter.store(CHILD_READY_QUIESCENT_MS + 1, Ordering::SeqCst);
    let eff = plane.detect_ready();
    assert!(
        matches!(eff, Some(Effect::Broadcast(DaemonMsg::ChildReady { .. }))),
        "fixture precondition: ChildReady should fire after the quiescent window"
    );
    assert!(plane.child_ready_fired());
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

/// A `ServerHello` payload that predates `is_child_ready` MUST decode with `is_child_ready: false` via `#[serde(default)]`.
#[test]
fn server_hello_decodes_missing_is_child_ready_as_false() {
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
