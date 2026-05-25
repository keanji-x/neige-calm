//! # INV-4 — turn-phase mutex (adopt vs turn-start must serialize)
//!
//! **Bug**: R2-B2 (from #318)
//! **Encoded contract**: the phase state machine that owns a codex
//! thread must never start a turn against a thread we cannot prove is
//! between turns. Specifically: the **boot-takeover resume path**
//! (`build_handle_after_spawn_resume`) `thread/resume`s a thread that
//! may be mid-turn from the prior process's perspective — codex's
//! JSON-RPC has no `thread/status` probe, so we cannot synchronously
//! confirm idle. Until the live notification stream delivers a
//! `turn/started` or `turn/completed` to reconcile, the handle's phase
//! is **unknown**. INV-4 says: in that unknown window, the dispatcher's
//! first catch-up push MUST `Enqueue` (and let the consumer task's
//! `turn/completed` flush deliver), NEVER `StartTurnNow` (which against
//! a mid-turn thread is silently dropped by codex).
//!
//! ## v3 encoding via observability seam
//!
//! v1 of this test asserted `decide(SpecPushPhase::Idle) != StartTurnNow`.
//! That was wrong — `Idle → StartTurnNow` is correct on the create-wave
//! path where the freshly-spawned thread really is idle by construction.
//! v2 asserted `decide(SpecPushStatus::default().phase) == Enqueue`,
//! using `Default` as a proxy for the resume-path planted phase. Codex
//! flagged that as testing the wrong surface — `Default` is shared
//! across construction sites; a fix changing the resume path doesn't
//! need to change `Default`.
//!
//! v3 pins the **exact value the resume path plants** via a narrow
//! observability seam: [`spec_appserver::initial_status_for_resume`]
//! is a `pub fn` that returns the literal status used by
//! `build_handle_after_spawn_resume`'s post-`thread/resume` mutex
//! init. The fn performs **no logic** — it's the extracted struct
//! literal — but exposing it lets the test capture the planted phase
//! without holding any handle / running any I/O.
//!
//! On origin/main, `initial_status_for_resume("thread-test").phase ==
//! SpecPushPhase::Idle` and `decide(Idle) == PushAction::StartTurnNow`
//! — composed, the resume path's first catch-up push fires `turn/start`
//! against a thread that may already be mid-turn (codex silently drops
//! the second `turn/start`, losing the catch-up payload).
//!
//! ## What a correct fix looks like
//!
//! The fix changes what `initial_status_for_resume` returns so its
//! `phase` decides `Enqueue`. Two shapes both satisfy the test:
//!
//! 1. **Add a `Resumed`/`Unknown` `SpecPushPhase` variant** that decides
//!    `Enqueue`, and have `initial_status_for_resume` plant it. The
//!    consumer task's first observed `turn/completed` (or, on a server
//!    actually-idle, a synthetic reconcile after a status probe) flips
//!    it to `TurnCompleted` so the next push fires the turn normally.
//! 2. **Flip the decision** for the planted phase by reshaping `decide` +
//!    adding a "confirmed idle" phase set only by the consumer task's
//!    `turn/completed`. The freshly-resumed default is then a phase that
//!    enqueues.
//!
//! Either makes `decide(initial_status_for_resume(…).phase) == Enqueue`.
//!
//! ## What this file ships
//!
//! 1. **`inv4_initial_resume_status_must_decide_enqueue` (active,
//!    fails on main)**: asserts via the new seam.
//! 2. **`inv4_decision_table_regression_guard` (active, passes on
//!    main)**: pins the non-resume decision table (`Idle → StartTurnNow`,
//!    `TurnRunning → Enqueue`, etc.) so a fix to the resume initial
//!    phase doesn't accidentally regress the create-wave path where
//!    `Idle → StartTurnNow` is the desired behavior.
//!
//! See: `src/spec_appserver.rs::initial_status_for_resume` (the new
//! seam, called from `build_handle_after_spawn_resume`), `decide`
//! (decision table).

use calm_server::spec_appserver::{PushAction, SpecPushPhase, decide, initial_status_for_resume};

/// INV-4 strict: the phase planted by the boot-takeover resume path
/// (`build_handle_after_spawn_resume`, observable via the new
/// `initial_status_for_resume` seam) MUST decide `Enqueue`, not
/// `StartTurnNow`. `thread/resume` cannot prove the server is between
/// turns, so the only sound behavior for the first catch-up push is
/// to defer to the consumer task's `turn/completed`-triggered flush.
///
/// Today `initial_status_for_resume(_).phase == SpecPushPhase::Idle`
/// and `decide(Idle) == StartTurnNow` — composed, this is the bug.
#[test]
fn inv4_initial_resume_status_must_decide_enqueue() {
    // The exact value `build_handle_after_spawn_resume` plants into the
    // SharedStatus mutex right after a successful `thread/resume`. The
    // `thread_id` arg matches the resume-echoed id; tests use a stub
    // value because the phase field — the load-bearing part — doesn't
    // depend on it.
    let planted = initial_status_for_resume("thread-test");
    let action = decide(planted.phase);

    assert_eq!(
        action,
        PushAction::Enqueue,
        "INV-4 violated (R2-B2): `initial_status_for_resume(...).phase = {:?}` \
         and `decide({:?}) = {:?}`. Composed, this means the boot-takeover \
         resume path (build_handle_after_spawn_resume → initial_status_for_resume, \
         spec_appserver.rs) plants a phase that tells the first catch-up push \
         to fire `turn/start`. But `thread/resume` cannot prove the server is \
         between turns (codex has no thread/status probe), so the server may \
         be mid-turn — and codex silently drops a second `turn/start` on a busy \
         thread (verified, see spec_appserver.rs module doc). A correct fix \
         changes what `initial_status_for_resume` returns (e.g. introduces a \
         `Resumed`/`Unknown` phase that decides Enqueue, planted here instead \
         of the `Default` Idle), OR reshapes `decide` so the planted phase \
         enqueues. Either flips this assertion to pass.",
        planted.phase,
        planted.phase,
        action,
    );
}

/// INV-4 (b): pin the documented-correct decision table for non-resume
/// phases so a fix to the resume initial phase doesn't accidentally
/// regress them. This PASSES on main — it's the regression-guard half.
///
/// * Idle / TurnCompleted on the **create-wave path** (where the server
///   really is between turns, by construction — we just sent
///   `thread/start`) must decide `StartTurnNow`.
/// * TurnRunning / Issuing must decide `Enqueue`.
#[test]
fn inv4_decision_table_regression_guard() {
    // Genuinely between turns — fresh spawn or after a confirmed
    // turn/completed: a turn/start is safe.
    assert_eq!(decide(SpecPushPhase::Idle), PushAction::StartTurnNow);
    assert_eq!(
        decide(SpecPushPhase::TurnCompleted),
        PushAction::StartTurnNow
    );
    // A turn is in flight (or being issued): enqueue.
    assert_eq!(decide(SpecPushPhase::TurnRunning), PushAction::Enqueue);
    assert_eq!(decide(SpecPushPhase::Issuing), PushAction::Enqueue);
}
