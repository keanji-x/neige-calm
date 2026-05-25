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
//! ## Why this encoding (and how it differs from the v1 version)
//!
//! v1 of this test asserted `decide(SpecPushPhase::Idle) != StartTurnNow`.
//! That's wrong: `Idle → StartTurnNow` is the correct semantics for a
//! genuinely idle, freshly-spawned thread on the create-wave path. A
//! correct fix that introduces a `Resumed`/`Unknown` variant — but
//! leaves `Idle → StartTurnNow` intact — would STILL FAIL the v1 test.
//!
//! The bug is not "Idle decides StartTurnNow". The bug is "the resume
//! path plants Idle as the initial phase even though the server may be
//! mid-turn", i.e. the **decision input** is wrong, not the **decision
//! table**. See `spec_appserver.rs::build_handle_after_spawn_resume`
//! (~line 1287):
//!
//! ```ignore
//!     let status: SharedStatus = Arc::new(Mutex::new(SpecPushStatus {
//!         last_thread_id: Some(thread_id.to_string()),
//!         ..Default::default()   // phase = Idle
//!     }));
//! ```
//!
//! ## What this test asserts
//!
//! `build_handle_after_spawn_resume` is a private fn so the test cannot
//! call it. The strongest available encoding without exposing it as
//! `pub`: assert the **observable post-resume initial phase** (`Idle`,
//! the value planted by `SpecPushStatus::default()`) decides
//! `PushAction::Enqueue` — i.e. it is NOT a green light for a fresh
//! `turn/start`.
//!
//! A correct fix has two shapes that both make this test pass:
//!
//! 1. Introduce a `Resumed`/`Unknown` `SpecPushPhase` variant that
//!    decides `Enqueue`, and have `build_handle_after_spawn_resume`
//!    plant it (overriding the `Default`). To make this test pass
//!    behaviorally, the fix must also flip `SpecPushStatus::default()`
//!    so the variant we observe via the `Default` is `Resumed` — OR
//!    expose a `pub fn` to construct a status for resumed handles and
//!    update this test to call it.
//! 2. Keep `Idle` as the default but change `decide(Idle) → Enqueue`
//!    AND introduce a new "confirmed idle" phase that decides
//!    `StartTurnNow` (set by the consumer task's first
//!    `turn/completed` arrival).
//!
//! Both fixes flip the relationship `decide(default_phase) =
//! StartTurnNow` that this test fails on. A fix that introduces a
//! variant but doesn't change either `Default` or `decide` would not
//! change the post-resume behavior — and would correctly STILL fail
//! this test, prompting a deeper fix.
//!
//! ## Why we don't write a behavioral test
//!
//! Behavioral encoding (boot a fake app-server, run resume, inspect the
//! parked handle's phase) requires `build_handle_after_spawn_resume`
//! (or `resume_spec_appserver`) to be reachable in test contexts; it
//! isn't, and the issue forbids production changes. The
//! `SpecPushStatus::default()` value is the next-best proxy: it is the
//! literal input the resume path uses (via `..Default::default()`), so
//! a fix that changes the default or the decision flips this test.
//!
//! **Current behavior on main**:
//! `SpecPushStatus::default().phase == SpecPushPhase::Idle` and
//! `decide(Idle) == PushAction::StartTurnNow`. The composition violates
//! INV-4: a freshly-resumed handle would let the first catch-up push
//! issue `turn/start` against a potentially mid-turn server.
//!
//! See: `src/spec_appserver.rs::build_handle_after_spawn_resume`
//! (post-resume status seed, line ~1287) and `decide` (line ~301).

use calm_server::spec_appserver::{PushAction, SpecPushPhase, SpecPushStatus, decide};

/// INV-4 strict: the phase used by the boot-takeover resume path as the
/// initial post-`thread/resume` status (concretely:
/// `SpecPushStatus::default().phase`) MUST decide `Enqueue`, not
/// `StartTurnNow`. The resume path can't prove the server is between
/// turns until the notification stream reconciles, so the only sound
/// behavior is to defer the first push to the consumer task's
/// `turn/completed` flush.
///
/// Today `SpecPushStatus::default().phase == SpecPushPhase::Idle` and
/// `decide(Idle) == StartTurnNow` — composition is the bug.
#[test]
fn inv4_post_resume_default_phase_must_decide_enqueue() {
    // The value `build_handle_after_spawn_resume` plants as the initial
    // phase: it constructs `SpecPushStatus { last_thread_id: …,
    // ..Default::default() }`, so the `phase` field comes from
    // `SpecPushStatus::default()` → `SpecPushPhase::default()` → `Idle`.
    let post_resume_phase = SpecPushStatus::default().phase;

    // The decision the first dispatcher catch-up push would compute
    // against that initial phase.
    let action = decide(post_resume_phase);

    assert_eq!(
        action,
        PushAction::Enqueue,
        "INV-4 violated: `SpecPushStatus::default().phase = {:?}` and \
         `decide({:?}) = {:?}` — composed, this means the boot-takeover \
         resume path (build_handle_after_spawn_resume, spec_appserver.rs \
         ~line 1287) plants a phase that tells the first catch-up push \
         to fire `turn/start`. But `thread/resume` cannot prove the \
         server is between turns (codex has no thread/status probe), so \
         the server may be mid-turn — and codex silently drops a second \
         `turn/start` on a busy thread (verified, see spec_appserver.rs \
         module doc). A correct fix introduces a `Resumed`/`Unknown` \
         phase (or otherwise changes the default OR the decision) so \
         the first push enqueues until a `turn/completed` confirms the \
         server is actually idle.",
        post_resume_phase,
        post_resume_phase,
        action,
    );
}

/// INV-4 (b): pin the documented-correct decision table for non-resume
/// phases so a fix to INV-4 doesn't accidentally regress them. This
/// test PASSES on main today — it's the regression guard half of the
/// invariant.
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
