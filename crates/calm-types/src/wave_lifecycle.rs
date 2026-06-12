//! Issue #145 — Wave lifecycle state machine.
//!
//! Single source of truth for which `(from, to, actor)` triples are
//! permitted. The Spec Agent drives the happy path, the user owns
//! kickoff / cancel / reopen, worker cards have no authority at all.
//!
//! ## Why this lives in the kernel
//!
//! The Spec Agent's job in the user's framing is to **manage Wave state
//! correctly** — that's only meaningful if "correct" is enforced by the
//! kernel rather than asked of the LLM as a vibe. Every wave-state
//! change funnels through `validate_transition` *before* the
//! `Event::WaveLifecycleChanged` event is persisted. An illegal
//! transition surfaces as `TransitionError`, which the call sites map
//! to `CalmError::Forbidden` so the txn rolls back without persisting
//! either the wave row update or the event.
//!
//! ## #679 PR1 — pure edge table only
//!
//! This module is the IO-free half of calm-server's `wave_lifecycle`:
//! the [`ActorKind`] classifier, [`validate_transition`] and
//! [`TransitionError`]. The in-transaction helpers
//! (`auto_promote_draft_in_tx`, `apply_requested_transition_in_tx`,
//! `auto_transition_if_current_in_tx`) stay in calm-server — they hold a
//! sqlx `Transaction`. The edge table itself is **unchanged** (PR0's
//! `wave_fsm_golden` pins it); only the module path moved.
//!
//! ## Rules at a glance
//!
//! | Edge                                       | User | SpecAgent | Worker |
//! |--------------------------------------------|------|-----------|--------|
//! | `draft → planning`  (kickoff)              | yes  | yes       | no     |
//! | `planning → dispatching`                   | no   | yes       | no     |
//! | `dispatching → working`                    | no   | yes       | no     |
//! | `working → blocked`                        | no   | yes       | no     |
//! | `working → reviewing`                      | no   | yes       | no     |
//! | `blocked → working`                        | yes  | yes       | no     |
//! | `reviewing → working`                      | no   | yes       | no     |
//! | `reviewing → done`                         | no   | yes       | no     |
//! | `reviewing → failed`                       | no   | yes       | no     |
//! | any non-terminal → `canceled`              | yes  | no        | no     |
//! | `done`/`canceled`/`failed` → `planning`    | yes  | no        | no     |
//! | (no-op) `state → same state`               | yes\*| yes\*     | no     |
//!
//! \* Same-state transitions are an **idempotent silent success** for
//! actors that have any lifecycle authority at all (User, SpecAgent /
//! Kernel). The validator returns `Ok(())` early after the actor-
//! authority check; the caller is expected to skip emitting
//! `WaveLifecycleChanged` (nothing changed) and to leave the row's
//! `lifecycle` column / `updated_at` alone if no other patch field
//! also changed. Worker cards and plugins still hit `NotAuthorized` —
//! idempotency only applies once the actor itself is permitted.
//!
//! Anything not on this table is rejected. Worker cards (the
//! `ActorId::AiCodex` / `ActorId::AiClaude` whose `CardRole` is `Worker`)
//! **never** drive a lifecycle transition — they emit task-level events
//! only and the Spec Agent reacts. The Dispatcher
//! (`ActorId::KernelDispatcher`) is
//! likewise out of the lifecycle business; it reports dispatch results
//! and the Spec Agent decides what state follows.
//!
//! ## Scope vs role gate
//!
//! `role_gate::enforce_role` already polices "who can emit which event
//! variant" at the wire-event level. This module is one level deeper:
//! once a caller has the right to emit `Event::WaveLifecycleChanged`
//! at all, **which transitions** are they allowed to express? Both
//! gates run; this one is the narrower predicate.

use crate::ids::ActorId;
use crate::model::WaveLifecycle;
use thiserror::Error;

/// A semantic label for the actor in lifecycle terms. Maps cleanly to
/// `ActorId` via [`actor_kind`]:
///
///   * `User` → the human via REST.
///   * `SpecAgent` → an `ActorId::AiSpec(_)` (the wave's spec card).
///     Also catches `ActorId::Kernel` and `ActorId::KernelDispatcher`
///     as a defense-in-depth — they're server-internal kernel writes
///     and can drive any spec-permitted transition.
///   * `Worker` → an `ActorId::AiCodex(_)` / `ActorId::AiClaude(_)`.
///     Always rejected.
///   * `Other` → plugins, etc. Rejected; lifecycle is not theirs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorKind {
    User,
    SpecAgent,
    Worker,
    Other,
}

/// Classify an `ActorId` into the lifecycle authority label.
///
/// Notes on the AiSpec / worker split:
///   * `ActorId::AiSpec(_)` is always treated as `SpecAgent`. The
///     in-tx `role_gate` already confirms the bound card's role is
///     actually `Spec` (a misnamed AiSpec carrying a non-Spec card is
///     refused upstream before this function runs).
///   * `ActorId::AiCodex(_)` / `ActorId::AiClaude(_)` is `Worker`
///     regardless of cached role and has no lifecycle authority by
///     construction.
pub fn actor_kind(actor: &ActorId) -> ActorKind {
    match actor {
        ActorId::User => ActorKind::User,
        ActorId::Kernel | ActorId::KernelDispatcher | ActorId::AiSpec(_) => ActorKind::SpecAgent,
        ActorId::AiCodex(_) | ActorId::AiClaude(_) => ActorKind::Worker,
        ActorId::Plugin(_) => ActorKind::Other,
    }
}

/// What the validator returns when a transition is denied. Mapped at
/// the call sites to `CalmError::Forbidden` (HTTP / MCP) so test
/// assertions can pattern-match on the structured variant rather than
/// parsing a free-form string.
///
/// Note: same-state transitions (`from == to`) by a lifecycle-
/// authorized actor are **not** an error variant — they return
/// `Ok(())` so retries and idempotent client behavior succeed
/// silently. The caller is responsible for not emitting a
/// `WaveLifecycleChanged` event when nothing changed; see
/// `routes::waves::update_wave` and the MCP per-write lifecycle
/// handlers.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TransitionError {
    /// The (from → to) edge is structurally impossible regardless of
    /// who tried it (e.g. `done → working` by anyone, `draft → done`
    /// by anyone).
    #[error("wave lifecycle: illegal transition {from:?} → {to:?}")]
    IllegalEdge {
        from: WaveLifecycle,
        to: WaveLifecycle,
    },

    /// The (from → to) edge exists, but this actor isn't authorized
    /// to drive it. E.g. a Spec Agent trying to cancel (cancel is
    /// user-only), or a User trying to perform `planning →
    /// dispatching` (spec-only). Worker cards and plugins fall into
    /// this bucket for *any* transition — including same-state — since
    /// they have zero lifecycle authority.
    #[error(
        "wave lifecycle: actor {actor_kind:?} may not drive {from:?} → {to:?} \
         (this edge is restricted)"
    )]
    NotAuthorized {
        from: WaveLifecycle,
        to: WaveLifecycle,
        actor_kind: ActorKind,
    },
}

/// Validate a wave lifecycle transition against the rule table.
///
/// `Ok(())` permits the call site to proceed. When `from != to` the
/// caller writes the row update and emits `Event::WaveLifecycleChanged`.
/// When `from == to` (and the actor is permitted to touch lifecycle
/// at all) the call is a silent idempotent no-op — the caller must
/// **not** emit `WaveLifecycleChanged`, must not bump `updated_at`
/// solely on account of the lifecycle field, and must return success
/// to the client. This idempotent shortcut keeps client retries clean
/// (an LLM re-sending the same lifecycle target doesn't pollute
/// the event log) without softening the actor-authority check below:
/// a Worker card sending `lifecycle: planning` while the wave is
/// already planning still hits `NotAuthorized`.
///
/// `Err(_)` must roll the transaction back without persisting either
/// the row update or any event.
pub fn validate_transition(
    from: WaveLifecycle,
    to: WaveLifecycle,
    actor: &ActorId,
) -> Result<(), TransitionError> {
    let kind = actor_kind(actor);

    // Worker cards never drive lifecycle. Reject up front so the
    // edge-permission table below stays focused on the User/SpecAgent
    // split — and so even a same-state `lifecycle: X` from a worker
    // hits `NotAuthorized` rather than the idempotency shortcut.
    if kind == ActorKind::Worker {
        return Err(TransitionError::NotAuthorized {
            from,
            to,
            actor_kind: kind,
        });
    }
    // Plugins / other namespaces are likewise rejected — lifecycle is
    // a wave-internal contract.
    if kind == ActorKind::Other {
        return Err(TransitionError::NotAuthorized {
            from,
            to,
            actor_kind: kind,
        });
    }

    // Idempotent same-state shortcut. Only authorized actors get here
    // (Worker/Other were rejected above), so this is "you have
    // lifecycle authority and you asked for the state we're already
    // in — that's fine, do nothing." The caller is responsible for
    // skipping `WaveLifecycleChanged` emission; see the doc comment.
    if from == to {
        return Ok(());
    }

    // Cancel is user-only and goes from any non-terminal state. The
    // user can always cancel; the Spec Agent never can (giving up is
    // a human decision in Neige's product model).
    if to == WaveLifecycle::Canceled {
        if from.is_terminal() {
            return Err(TransitionError::IllegalEdge { from, to });
        }
        return match kind {
            ActorKind::User => Ok(()),
            _ => Err(TransitionError::NotAuthorized {
                from,
                to,
                actor_kind: kind,
            }),
        };
    }

    // Reopen: terminal (done/canceled/failed) → planning is the
    // user-only escape hatch. Picking `planning` keeps the reopened
    // wave on the happy path's first non-draft state — the user
    // doesn't have to re-do goal entry, but the Spec Agent gets a
    // clean start when it next picks up the wave.
    if from.is_terminal() {
        if to == WaveLifecycle::Planning {
            return match kind {
                ActorKind::User => Ok(()),
                _ => Err(TransitionError::NotAuthorized {
                    from,
                    to,
                    actor_kind: kind,
                }),
            };
        }
        return Err(TransitionError::IllegalEdge { from, to });
    }

    // Happy-path edges. The (from, to) tuple must be in this table;
    // the per-edge authority list narrows down to User vs SpecAgent.
    // `to == Canceled` and the reopen branch were handled above; this
    // table is for the non-cancel, non-reopen edges.
    let (allow_user, allow_spec) = match (from, to) {
        // Kickoff. Both User (manual start) and SpecAgent (auto-
        // start) can drive draft → planning. The spec-driven path lets
        // harness-backed spec sessions advance themselves; the user-driven
        // path is the "Start" button in the UI.
        (WaveLifecycle::Draft, WaveLifecycle::Planning) => (true, true),

        // Spec-only progressions through the happy path.
        (WaveLifecycle::Planning, WaveLifecycle::Dispatching) => (false, true),
        (WaveLifecycle::Dispatching, WaveLifecycle::Working) => (false, true),
        (WaveLifecycle::Working, WaveLifecycle::Blocked) => (false, true),
        (WaveLifecycle::Working, WaveLifecycle::Reviewing) => (false, true),
        (WaveLifecycle::Reviewing, WaveLifecycle::Working) => (false, true),
        (WaveLifecycle::Reviewing, WaveLifecycle::Done) => (false, true),
        (WaveLifecycle::Reviewing, WaveLifecycle::Failed) => (false, true),

        // Unblock. Both User (manual unblock after providing input)
        // and SpecAgent (auto-recovery) can drive blocked → working.
        (WaveLifecycle::Blocked, WaveLifecycle::Working) => (true, true),

        // Everything else is structurally illegal regardless of
        // actor — e.g. draft → done (no skipping the pipeline),
        // planning → done (no skipping review), etc.
        _ => return Err(TransitionError::IllegalEdge { from, to }),
    };

    match kind {
        ActorKind::User if allow_user => Ok(()),
        ActorKind::SpecAgent if allow_spec => Ok(()),
        _ => Err(TransitionError::NotAuthorized {
            from,
            to,
            actor_kind: kind,
        }),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::CardId;

    // ----- Helpers ----------------------------------------------------------

    const ALL_STATES: [WaveLifecycle; 9] = [
        WaveLifecycle::Draft,
        WaveLifecycle::Planning,
        WaveLifecycle::Dispatching,
        WaveLifecycle::Working,
        WaveLifecycle::Blocked,
        WaveLifecycle::Reviewing,
        WaveLifecycle::Done,
        WaveLifecycle::Canceled,
        WaveLifecycle::Failed,
    ];

    fn user() -> ActorId {
        ActorId::User
    }

    fn spec() -> ActorId {
        ActorId::AiSpec(CardId::from("spec-1"))
    }

    fn worker() -> ActorId {
        ActorId::AiCodex(CardId::from("worker-1"))
    }

    fn claude_worker() -> ActorId {
        ActorId::AiClaude(CardId::from("worker-claude-1"))
    }

    fn plugin() -> ActorId {
        ActorId::Plugin("hello-world".into())
    }

    /// Mirror of the docs-comment rule table — the *only* `(from, to,
    /// actor)` triples that must validate `Ok`. Everything else must
    /// reject. Hand-derived from the issue so the table here is the
    /// readable contract; the implementation in
    /// `validate_transition` is checked against it.
    fn legal_edges() -> Vec<(WaveLifecycle, WaveLifecycle, ActorKind)> {
        use WaveLifecycle as L;
        let mut edges = vec![
            // kickoff (both)
            (L::Draft, L::Planning, ActorKind::User),
            (L::Draft, L::Planning, ActorKind::SpecAgent),
            // spec-only happy path
            (L::Planning, L::Dispatching, ActorKind::SpecAgent),
            (L::Dispatching, L::Working, ActorKind::SpecAgent),
            (L::Working, L::Blocked, ActorKind::SpecAgent),
            (L::Working, L::Reviewing, ActorKind::SpecAgent),
            (L::Reviewing, L::Working, ActorKind::SpecAgent),
            (L::Reviewing, L::Done, ActorKind::SpecAgent),
            (L::Reviewing, L::Failed, ActorKind::SpecAgent),
            // unblock (both)
            (L::Blocked, L::Working, ActorKind::User),
            (L::Blocked, L::Working, ActorKind::SpecAgent),
            // user-only: cancel from any non-terminal
            (L::Draft, L::Canceled, ActorKind::User),
            (L::Planning, L::Canceled, ActorKind::User),
            (L::Dispatching, L::Canceled, ActorKind::User),
            (L::Working, L::Canceled, ActorKind::User),
            (L::Blocked, L::Canceled, ActorKind::User),
            (L::Reviewing, L::Canceled, ActorKind::User),
            // user-only: reopen any terminal → planning
            (L::Done, L::Planning, ActorKind::User),
            (L::Canceled, L::Planning, ActorKind::User),
            (L::Failed, L::Planning, ActorKind::User),
        ];
        // Idempotent same-state shortcut: any authorized actor
        // (User, SpecAgent) gets `Ok(())` for `state → same state`.
        // Workers and plugins still hit `NotAuthorized`; that's
        // covered by the per-actor prohibition tests below.
        for state in ALL_STATES {
            edges.push((state, state, ActorKind::User));
            edges.push((state, state, ActorKind::SpecAgent));
        }
        edges
    }

    fn actor_for_kind(kind: ActorKind) -> ActorId {
        match kind {
            ActorKind::User => user(),
            ActorKind::SpecAgent => spec(),
            ActorKind::Worker => worker(),
            ActorKind::Other => plugin(),
        }
    }

    // ----- Exhaustive table -------------------------------------------------

    #[test]
    fn exhaustive_transition_table_matches_rule_set() {
        // For every (from, to, actor) triple in
        // {9 states} × {9 states} × {User, SpecAgent, Worker, Other},
        // assert validate_transition's answer matches the rule table.
        let legal: std::collections::HashSet<_> = legal_edges().into_iter().collect();

        for from in ALL_STATES {
            for to in ALL_STATES {
                for kind in [
                    ActorKind::User,
                    ActorKind::SpecAgent,
                    ActorKind::Worker,
                    ActorKind::Other,
                ] {
                    let actor = actor_for_kind(kind);
                    let res = validate_transition(from, to, &actor);
                    let expected_ok = legal.contains(&(from, to, kind));
                    match (expected_ok, res) {
                        (true, Ok(())) => {}
                        (false, Err(_)) => {}
                        (true, Err(e)) => panic!(
                            "expected legal {from:?} -> {to:?} for {kind:?}, got error {e:?}"
                        ),
                        (false, Ok(())) => panic!(
                            "expected illegal {from:?} -> {to:?} for {kind:?}, but validator accepted"
                        ),
                    }
                }
            }
        }
    }

    // ----- Worker prohibition ----------------------------------------------

    #[test]
    fn ai_claude_classifies_as_worker() {
        assert_eq!(actor_kind(&claude_worker()), ActorKind::Worker);
    }

    #[test]
    fn worker_cards_can_never_change_lifecycle() {
        // No (from, to) pair is permitted when the actor is a worker —
        // including same-state requests. The idempotent shortcut for
        // `from == to` only applies once the actor has lifecycle
        // authority, which Worker never does.
        for from in ALL_STATES {
            for to in ALL_STATES {
                for actor in [worker(), claude_worker()] {
                    let res = validate_transition(from, to, &actor);
                    assert!(
                        res.is_err(),
                        "worker should be forbidden for {from:?} -> {to:?} as {actor:?}, got {res:?}"
                    );
                }
            }
        }
    }

    // ----- Plugin / Other prohibition --------------------------------------

    #[test]
    fn plugins_cannot_change_lifecycle() {
        for from in ALL_STATES {
            for to in ALL_STATES {
                let res = validate_transition(from, to, &plugin());
                assert!(
                    res.is_err(),
                    "plugin should be forbidden for {from:?} -> {to:?}, got {res:?}"
                );
            }
        }
    }

    // ----- Anti-skip rules -------------------------------------------------

    #[test]
    fn cannot_skip_to_done_from_anywhere_but_reviewing() {
        for from in ALL_STATES {
            if from == WaveLifecycle::Reviewing || from == WaveLifecycle::Done {
                continue;
            }
            for actor in [user(), spec()] {
                let res = validate_transition(from, WaveLifecycle::Done, &actor);
                assert!(
                    res.is_err(),
                    "must not skip to Done from {from:?} as {actor:?}: {res:?}"
                );
            }
        }
    }

    #[test]
    fn cannot_regress_from_terminal_except_reopen_to_planning() {
        // Terminal states allow reopen → planning (user-only) and
        // nothing else. The exhaustive table already covers this; this
        // test exists for documentation: it's the human-readable
        // counterexample list.
        for from in [
            WaveLifecycle::Done,
            WaveLifecycle::Canceled,
            WaveLifecycle::Failed,
        ] {
            for to in ALL_STATES {
                if to == WaveLifecycle::Planning {
                    continue; // covered by the reopen rule
                }
                if from == to {
                    continue; // same-state idempotency — covered separately
                }
                for actor in [user(), spec(), worker(), plugin()] {
                    let res = validate_transition(from, to, &actor);
                    assert!(
                        res.is_err(),
                        "expected reject from terminal {from:?} -> {to:?} as {actor:?}: {res:?}",
                    );
                }
            }
        }
    }

    // ----- Same-state idempotency -----------------------------------------

    #[test]
    fn same_state_is_idempotent_for_authorized_actors() {
        // An authorized actor (User, SpecAgent / Kernel) asking for
        // the state the wave is already in is a silent success. The
        // caller is then expected to skip the `WaveLifecycleChanged`
        // emit; call-site integration tests cover the no-event path.
        for state in ALL_STATES {
            for actor in [user(), spec(), ActorId::Kernel, ActorId::KernelDispatcher] {
                let res = validate_transition(state, state, &actor);
                assert!(
                    res.is_ok(),
                    "expected idempotent Ok for {state:?} -> {state:?} as {actor:?}, got {res:?}"
                );
            }
        }
    }

    #[test]
    fn same_state_still_rejects_unauthorized_actors() {
        // Idempotency does NOT widen actor authority. A Worker / Plugin
        // sending `lifecycle: <current>` must still hit `NotAuthorized`
        // — these actors have zero lifecycle authority by construction,
        // and silently accepting their no-op write would let bugs
        // (worker mis-emits) hide instead of surfacing.
        for state in ALL_STATES {
            for actor in [worker(), claude_worker(), plugin()] {
                let res = validate_transition(state, state, &actor);
                assert!(
                    matches!(res, Err(TransitionError::NotAuthorized { .. })),
                    "expected NotAuthorized for {state:?} -> {state:?} as {actor:?}, got {res:?}"
                );
            }
        }
    }

    // ----- Authority split ------------------------------------------------

    #[test]
    fn spec_cannot_cancel() {
        // The user is the only actor that can cancel — the Spec
        // Agent giving up is a human decision in Neige's product
        // model. Pin so a future refactor can't silently widen this.
        for from in [
            WaveLifecycle::Draft,
            WaveLifecycle::Planning,
            WaveLifecycle::Dispatching,
            WaveLifecycle::Working,
            WaveLifecycle::Blocked,
            WaveLifecycle::Reviewing,
        ] {
            let res = validate_transition(from, WaveLifecycle::Canceled, &spec());
            assert!(
                matches!(res, Err(TransitionError::NotAuthorized { .. })),
                "spec should not cancel {from:?}: {res:?}"
            );
        }
    }

    #[test]
    fn user_cannot_drive_spec_only_progressions() {
        // The user is not allowed to skip ahead on the spec-driven
        // happy path. Each of these edges is `(false, true)` in the
        // implementation; we re-assert here so an accidental flip to
        // `(true, true)` surfaces as a test diff.
        let spec_only = [
            (WaveLifecycle::Planning, WaveLifecycle::Dispatching),
            (WaveLifecycle::Dispatching, WaveLifecycle::Working),
            (WaveLifecycle::Working, WaveLifecycle::Blocked),
            (WaveLifecycle::Working, WaveLifecycle::Reviewing),
            (WaveLifecycle::Reviewing, WaveLifecycle::Working),
            (WaveLifecycle::Reviewing, WaveLifecycle::Done),
            (WaveLifecycle::Reviewing, WaveLifecycle::Failed),
        ];
        for (from, to) in spec_only {
            let res = validate_transition(from, to, &user());
            assert!(
                matches!(res, Err(TransitionError::NotAuthorized { .. })),
                "user should not drive spec-only edge {from:?} -> {to:?}: {res:?}"
            );
        }
    }

    #[test]
    fn kernel_and_kernel_dispatcher_treated_as_spec_for_lifecycle() {
        // Server-internal kernel actors are allowed to drive
        // spec-permitted edges. This is defense-in-depth: nothing in
        // the codebase emits lifecycle events as Kernel today, but
        // when (say) a recovery task does, the gate should treat it
        // like a Spec Agent rather than refusing outright.
        for actor in [ActorId::Kernel, ActorId::KernelDispatcher] {
            assert!(
                validate_transition(WaveLifecycle::Planning, WaveLifecycle::Dispatching, &actor)
                    .is_ok(),
                "kernel-class actor should be allowed to drive spec edges (actor={actor:?})"
            );
        }
    }

    // ----- Serde round-trip pin -------------------------------------------

    #[test]
    fn serde_round_trip_pinned_lowercase() {
        // Lock the wire shape: lowercase variant names. Migration
        // 0012 stores these literal strings in `waves.lifecycle`,
        // and the frontend zod schema validates against them.
        for (state, json) in [
            (WaveLifecycle::Draft, "\"draft\""),
            (WaveLifecycle::Planning, "\"planning\""),
            (WaveLifecycle::Dispatching, "\"dispatching\""),
            (WaveLifecycle::Working, "\"working\""),
            (WaveLifecycle::Blocked, "\"blocked\""),
            (WaveLifecycle::Reviewing, "\"reviewing\""),
            (WaveLifecycle::Done, "\"done\""),
            (WaveLifecycle::Canceled, "\"canceled\""),
            (WaveLifecycle::Failed, "\"failed\""),
        ] {
            let s = serde_json::to_string(&state).expect("serialize");
            assert_eq!(s, json, "serialize mismatch for {state:?}");
            let back: WaveLifecycle = serde_json::from_str(json).expect("deserialize");
            assert_eq!(back, state, "round-trip mismatch for {json}");
        }
    }

    #[test]
    fn default_is_draft() {
        assert_eq!(WaveLifecycle::default(), WaveLifecycle::Draft);
    }

    #[test]
    fn is_terminal_marks_only_three() {
        for s in ALL_STATES {
            let expected = matches!(
                s,
                WaveLifecycle::Done | WaveLifecycle::Canceled | WaveLifecycle::Failed
            );
            assert_eq!(s.is_terminal(), expected, "is_terminal({s:?}) wrong");
        }
    }
}
