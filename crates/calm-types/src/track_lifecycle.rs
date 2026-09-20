//! Track lifecycle state machine: the single source of truth for which `(from, to, actor)` triples
//! are permitted. Same-state transitions by an authorized actor are an idempotent silent `Ok(())`;
//! the caller must not emit `TrackLifecycleChanged` for them.

use crate::ids::ActorId;
use crate::model::TrackLifecycle;
use thiserror::Error;

/// A semantic label for the actor in lifecycle terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorKind {
    User,
    PlannerAgent,
    Worker,
    Other,
}

/// Classify an `ActorId` into the lifecycle authority label.
pub fn actor_kind(actor: &ActorId) -> ActorKind {
    match actor {
        ActorId::User => ActorKind::User,
        ActorId::Kernel
        | ActorId::KernelDispatcher
        | ActorId::AiPlanner(_)
        | ActorId::AiPlannerSession(_) => ActorKind::PlannerAgent,
        ActorId::AiCodex(_)
        | ActorId::AiClaude(_)
        | ActorId::AiCodexSession(_)
        | ActorId::AiClaudeSession(_) => ActorKind::Worker,
        ActorId::Plugin(_) => ActorKind::Other,
    }
}

/// True when the actor represents the planner author identity, independent of
/// whether the actor is still card-keyed or already session-keyed.
pub fn actor_is_planner_author(actor: &ActorId) -> bool {
    matches!(actor, ActorId::AiPlanner(_) | ActorId::AiPlannerSession(_))
}

/// What the validator returns when a transition is denied.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TransitionError {
    /// The (from → to) edge is structurally impossible regardless of who tried it.
    #[error("track lifecycle: illegal transition {from:?} → {to:?}")]
    IllegalEdge {
        from: TrackLifecycle,
        to: TrackLifecycle,
    },

    /// The (from → to) edge exists, but this actor isn't authorized to drive it.
    #[error(
        "track lifecycle: actor {actor_kind:?} may not drive {from:?} → {to:?} \
         (this edge is restricted)"
    )]
    NotAuthorized {
        from: TrackLifecycle,
        to: TrackLifecycle,
        actor_kind: ActorKind,
    },
}

/// Whether the user may apply the product's one `Resume work` action from this lifecycle.
pub fn user_can_resume(lifecycle: TrackLifecycle) -> bool {
    matches!(
        lifecycle,
        TrackLifecycle::Blocked
            | TrackLifecycle::Reviewing
            | TrackLifecycle::Done
            | TrackLifecycle::Canceled
            | TrackLifecycle::Failed
    )
}

/// Every lifecycle the Planner Agent may move a track to from `from`, excluding the same-state
/// no-op; derived from [`validate_transition`] so there is no second edge table to drift.
pub fn planner_allowed_targets(from: TrackLifecycle) -> Vec<TrackLifecycle> {
    const ALL: [TrackLifecycle; 9] = [
        TrackLifecycle::Draft,
        TrackLifecycle::Planning,
        TrackLifecycle::Dispatching,
        TrackLifecycle::Working,
        TrackLifecycle::Blocked,
        TrackLifecycle::Reviewing,
        TrackLifecycle::Done,
        TrackLifecycle::Canceled,
        TrackLifecycle::Failed,
    ];
    let planner = ActorId::AiPlanner(crate::ids::CardId::from(""));
    ALL.into_iter()
        .filter(|&to| to != from && validate_transition(from, to, &planner).is_ok())
        .collect()
}

/// Validate a track lifecycle transition against the rule table. `from == to` by a permitted actor
/// is a silent idempotent no-op: the caller must **not** emit `TrackLifecycleChanged`. `Err(_)` must
/// roll the transaction back without persisting the row update or any event.
pub fn validate_transition(
    from: TrackLifecycle,
    to: TrackLifecycle,
    actor: &ActorId,
) -> Result<(), TransitionError> {
    let kind = actor_kind(actor);

    // Workers are rejected up front so even a same-state request hits `NotAuthorized` rather than the
    // idempotency shortcut.
    if kind == ActorKind::Worker {
        return Err(TransitionError::NotAuthorized {
            from,
            to,
            actor_kind: kind,
        });
    }
    if kind == ActorKind::Other {
        return Err(TransitionError::NotAuthorized {
            from,
            to,
            actor_kind: kind,
        });
    }

    if from == to {
        return Ok(());
    }

    // Cancel is user-only from any non-terminal state; giving up is a human decision.
    if to == TrackLifecycle::Canceled {
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

    // User recovery: a human may correct any waiting / terminal state back to Working; deliberately
    // not a general user override over the FSM.
    if to == TrackLifecycle::Working && user_can_resume(from) && kind == ActorKind::User {
        return Ok(());
    }

    // Reopen: terminal → planning is the other user-only escape hatch.
    if from.is_terminal() {
        if to == TrackLifecycle::Planning {
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

    let (allow_user, allow_planner) = match (from, to) {
        (TrackLifecycle::Draft, TrackLifecycle::Planning) => (true, true),

        // Dead-root convergence: the reaper drives a stalled Draft/Planning root to Failed on a POSITIVE
        // dead signal; planner authority so a user cannot skip a live track to Failed.
        (TrackLifecycle::Draft, TrackLifecycle::Failed) => (false, true),
        (TrackLifecycle::Planning, TrackLifecycle::Failed) => (false, true),

        // Planner-only progressions through the happy path.
        (TrackLifecycle::Planning, TrackLifecycle::Dispatching) => (false, true),
        (TrackLifecycle::Dispatching, TrackLifecycle::Working) => (false, true),
        (TrackLifecycle::Working, TrackLifecycle::Blocked) => (false, true),
        (TrackLifecycle::Working, TrackLifecycle::Reviewing) => (false, true),
        // Self-executed conclusion: the planner may go straight to `Reviewing`; `planning → done` stays illegal.
        (TrackLifecycle::Planning, TrackLifecycle::Reviewing) => (false, true),
        (TrackLifecycle::Reviewing, TrackLifecycle::Working) => (false, true),
        (TrackLifecycle::Reviewing, TrackLifecycle::Done) => (false, true),
        (TrackLifecycle::Reviewing, TrackLifecycle::Failed) => (false, true),

        (TrackLifecycle::Blocked, TrackLifecycle::Working) => (true, true),

        _ => return Err(TransitionError::IllegalEdge { from, to }),
    };

    match kind {
        ActorKind::User if allow_user => Ok(()),
        ActorKind::PlannerAgent if allow_planner => Ok(()),
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

    const ALL_STATES: [TrackLifecycle; 9] = [
        TrackLifecycle::Draft,
        TrackLifecycle::Planning,
        TrackLifecycle::Dispatching,
        TrackLifecycle::Working,
        TrackLifecycle::Blocked,
        TrackLifecycle::Reviewing,
        TrackLifecycle::Done,
        TrackLifecycle::Canceled,
        TrackLifecycle::Failed,
    ];

    fn user() -> ActorId {
        ActorId::User
    }

    fn planner() -> ActorId {
        ActorId::AiPlanner(CardId::from("planner-1"))
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

    /// Mirror of the rule table — the only `(from, to, actor)` triples that must validate `Ok`.
    fn legal_edges() -> Vec<(TrackLifecycle, TrackLifecycle, ActorKind)> {
        use TrackLifecycle as L;
        let mut edges = vec![
            // kickoff (both)
            (L::Draft, L::Planning, ActorKind::User),
            (L::Draft, L::Planning, ActorKind::PlannerAgent),
            // planner-only happy path
            (L::Planning, L::Dispatching, ActorKind::PlannerAgent),
            (L::Dispatching, L::Working, ActorKind::PlannerAgent),
            (L::Working, L::Blocked, ActorKind::PlannerAgent),
            (L::Working, L::Reviewing, ActorKind::PlannerAgent),
            // self-executed conclusion
            (L::Planning, L::Reviewing, ActorKind::PlannerAgent),
            (L::Reviewing, L::Working, ActorKind::PlannerAgent),
            (L::Reviewing, L::Done, ActorKind::PlannerAgent),
            (L::Reviewing, L::Failed, ActorKind::PlannerAgent),
            // kernel dead-root convergence (planner-authority)
            (L::Draft, L::Failed, ActorKind::PlannerAgent),
            (L::Planning, L::Failed, ActorKind::PlannerAgent),
            // unblock (both)
            (L::Blocked, L::Working, ActorKind::User),
            (L::Blocked, L::Working, ActorKind::PlannerAgent),
            // user-only resume
            (L::Reviewing, L::Working, ActorKind::User),
            (L::Done, L::Working, ActorKind::User),
            (L::Canceled, L::Working, ActorKind::User),
            (L::Failed, L::Working, ActorKind::User),
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
        for state in ALL_STATES {
            edges.push((state, state, ActorKind::User));
            edges.push((state, state, ActorKind::PlannerAgent));
        }
        edges
    }

    fn actor_for_kind(kind: ActorKind) -> ActorId {
        match kind {
            ActorKind::User => user(),
            ActorKind::PlannerAgent => planner(),
            ActorKind::Worker => worker(),
            ActorKind::Other => plugin(),
        }
    }

    #[test]
    fn exhaustive_transition_table_matches_rule_set() {
        let legal: std::collections::HashSet<_> = legal_edges().into_iter().collect();

        for from in ALL_STATES {
            for to in ALL_STATES {
                for kind in [
                    ActorKind::User,
                    ActorKind::PlannerAgent,
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

    #[test]
    fn ai_claude_classifies_as_worker() {
        assert_eq!(actor_kind(&claude_worker()), ActorKind::Worker);
    }

    #[test]
    fn worker_cards_can_never_change_lifecycle() {
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

    #[test]
    fn cannot_skip_to_done_from_anywhere_but_reviewing() {
        for from in ALL_STATES {
            if from == TrackLifecycle::Reviewing || from == TrackLifecycle::Done {
                continue;
            }
            for actor in [user(), planner()] {
                let res = validate_transition(from, TrackLifecycle::Done, &actor);
                assert!(
                    res.is_err(),
                    "must not skip to Done from {from:?} as {actor:?}: {res:?}"
                );
            }
        }
    }

    fn sorted_targets(from: TrackLifecycle) -> Vec<TrackLifecycle> {
        let mut v = planner_allowed_targets(from);
        v.sort_by_key(|l| format!("{l:?}"));
        v
    }

    fn sorted(mut v: Vec<TrackLifecycle>) -> Vec<TrackLifecycle> {
        v.sort_by_key(|l| format!("{l:?}"));
        v
    }

    #[test]
    fn planner_allowed_targets_from_planning_include_self_executed_review() {
        use TrackLifecycle as L;
        assert_eq!(
            sorted_targets(L::Planning),
            sorted(vec![L::Dispatching, L::Reviewing, L::Failed])
        );
    }

    #[test]
    fn planner_allowed_targets_from_reviewing_conclude_or_resume() {
        use TrackLifecycle as L;
        assert_eq!(
            sorted_targets(L::Reviewing),
            sorted(vec![L::Working, L::Done, L::Failed])
        );
    }

    #[test]
    fn planner_allowed_targets_from_terminal_states_are_empty() {
        use TrackLifecycle as L;
        for from in [L::Done, L::Canceled, L::Failed] {
            assert!(
                planner_allowed_targets(from).is_empty(),
                "planner has no edge out of {from:?}"
            );
        }
    }

    #[test]
    fn planner_allowed_targets_mirror_legal_edges_table() {
        for from in ALL_STATES {
            let expected: Vec<TrackLifecycle> = legal_edges()
                .into_iter()
                .filter(|(f, t, k)| *f == from && *t != from && *k == ActorKind::PlannerAgent)
                .map(|(_, t, _)| t)
                .collect();
            assert_eq!(sorted_targets(from), sorted(expected), "from {from:?}");
        }
    }

    #[test]
    fn user_can_resume_recoverable_states_to_working() {
        for from in [
            TrackLifecycle::Blocked,
            TrackLifecycle::Reviewing,
            TrackLifecycle::Done,
            TrackLifecycle::Canceled,
            TrackLifecycle::Failed,
        ] {
            let result = validate_transition(from, TrackLifecycle::Working, &user());
            assert!(
                result.is_ok(),
                "user should be able to resume {from:?} -> Working, got {result:?}"
            );
        }
    }

    #[test]
    fn terminal_states_only_allow_user_reopen_or_resume() {
        for from in [
            TrackLifecycle::Done,
            TrackLifecycle::Canceled,
            TrackLifecycle::Failed,
        ] {
            for to in ALL_STATES {
                if from == to {
                    continue; // same-state idempotency — covered separately
                }
                for actor in [user(), planner(), worker(), plugin()] {
                    if actor == user()
                        && matches!(to, TrackLifecycle::Planning | TrackLifecycle::Working)
                    {
                        continue;
                    }
                    let res = validate_transition(from, to, &actor);
                    assert!(
                        res.is_err(),
                        "expected reject from terminal {from:?} -> {to:?} as {actor:?}: {res:?}",
                    );
                }
            }
        }
    }

    #[test]
    fn same_state_is_idempotent_for_authorized_actors() {
        for state in ALL_STATES {
            for actor in [
                user(),
                planner(),
                ActorId::Kernel,
                ActorId::KernelDispatcher,
            ] {
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

    #[test]
    fn planner_cannot_cancel() {
        for from in [
            TrackLifecycle::Draft,
            TrackLifecycle::Planning,
            TrackLifecycle::Dispatching,
            TrackLifecycle::Working,
            TrackLifecycle::Blocked,
            TrackLifecycle::Reviewing,
        ] {
            let res = validate_transition(from, TrackLifecycle::Canceled, &planner());
            assert!(
                matches!(res, Err(TransitionError::NotAuthorized { .. })),
                "planner should not cancel {from:?}: {res:?}"
            );
        }
    }

    #[test]
    fn user_cannot_drive_non_recovery_planner_progressions() {
        let planner_only = [
            (TrackLifecycle::Planning, TrackLifecycle::Dispatching),
            (TrackLifecycle::Dispatching, TrackLifecycle::Working),
            (TrackLifecycle::Working, TrackLifecycle::Blocked),
            (TrackLifecycle::Working, TrackLifecycle::Reviewing),
            (TrackLifecycle::Reviewing, TrackLifecycle::Done),
            (TrackLifecycle::Reviewing, TrackLifecycle::Failed),
            (TrackLifecycle::Draft, TrackLifecycle::Failed),
            (TrackLifecycle::Planning, TrackLifecycle::Failed),
        ];
        for (from, to) in planner_only {
            let res = validate_transition(from, to, &user());
            assert!(
                matches!(res, Err(TransitionError::NotAuthorized { .. })),
                "user should not drive planner-only edge {from:?} -> {to:?}: {res:?}"
            );
        }
    }

    #[test]
    fn kernel_and_kernel_dispatcher_treated_as_planner_for_lifecycle() {
        for actor in [ActorId::Kernel, ActorId::KernelDispatcher] {
            assert!(
                validate_transition(
                    TrackLifecycle::Planning,
                    TrackLifecycle::Dispatching,
                    &actor
                )
                .is_ok(),
                "kernel-class actor should be allowed to drive planner edges (actor={actor:?})"
            );
        }
    }

    #[test]
    fn serde_round_trip_pinned_lowercase() {
        for (state, json) in [
            (TrackLifecycle::Draft, "\"draft\""),
            (TrackLifecycle::Planning, "\"planning\""),
            (TrackLifecycle::Dispatching, "\"dispatching\""),
            (TrackLifecycle::Working, "\"working\""),
            (TrackLifecycle::Blocked, "\"blocked\""),
            (TrackLifecycle::Reviewing, "\"reviewing\""),
            (TrackLifecycle::Done, "\"done\""),
            (TrackLifecycle::Canceled, "\"canceled\""),
            (TrackLifecycle::Failed, "\"failed\""),
        ] {
            let s = serde_json::to_string(&state).expect("serialize");
            assert_eq!(s, json, "serialize mismatch for {state:?}");
            let back: TrackLifecycle = serde_json::from_str(json).expect("deserialize");
            assert_eq!(back, state, "round-trip mismatch for {json}");
        }
    }

    #[test]
    fn default_is_draft() {
        assert_eq!(TrackLifecycle::default(), TrackLifecycle::Draft);
    }

    #[test]
    fn is_terminal_marks_only_three() {
        for s in ALL_STATES {
            let expected = matches!(
                s,
                TrackLifecycle::Done | TrackLifecycle::Canceled | TrackLifecycle::Failed
            );
            assert_eq!(s.is_terminal(), expected, "is_terminal({s:?}) wrong");
        }
    }
}
