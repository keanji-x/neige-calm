mod common;

use common::{command, exit_run, objective};
use objective::definition::{Mode, Role, Usage};
use objective::engine::{Event, IgnoreReason, Intent, Notice, State, react};
use objective::ids::{CheckId, EvaluationId, ObjectiveKey, Revision};
use objective::ledger::Decision;
use objective::standard::{Approval, standard_hash};

fn usage(n: u64) -> Usage {
    Usage {
        turns: n as u32,
        context_tokens: n * 1000,
        elapsed_secs: n * 10,
    }
}

fn frozen_usage_replays(done: bool) {
    let definition = objective("stage", Mode::Converge, vec![command("goal", Role::Goal)]);
    let key = definition.key.clone();
    let mut events = vec![
        Event::Usage { usage: usage(2) },
        Event::DefinitionChanged {
            objective: definition.clone(),
        },
        Event::Usage { usage: usage(5) },
    ];
    if done {
        events.extend([
            Event::ApprovalGranted {
                objective: key.clone(),
                approval: Approval {
                    standard_hash: standard_hash(&definition),
                    approver: "user".into(),
                },
            },
            Event::Requested {
                objective: key.clone(),
                revision: Revision::new("r"),
            },
            Event::CheckFinished {
                objective: key.clone(),
                evaluation: EvaluationId::new(0),
                check: CheckId::new("goal"),
                revision: Revision::new("r"),
                sample: 0,
                output: exit_run(0),
            },
            Event::Settled {
                objective: key.clone(),
                evaluation: EvaluationId::new(0),
                decision: Decision::Done,
                note: "done".into(),
            },
        ]);
    } else {
        events.push(Event::Closed {
            objective: key.clone(),
        });
    }
    let fold = |log: &[Event]| log.iter().fold(State::default(), |s, e| react(&s, e).0);
    let frozen = fold(&events);
    assert_ne!(
        frozen.objectives[&key].status,
        objective::engine::Status::Open
    );
    assert_eq!(frozen.objectives[&key].used(&frozen.usage), usage(3));
    for n in [8, 13, 21] {
        events.push(Event::Usage { usage: usage(n) });
        // Closing Done must preserve its earlier frozen amount.
        events.push(Event::Closed {
            objective: key.clone(),
        });
        let state = fold(&events);
        assert_eq!(state.objectives[&key].used(&state.usage), usage(3));
        let log: Vec<Event> =
            serde_json::from_str(&serde_json::to_string(&events).unwrap()).unwrap();
        assert_eq!(fold(&log), state);
        let restored: State =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(restored.objectives[&key].used(&usage(100)), usage(3));
    }
}

#[test]
fn objective_closed_usage_freezes_and_replays() {
    frozen_usage_replays(false);
}

#[test]
fn objective_done_usage_freezes_and_replays() {
    frozen_usage_replays(true);
}

#[test]
fn objective_close_unknown_and_closed_are_ignored() {
    let key = ObjectiveKey::new("stage");
    let close = Event::Closed {
        objective: key.clone(),
    };
    let ignored = |reason| {
        vec![Intent::Notify {
            notice: Notice::EventIgnored { reason },
        }]
    };
    assert_eq!(
        react(&State::default(), &close).1,
        ignored(IgnoreReason::UnknownObjective)
    );
    let state = react(
        &State::default(),
        &Event::DefinitionChanged {
            objective: objective("stage", Mode::Converge, vec![command("goal", Role::Goal)]),
        },
    )
    .0;
    let closed = react(&state, &close).0;
    assert_eq!(
        react(&closed, &close),
        (closed, ignored(IgnoreReason::NotOpen))
    );
}
