//! Acceptance tests for [`RenderPlane::detect_ready`] — the one-shot
//! `ChildReady` quiescent-window detector introduced as PR-2.5 follow-up
//! to PR #68.
//!
//! These tests drive [`RenderPlane`] under virtual time via an injected
//! mock clock (`Arc<AtomicU64>`-backed) constructed through
//! [`RenderPlane::with_clock`]. Both write-side (`on_pty_chunk` setting
//! `last_rev_change_at`) and read-side (`detect_ready` comparing against
//! "now") route through the same clock, so advancing the counter is the
//! sole source of elapsed time. No wall-clock sleeps, no jitter margin
//! needed.
//!
//! See [`RenderPlane::with_clock`] for the contract that keeps the two
//! sides in sync.
//!
//! Closes #82.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use calm_session::DaemonMsg;
use calm_session::terminal_session::{CHILD_READY_QUIESCENT_MS, Effect, RenderPlane};

/// Build a mock clock. Returns `(counter, clock)` where:
///
/// - `counter` is the shared virtual-millisecond store. Tests advance
///   it via `counter.store(N, ..)` or `counter.fetch_add(N, ..)`.
/// - `clock` is the closure handed to [`RenderPlane::with_clock`]; each
///   call returns `base + counter ms`, so virtual time is monotonic and
///   the absolute `Instant` returned is meaningful for `duration_since`.
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

    // No chunks yet → detector cannot fire (`last_rev_change_at` is None).
    assert!(plane.detect_ready().is_none());

    // Feed a chunk → model.rev() bumps → timer starts at virtual t=0.
    plane.on_pty_chunk(b"$ ".to_vec());
    // Immediately after the chunk the window hasn't elapsed.
    assert!(
        plane.detect_ready().is_none(),
        "should not fire immediately after first chunk"
    );

    // Advance past the quiescent window.
    counter.store(CHILD_READY_QUIESCENT_MS + 1, Ordering::SeqCst);
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

#[test]
fn child_ready_resets_on_new_chunk_within_window() {
    let (counter, clock) = mock_clock();
    let mut plane = RenderPlane::with_clock(80, 24, 1024, 100, clock);

    // First chunk at virtual t=0.
    plane.on_pty_chunk(b"$ ".to_vec());
    // Advance to half-window — still painting; second chunk arrives.
    counter.store(CHILD_READY_QUIESCENT_MS / 2, Ordering::SeqCst);
    plane.on_pty_chunk(b"a".to_vec());
    // Advance another half-window (cumulative ~100ms virtual, but the
    // LAST chunk was only 50ms ago — detector must not fire yet).
    counter.store(CHILD_READY_QUIESCENT_MS, Ordering::SeqCst);

    assert!(
        plane.detect_ready().is_none(),
        "ChildReady fired before quiescent window elapsed since the most recent chunk"
    );

    // Now advance the rest of the window past the second chunk
    // (chunk landed at QUIESCENT/2, fire at QUIESCENT/2 + QUIESCENT + 1).
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
    // Two chunks, both move state visibly (printable chars bump rev).
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
    // Guard against the obvious bug: a fresh plane with no chunks must
    // NEVER fire ChildReady, no matter how long we wait. The detector
    // gates on `last_rev_change_at` being Some(_), and that's set only
    // when a chunk produces a rev change.
    let (counter, clock) = mock_clock();
    let mut plane = RenderPlane::with_clock(80, 24, 1024, 100, clock);
    counter.store(CHILD_READY_QUIESCENT_MS * 5, Ordering::SeqCst);
    assert!(
        plane.detect_ready().is_none(),
        "detect_ready fired without any PTY chunks ever arriving"
    );
}
