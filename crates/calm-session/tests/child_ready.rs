//! Acceptance tests for [`RenderPlane::detect_ready`], driven under virtual time via an injected mock clock.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use calm_session::DaemonMsg;
use calm_session::terminal_session::{CHILD_READY_QUIESCENT_MS, Effect, RenderPlane};

/// Build a mock clock: `counter` is the shared virtual-millisecond store; `clock` returns `base + counter ms`.
fn mock_clock() -> (Arc<AtomicU64>, Box<dyn Fn() -> Instant + Send + Sync>) {
    let counter = Arc::new(AtomicU64::new(0));
    let base = Instant::now();
    let c = counter.clone();
    let f: Box<dyn Fn() -> Instant + Send + Sync> =
        Box::new(move || base + Duration::from_millis(c.load(Ordering::SeqCst)));
    (counter, f)
}

#[test]
fn child_ready_fires_once_after_quiescent_window() {
    let (counter, clock) = mock_clock();
    let mut plane = RenderPlane::with_clock(80, 24, 1024, 100, clock);

    assert!(plane.detect_ready().is_none());

    plane.on_pty_chunk(b"$ ".to_vec());
    assert!(
        plane.detect_ready().is_none(),
        "should not fire immediately after first chunk"
    );

    counter.store(CHILD_READY_QUIESCENT_MS + 1, Ordering::SeqCst);
    let eff = plane.detect_ready();
    assert!(
        matches!(eff, Some(Effect::Broadcast(DaemonMsg::ChildReady { .. }))),
        "expected ChildReady broadcast, got {eff:?}"
    );
    assert!(plane.child_ready_fired());

    assert!(
        plane.detect_ready().is_none(),
        "ChildReady fired twice (one-shot violation)"
    );
}

#[test]
fn child_ready_resets_on_new_chunk_within_window() {
    let (counter, clock) = mock_clock();
    let mut plane = RenderPlane::with_clock(80, 24, 1024, 100, clock);

    plane.on_pty_chunk(b"$ ".to_vec());
    counter.store(CHILD_READY_QUIESCENT_MS / 2, Ordering::SeqCst);
    plane.on_pty_chunk(b"a".to_vec());
    // Cumulative ~100ms virtual, but the LAST chunk was only 50ms ago — must not fire yet.
    counter.store(CHILD_READY_QUIESCENT_MS, Ordering::SeqCst);

    assert!(
        plane.detect_ready().is_none(),
        "ChildReady fired before quiescent window elapsed since the most recent chunk"
    );

    counter.store(
        CHILD_READY_QUIESCENT_MS / 2 + CHILD_READY_QUIESCENT_MS + 1,
        Ordering::SeqCst,
    );
    let eff = plane.detect_ready();
    assert!(
        matches!(eff, Some(Effect::Broadcast(DaemonMsg::ChildReady { .. }))),
        "expected ChildReady after waiting full window since last chunk, got {eff:?}"
    );
}

#[test]
fn child_ready_carries_correct_seq_and_rev() {
    let (counter, clock) = mock_clock();
    let mut plane = RenderPlane::with_clock(80, 24, 1024, 100, clock);
    plane.on_pty_chunk(b"$".to_vec());
    plane.on_pty_chunk(b" ".to_vec());

    counter.store(CHILD_READY_QUIESCENT_MS + 1, Ordering::SeqCst);

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

#[test]
fn detect_ready_returns_none_when_no_chunks_observed() {
    let (counter, clock) = mock_clock();
    let mut plane = RenderPlane::with_clock(80, 24, 1024, 100, clock);
    counter.store(CHILD_READY_QUIESCENT_MS * 5, Ordering::SeqCst);
    assert!(
        plane.detect_ready().is_none(),
        "detect_ready fired without any PTY chunks ever arriving"
    );
}
