//! Acceptance tests for [`RenderPlane::detect_ready`] — the one-shot
//! `ChildReady` quiescent-window detector introduced as PR-2.5 follow-up
//! to PR #68.
//!
//! These tests use real `tokio::time::sleep` (wall-clock) rather than
//! virtual time because [`RenderPlane`] internally captures
//! `std::time::Instant` instants in `on_pty_chunk` — virtualizing the
//! tokio runtime would not advance those. A reviewer-suggested follow-up
//! is to refactor to inject a `now: fn() -> Instant`; for now the
//! wall-clock waits are bounded (≤150ms per case) and the suite stays
//! under 1s total.

use std::time::Duration;

use calm_session::DaemonMsg;
use calm_session::terminal_session::{CHILD_READY_QUIESCENT_MS, Effect, RenderPlane};

/// Margin we add on top of `CHILD_READY_QUIESCENT_MS` when sleeping to
/// guarantee the deadline is past — covers tokio scheduler jitter on CI
/// (typically <5ms in practice; 30ms keeps us comfortably above noise).
const MARGIN_MS: u64 = 30;

#[tokio::test]
async fn child_ready_fires_once_after_quiescent_window() {
    let mut plane = RenderPlane::new(80, 24, 1024, 100);

    // No chunks yet → detector cannot fire (`last_rev_change_at` is None).
    assert!(plane.detect_ready().is_none());

    // Feed a chunk → model.rev() bumps → timer starts now.
    plane.on_pty_chunk(b"$ ".to_vec());
    // Immediately after the chunk the window hasn't elapsed.
    assert!(
        plane.detect_ready().is_none(),
        "should not fire immediately after first chunk"
    );

    // Wait past the quiescent window.
    tokio::time::sleep(Duration::from_millis(CHILD_READY_QUIESCENT_MS + MARGIN_MS)).await;
    let eff = plane.detect_ready();
    assert!(
        matches!(eff, Some(Effect::Broadcast(DaemonMsg::ChildReady { .. }))),
        "expected ChildReady broadcast, got {eff:?}"
    );
    assert!(plane.child_ready_fired());

    // Second call MUST NOT re-fire — `ChildReady` is one-shot per session.
    assert!(
        plane.detect_ready().is_none(),
        "ChildReady fired twice (one-shot violation)"
    );
}

#[tokio::test]
async fn child_ready_resets_on_new_chunk_within_window() {
    let mut plane = RenderPlane::new(80, 24, 1024, 100);

    plane.on_pty_chunk(b"$ ".to_vec());
    tokio::time::sleep(Duration::from_millis(CHILD_READY_QUIESCENT_MS / 2)).await;
    // Still painting — second chunk arrives mid-window.
    plane.on_pty_chunk(b"a".to_vec());
    tokio::time::sleep(Duration::from_millis(CHILD_READY_QUIESCENT_MS / 2)).await;

    // Cumulative wall time is ~100ms but the LAST chunk was only 50ms
    // ago — detector must not fire yet.
    assert!(
        plane.detect_ready().is_none(),
        "ChildReady fired before quiescent window elapsed since the most recent chunk"
    );

    // Now wait the rest of the window past the second chunk.
    tokio::time::sleep(Duration::from_millis(
        CHILD_READY_QUIESCENT_MS / 2 + MARGIN_MS,
    ))
    .await;
    let eff = plane.detect_ready();
    assert!(
        matches!(eff, Some(Effect::Broadcast(DaemonMsg::ChildReady { .. }))),
        "expected ChildReady after waiting full window since last chunk, got {eff:?}"
    );
}

#[tokio::test]
async fn child_ready_carries_correct_seq_and_rev() {
    let mut plane = RenderPlane::new(80, 24, 1024, 100);
    // Two chunks, both move state visibly (printable chars bump rev).
    plane.on_pty_chunk(b"$".to_vec());
    plane.on_pty_chunk(b" ".to_vec());

    tokio::time::sleep(Duration::from_millis(CHILD_READY_QUIESCENT_MS + MARGIN_MS)).await;

    match plane.detect_ready() {
        Some(Effect::Broadcast(DaemonMsg::ChildReady {
            pty_seq,
            render_rev,
        })) => {
            assert_eq!(pty_seq, 2, "two chunks fed; pty_seq should equal 2");
            assert!(
                render_rev >= 1,
                "two printables should have bumped render_rev at least once"
            );
        }
        other => panic!("expected ChildReady broadcast, got {other:?}"),
    }
}

#[tokio::test]
async fn detect_ready_returns_none_when_no_chunks_observed() {
    // Guard against the obvious bug: a fresh plane with no chunks must
    // NEVER fire ChildReady, no matter how long we wait. The detector
    // gates on `last_rev_change_at` being Some(_), and that's set only
    // when a chunk produces a rev change.
    let mut plane = RenderPlane::new(80, 24, 1024, 100);
    tokio::time::sleep(Duration::from_millis(CHILD_READY_QUIESCENT_MS * 2)).await;
    assert!(
        plane.detect_ready().is_none(),
        "detect_ready fired without any PTY chunks ever arriving"
    );
}
