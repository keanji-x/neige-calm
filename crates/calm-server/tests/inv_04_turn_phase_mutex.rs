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
//! ## Status: regression guard (passes on current main)
//!
//! INV-4 was the **only** invariant test FAILING on the v3 PR. PR #323
//! closed R2-B2 by:
//!
//! 1. Adding a new [`SpecPushPhase::Resumed`] variant.
//! 2. Wiring [`build_handle_after_spawn_resume`] to plant `Resumed`
//!    directly (inline struct literal) right after `thread/resume`,
//!    instead of falling through to `SpecPushPhase::Idle` via
//!    `Default`.
//! 3. Extending [`decide`] so `decide(Resumed) == PushAction::Enqueue`.
//! 4. Reconciling `Resumed → TurnCompleted` from the consumer task's
//!    next observed `turn/started` / `turn/completed`, plus a reconcile
//!    timer fallback for servers that are genuinely idle.
//!
//! With (1) + (3) composed, the resume path's first catch-up push now
//! buffers and rides a coalesced `turn/start`, regardless of whether
//! the resumed server is mid-turn or idle. INV-4 is satisfied.
//!
//! This test file is now a **pure regression guard**: it pins both
//! halves of #323's contract so a future refactor that either
//!
//! * drops the `Resumed` variant (collapsing it back into `Idle`), OR
//! * flips `decide(Resumed)` to `StartTurnNow`,
//!
//! regresses INV-4 and trips this file. Both halves live in the same
//! decision-table guard for compactness — the old v3
//! `initial_status_for_resume` observability seam (a no-logic helper
//! that returned the planted struct literal) was deleted alongside
//! this rewrite because `SpecPushPhase::Resumed` is a public enum
//! variant the test can name directly, making the seam unnecessary.
//!
//! ### Pre-rebase history (for the curious)
//!
//! Pre-#323, the seam-based test in this file failed for the right
//! reason: `decide(initial_status_for_resume(…).phase) == StartTurnNow`
//! because the planted phase was `Default::default() == Idle`. Post-#323
//! the seam was unnecessary and the test was rewritten to name
//! `SpecPushPhase::Resumed` directly.
//!
//! See: `src/spec_appserver.rs::build_handle_after_spawn_resume` (plants
//! `Resumed`), `SpecPushPhase::Resumed` (the new variant), `decide` (the
//! decision table). PR #323 for R2-B2.

use calm_server::spec_appserver::{PushAction, SpecPushPhase, decide};

/// INV-4 regression guard: the full `decide` decision table, including
/// the `Resumed` arm added by #323.
///
/// **What this test pins (and what it doesn't)**: it asserts the
/// **decision-table** half of the #323 R2-B2 fix — `decide(Resumed) ==
/// Enqueue` — so a future change that either (a) drops the `Resumed`
/// variant (compile error here) or (b) flips `decide(Resumed)` to
/// `StartTurnNow` (assertion fails) regresses INV-4. It does NOT verify
/// that `build_handle_after_spawn_resume` actually plants `Resumed`
/// post-`thread/resume`; that side of the fix is covered by the
/// in-module unit tests in `spec_appserver.rs` (`resume_reconcile_*`)
/// added by #323 itself. The two together pin both halves; this file
/// is the external regression guard for the decision table only.
///
/// The other arms pin the create-wave-path decision table so a fix to
/// the resume initial phase doesn't accidentally regress them:
///
/// * `Idle` / `TurnCompleted` on the create-wave path (where the server
///   really is between turns, by construction — we just sent
///   `thread/start`) must decide `StartTurnNow`.
/// * `PendingThreadStart` must decide `Enqueue` because empty-goal waves do
///   not have a codex thread id until the remote TUI fresh-starts one.
/// * `TurnRunning` / `Issuing` must decide `Enqueue`.
#[test]
fn inv4_decision_table_regression_guard() {
    // INV-4 load-bearing arm: #323 R2-B2 fix. `build_handle_after_spawn_resume`
    // plants `Resumed` (not `Idle`) after a successful `thread/resume`,
    // and `decide(Resumed) == Enqueue`. This pins both halves of the fix
    // — a refactor that drops the variant or flips its decision regresses
    // INV-4.
    assert_eq!(
        decide(SpecPushPhase::Resumed),
        PushAction::Enqueue,
        "INV-4 regression: decide(Resumed) MUST be Enqueue. The boot-takeover \
         resume path plants Resumed right after thread/resume because we cannot \
         prove the server is between turns; firing turn/start there risks codex \
         silently dropping it against a mid-turn thread. Restore decide(Resumed) \
         == Enqueue."
    );

    // Create-wave / post-turn-completed: genuinely between turns by
    // construction, so turn/start is safe.
    assert_eq!(decide(SpecPushPhase::Idle), PushAction::StartTurnNow);
    assert_eq!(
        decide(SpecPushPhase::TurnCompleted),
        PushAction::StartTurnNow
    );

    // A turn is in flight (or being issued): enqueue.
    assert_eq!(
        decide(SpecPushPhase::PendingThreadStart),
        PushAction::Enqueue
    );
    assert_eq!(decide(SpecPushPhase::TurnRunning), PushAction::Enqueue);
    assert_eq!(decide(SpecPushPhase::Issuing), PushAction::Enqueue);
}
