//! # INV-4 — turn-phase mutex (adopt vs turn-start must serialize)
//!
//! **Bug**: R2-B2 (from #318)
//! **Encoded contract**: the phase state machine that owns a codex
//! thread must serialize transitions. Critically: a handle freshly
//! parked after `thread/resume` (boot-takeover path) must NOT start in a
//! state that the `decide` table would treat as "safe to issue
//! turn/start now" UNLESS the resumed server is actually idle. Today
//! `build_handle_after_spawn_resume` plants `SpecPushStatus::default()`
//! whose `phase` is `Idle`, then waits for the notification stream to
//! reconcile — but the dispatcher's catch-up push can race the
//! notification arrival, hit `Idle`, and issue `turn/start` against a
//! mid-turn server. Codex silently drops that `turn/start` (verified in
//! `spec_appserver.rs` module docs). The whole queue machinery exists
//! to prevent exactly this; turning around and starting at `Idle`
//! defeats the guard for the resume path.
//!
//! **Why this design**: `decide(phase)` is `pub` and pure. We exercise
//! it against the post-resume initial phase the production code parks
//! today (`SpecPushPhase::Idle`) and assert that the decision is NOT
//! `StartTurnNow`. The invariant says: post-resume the handle must
//! treat the server as potentially-mid-turn — the decision must be
//! `Enqueue` (or there must be a dedicated phase variant — e.g. an
//! `Unknown` between `Idle` and `Issuing` — that gates `decide`). On
//! main, `SpecPushPhase::Idle` decides `StartTurnNow`, so this fails.
//!
//! **Current behavior on main**: `decide(SpecPushPhase::Idle)` returns
//! `PushAction::StartTurnNow`. A freshly-resumed handle therefore lets
//! the first catch-up push fire `turn/start` against a potentially
//! mid-turn server — the silently-dropped failure mode the queue
//! exists to prevent.
//!
//! See: `src/spec_appserver.rs::build_handle_after_spawn_resume`
//! (`status.phase = Idle` post-resume, line ~1287) and `decide`
//! (line ~301).

use calm_server::spec_appserver::{PushAction, SpecPushPhase, decide};

/// INV-4 strict: a phase value that's plausibly held by a freshly
/// resumed handle (`Idle`, the default post-`thread/resume`) MUST not
/// be a green light for `turn/start`. The mid-turn-on-resume scenario
/// is undecidable from the kernel's side — the only sound behavior is
/// to enqueue and let the consumer task's `turn/started` /
/// `turn/completed` reconcile.
#[test]
fn inv4_resume_initial_phase_must_not_decide_start_turn() {
    // The actual initial phase planted by
    // `build_handle_after_spawn_resume` (line ~1287 in
    // spec_appserver.rs): `SpecPushStatus::default()` → `phase = Idle`.
    let post_resume_phase = SpecPushPhase::Idle;

    let action = decide(post_resume_phase);

    assert_ne!(
        action,
        PushAction::StartTurnNow,
        "INV-4 violated: decide({:?}) = {:?}, meaning a freshly-resumed handle would \
         let the first catch-up push issue `turn/start` against a potentially mid-turn \
         server (codex silently drops that turn). The turn-phase state machine must \
         serialize against the server's actual state — a resumed handle should start \
         in a pessimistic phase (e.g. an `Unknown`/`Resumed` variant) that decides \
         `Enqueue` until the notification stream confirms idle. Today no such variant \
         exists; `Idle` is the only between-turns phase and it decides `StartTurnNow`.",
        post_resume_phase,
        action,
    );
}

/// INV-4 strict (b): the phase transition `Idle → Issuing` is meant to
/// be the single-winner gate (the comment at `push_observation` calls
/// it that explicitly). But the gate is only useful if the FIRST
/// observer of a between-turns phase comes from a context where it
/// actually owns the right to issue. After a `thread/resume`, the
/// FIRST observer is the dispatcher's catch-up push — but at that
/// point we haven't proven the server is between turns. The strict
/// reading: there must be a phase value (call it `Resumed`) distinct
/// from `Idle`, whose `decide` is `Enqueue`. We assert that variant
/// exists in `SpecPushPhase`.
///
/// (We can't enumerate `SpecPushPhase` variants from a downstream
/// crate; we check the property indirectly: if EVERY variant either
/// decides `Enqueue` or is one of the well-known Running/Issuing
/// variants — i.e. there's NO between-turns Idle-shape variant — that's
/// also acceptable. The current shape has `Idle` and `TurnCompleted`
/// both deciding `StartTurnNow`, so the check below sees both and
/// fails.)
#[test]
fn inv4_post_resume_state_must_default_to_enqueue() {
    // Both "between turns" decisions return StartTurnNow today.
    // INV-4 says a resumed handle (no server confirmation yet) should
    // default to Enqueue, not StartTurnNow. There's no such variant on
    // main.
    let between_turns_decisions = [
        decide(SpecPushPhase::Idle),
        decide(SpecPushPhase::TurnCompleted),
    ];
    let any_safe_default = between_turns_decisions
        .iter()
        .any(|a| matches!(a, PushAction::Enqueue));
    assert!(
        any_safe_default,
        "INV-4 violated: every between-turns phase (Idle, TurnCompleted) decides \
         StartTurnNow. Boot-takeover plants `Idle` after a resume — there is no \
         pessimistic 'resumed, not yet reconciled' phase, so the first catch-up \
         push fires `turn/start` even when the server is mid-turn. The \
         single-winner Idle→Issuing gate inside `push_observation` only protects \
         CONCURRENT same-process pushes; it does NOT protect against the kernel \
         vs. server-side turn-phase mismatch a resume can create."
    );
}
