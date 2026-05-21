//! Unit tests for the pure terminal-mode protocol state machine.
//!
//! Each case feeds one or more frames into a fresh
//! [`TerminalSessionState`] and asserts on the [`Effect`] list returned.
//! No PTY, no tokio runtime, no socket — these complete in microseconds and
//! replace the slow fork-based assertions that used to live in
//! `tests/chat_e2e.rs` analogues.

use std::collections::HashMap;

use calm_session::terminal_session::{ByteRing, Effect, PtyBroadcaster, TerminalSessionState};
use calm_session::{ClientMsg, DaemonMsg};
use uuid::Uuid;

// ---------- TerminalSessionState ----------

#[test]
fn first_frame_attach_emits_resize_then_hello_with_replay() {
    let mut ring = ByteRing::new(1024);
    ring.append(b"prior pty output".to_vec());
    let mut state = TerminalSessionState::new();

    let effects = state.on_client_frame(ClientMsg::Attach { cols: 80, rows: 24 }, &ring);

    assert!(state.is_attached());
    assert_eq!(
        effects,
        vec![
            Effect::ResizePty { cols: 80, rows: 24 },
            Effect::SendToClient(DaemonMsg::Hello {
                replay: b"prior pty output".to_vec(),
            }),
        ]
    );
}

#[test]
fn first_frame_non_attach_yields_protocol_violation() {
    let ring = ByteRing::new(1024);
    let mut state = TerminalSessionState::new();

    let effects = state.on_client_frame(ClientMsg::Stdin(b"oops".to_vec()), &ring);

    assert!(!state.is_attached());
    assert_eq!(effects.len(), 1);
    assert!(matches!(effects.last(), Some(Effect::ProtocolViolation(_))));
}

#[test]
fn attached_stdin_emits_only_write_to_pty() {
    let ring = ByteRing::new(1024);
    let mut state = TerminalSessionState::new();
    let _ = state.on_client_frame(ClientMsg::Attach { cols: 80, rows: 24 }, &ring);

    let effects = state.on_client_frame(ClientMsg::Stdin(b"hi".to_vec()), &ring);

    assert_eq!(effects, vec![Effect::WriteToPty(b"hi".to_vec())]);
}

#[test]
fn attached_resize_emits_resize_pty() {
    let ring = ByteRing::new(1024);
    let mut state = TerminalSessionState::new();
    let _ = state.on_client_frame(ClientMsg::Attach { cols: 80, rows: 24 }, &ring);

    let effects = state.on_client_frame(
        ClientMsg::Resize {
            cols: 120,
            rows: 40,
        },
        &ring,
    );

    assert_eq!(
        effects,
        vec![Effect::ResizePty {
            cols: 120,
            rows: 40
        }]
    );
}

#[test]
fn attached_kill_emits_kill_child() {
    let ring = ByteRing::new(1024);
    let mut state = TerminalSessionState::new();
    let _ = state.on_client_frame(ClientMsg::Attach { cols: 80, rows: 24 }, &ring);

    let effects = state.on_client_frame(ClientMsg::Kill, &ring);

    assert_eq!(effects, vec![Effect::KillChild]);
}

#[test]
fn attached_reattach_is_silent_noop() {
    // Matches daemon.rs:728 "Ignore re-attach on a live connection."
    let ring = ByteRing::new(1024);
    let mut state = TerminalSessionState::new();
    let _ = state.on_client_frame(ClientMsg::Attach { cols: 80, rows: 24 }, &ring);

    let effects = state.on_client_frame(
        ClientMsg::Attach {
            cols: 200,
            rows: 60,
        },
        &ring,
    );

    assert!(effects.is_empty());
}

#[test]
fn attached_chat_frames_are_silently_ignored() {
    // Matches daemon.rs:735-741: ChatUserMessage/ChatStop/AnswerQuestion in
    // terminal mode produce no Effect (just a debug log in the shell).
    let ring = ByteRing::new(1024);
    let mut state = TerminalSessionState::new();
    let _ = state.on_client_frame(ClientMsg::Attach { cols: 80, rows: 24 }, &ring);

    assert!(
        state
            .on_client_frame(
                ClientMsg::ChatUserMessage {
                    content: "hello agent".to_string(),
                },
                &ring,
            )
            .is_empty()
    );
    assert!(state.on_client_frame(ClientMsg::ChatStop, &ring).is_empty());
    assert!(
        state
            .on_client_frame(
                ClientMsg::AnswerQuestion {
                    question_id: Uuid::nil(),
                    answers: HashMap::new(),
                },
                &ring,
            )
            .is_empty()
    );
}

// ---------- PtyBroadcaster + ByteRing ----------

#[test]
fn pty_chunk_broadcasts_stdout_and_appends_to_buffer() {
    let mut pb = PtyBroadcaster::new(1024);

    let effects = pb.on_pty_chunk(b"abc".to_vec());

    assert_eq!(
        effects,
        vec![Effect::Broadcast(DaemonMsg::Stdout(b"abc".to_vec()))]
    );
    assert_eq!(pb.buffer().snapshot(), b"abc");
}

#[test]
fn byte_ring_evicts_oldest_chunk_when_over_budget() {
    // ByteRing(100), append 60 bytes then 80 bytes: total would be 140 >
    // 100, so the first chunk is dropped and snapshot is just the second.
    let mut pb = PtyBroadcaster::new(100);
    pb.on_pty_chunk(vec![b'a'; 60]);
    pb.on_pty_chunk(vec![b'b'; 80]);

    let snap = pb.buffer().snapshot();
    assert!(snap.len() <= 100, "snapshot was {} bytes", snap.len());
    assert_eq!(snap, vec![b'b'; 80]);
}

#[test]
fn child_exit_broadcasts_child_exited_with_code() {
    let mut pb = PtyBroadcaster::new(1024);

    let effects = pb.on_child_exit(Some(7));

    assert_eq!(
        effects,
        vec![Effect::Broadcast(DaemonMsg::ChildExited { code: Some(7) })]
    );
}

#[test]
fn resize_with_zero_dim_still_produces_resize_effect() {
    // The IO shell's apply_resize keeps the cols==0 / rows==0 guard; the
    // state machine just reports what the client asked for.
    let ring = ByteRing::new(1024);
    let mut state = TerminalSessionState::new();
    let _ = state.on_client_frame(ClientMsg::Attach { cols: 80, rows: 24 }, &ring);

    let effects = state.on_client_frame(ClientMsg::Resize { cols: 0, rows: 24 }, &ring);

    assert_eq!(effects, vec![Effect::ResizePty { cols: 0, rows: 24 }]);
}
