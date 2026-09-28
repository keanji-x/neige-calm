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

impl ActorKind {
    /// The actor in plain words, as a refusal names it.
    pub fn label(self) -> &'static str {
        match self {
            ActorKind::User => "user",
            ActorKind::PlannerAgent => "planner",
            ActorKind::Worker => "worker",
            ActorKind::Other => "plugin",
        }
    }
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

/// What the validator returns when a transition is denied. Both variants display, in wire names,
/// the lifecycles this actor may write from `from` (see [`allowed_targets`]).
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TransitionError {
    /// The (from → to) edge is structurally impossible regardless of who tried it.
    #[error(
        "track lifecycle: {} → {} is not allowed; {}",
        from.as_db_str(),
        to.as_db_str(),
        legal_targets_clause(*from, *actor_kind)
    )]
    IllegalEdge {
        from: TrackLifecycle,
        to: TrackLifecycle,
        actor_kind: ActorKind,
    },

    /// The (from → to) edge exists, but this actor isn't authorized to drive it.
    #[error(
        "track lifecycle: {} → {} is not allowed for the {}; {}",
        from.as_db_str(),
        to.as_db_str(),
        actor_kind.label(),
        legal_targets_clause(*from, *actor_kind)
    )]
    NotAuthorized {
        from: TrackLifecycle,
        to: TrackLifecycle,
        actor_kind: ActorKind,
    },
}

/// `from planning the planner may write: dispatching, reviewing, failed (…)`, or `… may write nothing`.
/// For the planner, a listed stage the kernel drives on claim carries that note, so the refusal
/// does not invite the stage writes the planner prompt tells it to leave to the kernel.
fn legal_targets_clause(from: TrackLifecycle, kind: ActorKind) -> String {
    let targets = allowed_targets(from, kind);
    let from_name = from.as_db_str();
    let actor = kind.label();
    if targets.is_empty() {
        return format!("from {from_name} the {actor} may write nothing");
    }
    let names: Vec<&str> = targets.iter().map(|t| t.as_db_str()).collect();
    let mut clause = format!(
        "from {from_name} the {actor} may write: {}",
        names.join(", ")
    );
    let kernel_driven: Vec<&str> = targets
        .iter()
        .filter(|t| matches!(t, TrackLifecycle::Dispatching | TrackLifecycle::Working))
        .map(|t| t.as_db_str())
        .collect();
    if kind == ActorKind::PlannerAgent && !kernel_driven.is_empty() {
        clause.push_str(&format!(
            " (the kernel advances {} itself when it claims a task)",
            kernel_driven.join(" and ")
        ));
    }
    clause
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

/// Every lifecycle in declaration order.
const ALL_LIFECYCLES: [TrackLifecycle; 9] = [
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

/// Every lifecycle an actor of `kind` may move a track to from `from`, in declaration order and
/// excluding the same-state no-op; derived from the same [`check`] as [`validate_transition`], so
/// there is no second edge table to drift.
pub fn allowed_targets(from: TrackLifecycle, kind: ActorKind) -> Vec<TrackLifecycle> {
    ALL_LIFECYCLES
        .into_iter()
        .filter(|&to| to != from && check(from, to, kind).is_ok())
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
    let actor_kind = actor_kind(actor);
    check(from, to, actor_kind).map_err(|denial| match denial {
        Denial::IllegalEdge => TransitionError::IllegalEdge {
            from,
            to,
            actor_kind,
        },
        Denial::NotAuthorized => TransitionError::NotAuthorized {
            from,
            to,
            actor_kind,
        },
    })
}

/// Why [`check`] denied an edge; [`validate_transition`] adds the edge and actor.
enum Denial {
    IllegalEdge,
    NotAuthorized,
}

/// The one rule table.
fn check(from: TrackLifecycle, to: TrackLifecycle, kind: ActorKind) -> Result<(), Denial> {
    // Workers and plugins are rejected up front so even a same-state request hits `NotAuthorized`
    // rather than the idempotency shortcut.
    if matches!(kind, ActorKind::Worker | ActorKind::Other) {
        return Err(Denial::NotAuthorized);
    }

    if from == to {
        return Ok(());
    }

    // Cancel is user-only from any non-terminal state; giving up is a human decision.
    if to == TrackLifecycle::Canceled {
        if from.is_terminal() {
            return Err(Denial::IllegalEdge);
        }
        return match kind {
            ActorKind::User => Ok(()),
            _ => Err(Denial::NotAuthorized),
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
                _ => Err(Denial::NotAuthorized),
            };
        }
        return Err(Denial::IllegalEdge);
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

        _ => return Err(Denial::IllegalEdge),
    };

    match kind {
        ActorKind::User if allow_user => Ok(()),
        ActorKind::PlannerAgent if allow_planner => Ok(()),
        _ => Err(Denial::NotAuthorized),
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

    const ALL_KINDS: [ActorKind; 4] = [
        ActorKind::User,
        ActorKind::PlannerAgent,
        ActorKind::Worker,
        ActorKind::Other,
    ];

    #[test]
    fn allowed_targets_for_the_planner_from_planning_include_self_executed_review() {
        use TrackLifecycle as L;
        assert_eq!(
            allowed_targets(L::Planning, ActorKind::PlannerAgent),
            vec![L::Dispatching, L::Reviewing, L::Failed]
        );
    }

    #[test]
    fn allowed_targets_for_the_planner_from_reviewing_conclude_or_resume() {
        use TrackLifecycle as L;
        assert_eq!(
            allowed_targets(L::Reviewing, ActorKind::PlannerAgent),
            vec![L::Working, L::Done, L::Failed]
        );
    }

    #[test]
    fn allowed_targets_for_the_user_from_terminal_are_reopen_or_resume() {
        use TrackLifecycle as L;
        for from in [L::Done, L::Canceled, L::Failed] {
            assert_eq!(
                allowed_targets(from, ActorKind::User),
                vec![L::Planning, L::Working]
            );
            assert!(
                allowed_targets(from, ActorKind::PlannerAgent).is_empty(),
                "planner has no edge out of {from:?}"
            );
        }
    }

    #[test]
    fn allowed_targets_for_workers_and_plugins_are_always_empty() {
        for from in ALL_STATES {
            for kind in [ActorKind::Worker, ActorKind::Other] {
                assert!(allowed_targets(from, kind).is_empty(), "{from:?} {kind:?}");
            }
        }
    }

    /// Order is not asserted here (the tests above pin declaration order); membership must equal
    /// the mirror table for every actor kind.
    #[test]
    fn allowed_targets_mirror_legal_edges_table_for_every_actor_kind() {
        for from in ALL_STATES {
            for kind in ALL_KINDS {
                let expected: Vec<TrackLifecycle> = ALL_STATES
                    .into_iter()
                    .filter(|&to| to != from && legal_edges().contains(&(from, to, kind)))
                    .collect();
                assert_eq!(
                    allowed_targets(from, kind),
                    expected,
                    "from {from:?} as {kind:?}"
                );
            }
        }
    }

    #[test]
    fn illegal_edge_refusal_names_the_edge_and_the_legal_targets_in_wire_names() {
        let err = validate_transition(TrackLifecycle::Planning, TrackLifecycle::Done, &planner())
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "track lifecycle: planning → done is not allowed; \
             from planning the planner may write: dispatching, reviewing, failed \
             (the kernel advances dispatching itself when it claims a task)"
        );
        let err = validate_transition(TrackLifecycle::Done, TrackLifecycle::Reviewing, &planner())
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "track lifecycle: done → reviewing is not allowed; \
             from done the planner may write nothing"
        );
    }

    #[test]
    fn not_authorized_refusal_names_the_actor_and_its_legal_targets() {
        let err = validate_transition(
            TrackLifecycle::Planning,
            TrackLifecycle::Canceled,
            &planner(),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "track lifecycle: planning → canceled is not allowed for the planner; \
             from planning the planner may write: dispatching, reviewing, failed \
             (the kernel advances dispatching itself when it claims a task)"
        );
        let err = validate_transition(
            TrackLifecycle::Reviewing,
            TrackLifecycle::Canceled,
            &planner(),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "track lifecycle: reviewing → canceled is not allowed for the planner; \
             from reviewing the planner may write: working, done, failed \
             (the kernel advances working itself when it claims a task)"
        );
        let err = validate_transition(TrackLifecycle::Reviewing, TrackLifecycle::Done, &user())
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "track lifecycle: reviewing → done is not allowed for the user; \
             from reviewing the user may write: working, canceled"
        );
        let err = validate_transition(TrackLifecycle::Working, TrackLifecycle::Working, &worker())
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "track lifecycle: working → working is not allowed for the worker; \
             from working the worker may write nothing"
        );
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
