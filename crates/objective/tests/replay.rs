//! Replay determinism: the same event log always yields the same states and intents,
//! including across a JSON round trip of the log and a resume from a serialized snapshot.

mod common;

use common::{command, evaluator, exit_run, judged_run, metric, objective, value_run};
use objective::check::{Direction, SampleOutput, Trend};
use objective::command::CommandCall;
use objective::definition::{BudgetLimit, CompactPolicy, Mode, ObjectiveDefinition, Role, Usage};
use objective::engine::{Event, IgnoreReason, Intent, Notice, State, Status, react};
use objective::ids::{CheckId, EvaluationId, ObjectiveKey, Revision};
use objective::ledger::{Decision, SettleError};
use objective::standard::{Approval, standard_hash};
use objective::verdict::VerdictKind;

fn key(name: &str) -> ObjectiveKey {
    ObjectiveKey::new(name)
}

fn rev(name: &str) -> Revision {
    Revision::new(name)
}

fn speed() -> ObjectiveDefinition {
    objective(
        "speed",
        Mode::Loop { plateau: 2 },
        vec![
            command("build", Role::Invariant),
            metric("latency", Role::Ratchet, Direction::Lower),
            evaluator("review", Role::Ratchet, 2),
        ],
    )
}

fn speed_redefined() -> ObjectiveDefinition {
    let mut redefined = speed();
    redefined.constraints.push("no unsafe code".into());
    redefined
}

fn docs() -> ObjectiveDefinition {
    let mut docs = objective("docs", Mode::Converge, vec![command("docs", Role::Goal)]);
    docs.compact = CompactPolicy::OnClose;
    docs
}

fn approve(objective: &ObjectiveDefinition) -> Event {
    Event::ApprovalGranted {
        objective: objective.key.clone(),
        approval: Approval {
            standard_hash: standard_hash(objective),
            approver: "user:owner".into(),
        },
    }
}

fn finished(
    objective: &str,
    evaluation: u64,
    check: &str,
    revision: &str,
    sample: u32,
    output: SampleOutput,
) -> Event {
    Event::CheckFinished {
        objective: key(objective),
        evaluation: EvaluationId::new(evaluation),
        check: CheckId::new(check),
        revision: rev(revision),
        sample,
        output,
    }
}

/// All four samples of evaluation `evaluation` of `speed`.
fn speed_results(evaluation: u64, revision: &str, latency: f64) -> Vec<Event> {
    let review = |sample| {
        finished(
            "speed",
            evaluation,
            "review",
            revision,
            sample,
            judged_run(Trend::Same, 0),
        )
    };
    vec![
        finished("speed", evaluation, "build", revision, 0, exit_run(0)),
        finished(
            "speed",
            evaluation,
            "latency",
            revision,
            0,
            value_run(latency),
        ),
        review(0),
        review(1),
    ]
}

fn settled(objective: &str, evaluation: u64, decision: Decision, note: &str) -> Event {
    Event::Settled {
        objective: key(objective),
        evaluation: EvaluationId::new(evaluation),
        decision,
        note: note.into(),
    }
}

fn usage(context_tokens: u64) -> Event {
    Event::Usage {
        usage: Usage {
            turns: 3,
            context_tokens,
            elapsed_secs: 100,
        },
    }
}

fn scenario() -> Vec<Event> {
    let mut stale_docs = docs();
    stale_docs.constraints.clear();
    let mut log = vec![
        usage(0),
        Event::DefinitionChanged { objective: speed() },
        Event::DefinitionChanged { objective: docs() },
        Event::Requested {
            objective: key("speed"),
            revision: rev("r0"),
        },
        approve(&speed()),
        approve(&stale_docs),
        approve(&docs()),
        Event::RevisionDelivered {
            revision: rev("r0"),
        },
    ];
    log.extend(speed_results(0, "r0", 100.0));
    log.extend([
        finished("docs", 0, "docs", "r0", 0, exit_run(1)),
        finished("speed", 0, "build", "r9", 0, exit_run(0)),
        settled("speed", 0, Decision::Keep, "baseline"),
        settled("docs", 0, Decision::Keep, "baseline"),
        Event::HostIdle { queue_len: 1 },
        Event::HostIdle { queue_len: 0 },
        Event::Requested {
            objective: key("speed"),
            revision: rev("r1"),
        },
    ]);
    log.extend(speed_results(1, "r1", 120.0));
    log.extend([
        settled("speed", 1, Decision::Keep, "slower"),
        settled("speed", 1, Decision::Discard, "slower"),
        Event::Requested {
            objective: key("speed"),
            revision: rev("r2"),
        },
    ]);
    log.extend(speed_results(2, "r2", 100.0));
    log.extend([
        settled("speed", 2, Decision::Discard, "no gain"),
        usage(60_000),
        usage(70_000),
        Event::Requested {
            objective: key("docs"),
            revision: rev("r3"),
        },
        finished("docs", 1, "docs", "r3", 0, exit_run(0)),
        settled("docs", 1, Decision::Done, "documented"),
        Event::Closed {
            objective: key("docs"),
        },
        Event::HostIdle { queue_len: 0 },
        Event::DefinitionChanged {
            objective: speed_redefined(),
        },
        Event::Requested {
            objective: key("speed"),
            revision: rev("r4"),
        },
    ]);
    log
}

/// Folds `events` over `start`, recording every intermediate state and intent batch.
fn replay(start: State, events: &[Event]) -> Vec<(State, Vec<Intent>)> {
    let mut state = start;
    let mut steps = Vec::with_capacity(events.len());
    for event in events {
        let (next, intents) = react(&state, event);
        steps.push((next.clone(), intents));
        state = next;
    }
    steps
}

fn final_state(steps: &[(State, Vec<Intent>)]) -> &State {
    &steps.last().expect("the scenario has events").0
}

fn all_intents(steps: &[(State, Vec<Intent>)]) -> Vec<Intent> {
    steps
        .iter()
        .flat_map(|(_, intents)| intents.clone())
        .collect()
}

#[test]
fn the_same_log_replays_to_identical_states_and_intents() {
    let log = scenario();
    let first = replay(State::default(), &log);
    let second = replay(State::default(), &log);
    assert_eq!(first, second);
}

#[test]
fn a_json_round_trip_of_the_log_replays_identically() {
    let log = scenario();
    let encoded = serde_json::to_string(&log).unwrap();
    let decoded: Vec<Event> = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, log);
    let direct = replay(State::default(), &log);
    let decoded_run = replay(State::default(), &decoded);
    assert_eq!(direct, decoded_run);

    for (state, intents) in &direct {
        let state_back: State =
            serde_json::from_str(&serde_json::to_string(state).unwrap()).unwrap();
        assert_eq!(&state_back, state);
        let intents_back: Vec<Intent> =
            serde_json::from_str(&serde_json::to_string(intents).unwrap()).unwrap();
        assert_eq!(&intents_back, intents);
    }
}

#[test]
fn resuming_from_any_serialized_snapshot_matches_the_uninterrupted_run() {
    let log = scenario();
    let whole = replay(State::default(), &log);
    for split in 0..=log.len() {
        let prefix = replay(State::default(), &log[..split]);
        let snapshot = prefix
            .last()
            .map_or_else(State::default, |(s, _)| s.clone());
        let restored: State =
            serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();
        let suffix = replay(restored, &log[split..]);
        let mut resumed = prefix;
        resumed.extend(suffix);
        assert_eq!(resumed, whole, "resume after event {split} diverged");
    }
}

#[test]
fn the_scenario_notifies_the_planner_in_order() {
    let steps = replay(State::default(), &scenario());
    let notices: Vec<Notice> = all_intents(&steps)
        .into_iter()
        .filter_map(|intent| match intent {
            Intent::Notify { notice } => Some(notice),
            _ => None,
        })
        .collect();
    let required = |objective: &ObjectiveDefinition| Notice::ApprovalRequired {
        objective: objective.key.clone(),
        standard_hash: standard_hash(objective),
    };
    let ready = |objective: &str, evaluation, revision: &str, verdict| Notice::EvaluationReady {
        objective: key(objective),
        evaluation: EvaluationId::new(evaluation),
        revision: rev(revision),
        verdict,
    };
    let ignored = |reason| Notice::EventIgnored { reason };
    let exhausted = |objective: &str| Notice::BudgetExhausted {
        objective: key(objective),
        limit: BudgetLimit::Tokens,
    };
    assert_eq!(
        notices,
        vec![
            required(&speed()),
            required(&docs()),
            required(&speed()),
            ignored(IgnoreReason::StaleApproval),
            ready("speed", 0, "r0", VerdictKind::Keepable),
            ready("docs", 0, "r0", VerdictKind::Keepable),
            ignored(IgnoreReason::StaleResult),
            ready("speed", 1, "r1", VerdictKind::Regression),
            Notice::SettleRefused {
                objective: key("speed"),
                error: SettleError::RegressionRefused,
            },
            ready("speed", 2, "r2", VerdictKind::Discard),
            Notice::PlateauReached {
                objective: key("speed"),
            },
            exhausted("docs"),
            exhausted("speed"),
            ready("docs", 1, "r3", VerdictKind::Done),
            Notice::ObjectiveDone {
                objective: key("docs"),
            },
            Notice::ApprovalInvalidated {
                objective: key("speed"),
            },
            required(&speed_redefined()),
            required(&speed_redefined()),
        ]
    );
}

#[test]
fn the_scenario_runs_every_sample_and_compacts_when_idle() {
    let log = scenario();
    let steps = replay(State::default(), &log);
    let intents = all_intents(&steps);
    let runs: Vec<(String, u64, String, String, u32)> = intents
        .iter()
        .filter_map(|intent| match intent {
            Intent::RunCheck {
                objective,
                evaluation,
                check,
                revision,
                sample,
            } => Some((
                objective.to_string(),
                evaluation.sequence(),
                check.to_string(),
                revision.to_string(),
                *sample,
            )),
            _ => None,
        })
        .collect();
    let speed_runs = |evaluation, revision: &str| {
        [("build", 0), ("latency", 0), ("review", 0), ("review", 1)].map(|(check, sample)| {
            (
                "speed".into(),
                evaluation,
                check.into(),
                revision.into(),
                sample,
            )
        })
    };
    let mut expected: Vec<(String, u64, String, String, u32)> = Vec::new();
    // A delivered revision starts every open, approved objective in key order.
    expected.push(("docs".into(), 0, "docs".into(), "r0".into(), 0));
    expected.extend(speed_runs(0, "r0"));
    expected.extend(speed_runs(1, "r1"));
    expected.extend(speed_runs(2, "r2"));
    expected.push(("docs".into(), 1, "docs".into(), "r3".into(), 0));
    assert_eq!(runs, expected);

    let compactions: Vec<(String, Vec<String>)> = intents
        .iter()
        .filter_map(|intent| match intent {
            Intent::Execute { call } => {
                let CommandCall::Compact(compact) = call;
                let keys = compact
                    .brief
                    .objectives
                    .iter()
                    .map(|o| o.key.to_string())
                    .collect();
                Some((call.idempotency_key(), keys))
            }
            _ => None,
        })
        .collect();
    let both = vec!["docs".to_string(), "speed".to_string()];
    assert_eq!(
        compactions,
        vec![
            ("compact:0".to_string(), both.clone()),
            ("compact:1".to_string(), both)
        ]
    );
    // The idle host with a queued input deferred the first compaction by one event.
    let queued = log
        .iter()
        .position(|e| *e == Event::HostIdle { queue_len: 1 })
        .unwrap();
    let deferred = &steps[queued];
    assert!(deferred.1.is_empty() && deferred.0.pending_compact);
}

#[test]
fn the_scenario_leaves_ledgers_and_statuses_consistent() {
    let steps = replay(State::default(), &scenario());
    let state = final_state(&steps);
    let speed = &state.objectives[&key("speed")];
    assert_eq!(speed.status, Status::Open);
    assert!(!speed.is_approved());
    assert!(speed.evaluation.is_none());
    let baseline = speed.ledger.baseline.as_ref().unwrap();
    assert_eq!(baseline.revision, rev("r0"));
    let decisions: Vec<Decision> = speed.ledger.entries.iter().map(|e| e.decision).collect();
    assert_eq!(
        decisions,
        vec![Decision::Keep, Decision::Discard, Decision::Discard]
    );
    assert_eq!(speed.ledger.plateau, 2);

    let docs = &state.objectives[&key("docs")];
    assert!(matches!(docs.status, Status::Closed { .. }));
    assert_eq!(docs.ledger.baseline.as_ref().unwrap().revision, rev("r3"));
    assert_eq!(state.compactions, 2);
    assert!(!state.pending_compact);
}

#[test]
fn objective_host_serde_version_pins() {
    assert_eq!(objective::engine::EVENT_VERSION, 1);
    assert_eq!(objective::engine::STATE_VERSION, 2);
    let events = scenario();
    let steps = replay(State::default(), &events);
    let running = steps
        .iter()
        .map(|(state, _)| state)
        .find(|state| {
            state.objectives.values().any(|o| {
                matches!(
                    o.evaluation,
                    Some(objective::engine::Evaluation::Running { .. })
                )
            })
        })
        .unwrap();
    let ready = steps
        .iter()
        .map(|(state, _)| state)
        .find(|state| {
            state.objectives.values().any(|o| {
                matches!(
                    o.evaluation,
                    Some(objective::engine::Evaluation::Ready { .. })
                )
            })
        })
        .unwrap();
    let actual = serde_json::json!({"events": events, "default":State::default(), "running":running, "ready":ready, "final":final_state(&steps)});
    if std::env::var_os("REGEN_OBJECTIVE_STATE_GOLDEN").is_some() {
        std::fs::write(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/goldens/state-final-v2.json"
            ),
            format!(
                "{}\n",
                serde_json::to_string_pretty(&actual["final"]).unwrap()
            ),
        )
        .unwrap();
        panic!(
            "state-final-v2.json regenerated; inspect and rerun without REGEN_OBJECTIVE_STATE_GOLDEN"
        );
    }
    let events: serde_json::Value =
        serde_json::from_str(include_str!("goldens/events-v1.json")).unwrap();
    let active: serde_json::Value =
        serde_json::from_str(include_str!("goldens/states-active-v1.json")).unwrap();
    let final_state: serde_json::Value =
        serde_json::from_str(include_str!("goldens/state-final-v2.json")).unwrap();
    let expected = serde_json::json!({"events":events,"default":active["default"],"running":active["running"],"ready":active["ready"],"final":final_state});
    assert_eq!(
        actual, expected,
        "persisted objective contracts changed; review version bumps"
    );
}
