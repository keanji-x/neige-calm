//! Evaluations stay tied to the standard they were started under: results of a superseded
//! evaluation are refused, and a redefined check is not judged against results its old
//! definition produced.

mod common;

use common::{command, exit_run, metric, objective, value_run};
use objective::check::{CheckConfig, CommandParams, Direction, SampleOutput};
use objective::definition::{Mode, ObjectiveSpec, Role};
use objective::engine::{Evaluation, Event, IgnoreReason, Intent, Notice, State, react};
use objective::ids::{CheckId, EvaluationId, Revision};
use objective::ledger::Decision;
use objective::standard::{Approval, standard_hash};
use objective::verdict::{Finding, VerdictKind};

fn step(state: &mut State, event: Event) -> Vec<Intent> {
    let (next, intents) = react(state, &event);
    *state = next;
    intents
}

fn define_and_approve(state: &mut State, objective: &ObjectiveSpec) {
    step(
        state,
        Event::SpecChanged {
            objective: objective.clone(),
        },
    );
    step(
        state,
        Event::ApprovalGranted {
            objective: objective.key.clone(),
            approval: Approval {
                standard_hash: standard_hash(objective),
                approver: "user:owner".into(),
            },
        },
    );
}

fn request(state: &mut State, objective: &ObjectiveSpec, revision: &str) -> Vec<Intent> {
    step(
        state,
        Event::Requested {
            objective: objective.key.clone(),
            revision: Revision::new(revision),
        },
    )
}

/// The evaluation the `RunCheck` intents in `intents` belong to.
fn requested(intents: &[Intent]) -> EvaluationId {
    let ids: Vec<EvaluationId> = intents
        .iter()
        .filter_map(|intent| match intent {
            Intent::RunCheck { evaluation, .. } => Some(*evaluation),
            _ => None,
        })
        .collect();
    assert!(!ids.is_empty() && ids.iter().all(|id| *id == ids[0]));
    ids[0]
}

fn finish(
    state: &mut State,
    objective: &ObjectiveSpec,
    evaluation: EvaluationId,
    check: &str,
    revision: &str,
    output: SampleOutput,
) -> Vec<Intent> {
    step(
        state,
        Event::CheckFinished {
            objective: objective.key.clone(),
            evaluation,
            check: CheckId::new(check),
            revision: Revision::new(revision),
            sample: 0,
            output,
        },
    )
}

fn settle(
    state: &mut State,
    objective: &ObjectiveSpec,
    evaluation: EvaluationId,
    decision: Decision,
) -> Vec<Intent> {
    step(
        state,
        Event::Settled {
            objective: objective.key.clone(),
            evaluation,
            decision,
            note: String::new(),
        },
    )
}

fn notices(intents: &[Intent]) -> Vec<Notice> {
    intents
        .iter()
        .filter_map(|intent| match intent {
            Intent::Notify { notice } => Some(notice.clone()),
            _ => None,
        })
        .collect()
}

fn with_command(objective: &ObjectiveSpec, check: usize, line: &str) -> ObjectiveSpec {
    let mut changed = objective.clone();
    changed.checks[check].config = CheckConfig::Command(CommandParams {
        command: line.into(),
    });
    changed
}

#[test]
fn a_result_of_a_superseded_evaluation_is_refused() {
    let passing = with_command(
        &objective("green", Mode::Converge, vec![command("tests", Role::Goal)]),
        0,
        "true",
    );
    let failing = with_command(&passing, 0, "false");
    let mut state = State::default();
    define_and_approve(&mut state, &passing);
    let superseded = requested(&request(&mut state, &passing, "r"));
    define_and_approve(&mut state, &failing);
    let running = requested(&request(&mut state, &failing, "r"));
    assert_ne!(running, superseded);

    // The host delivers the result of the first run, made under `true`.
    let intents = finish(&mut state, &failing, superseded, "tests", "r", exit_run(0));
    assert_eq!(
        notices(&intents),
        vec![Notice::EventIgnored {
            reason: IgnoreReason::StaleResult
        }]
    );
    let objective = &state.objectives[&failing.key];
    assert!(matches!(
        objective.evaluation,
        Some(Evaluation::Running { .. })
    ));
    assert!(objective.latest_verdict.is_none());

    // The run of the current evaluation decides, and `false` does not finish the objective.
    let ready = finish(&mut state, &failing, running, "tests", "r", exit_run(1));
    assert_eq!(
        notices(&ready),
        vec![Notice::EvaluationReady {
            objective: failing.key.clone(),
            evaluation: running,
            revision: Revision::new("r"),
            verdict: VerdictKind::Keepable,
        }]
    );
}

#[test]
fn a_settle_of_a_superseded_evaluation_is_refused() {
    let green = objective("green", Mode::Converge, vec![command("tests", Role::Goal)]);
    let stricter = with_command(&green, 0, "run all tests");
    let mut state = State::default();
    define_and_approve(&mut state, &green);
    let superseded = requested(&request(&mut state, &green, "r"));
    finish(&mut state, &green, superseded, "tests", "r", exit_run(0));
    define_and_approve(&mut state, &stricter);
    let running = requested(&request(&mut state, &stricter, "r"));
    finish(&mut state, &stricter, running, "tests", "r", exit_run(0));

    // The planner settles the evaluation it was told about, which no longer exists.
    let refused = settle(&mut state, &stricter, superseded, Decision::Done);
    assert_eq!(
        notices(&refused),
        vec![Notice::EventIgnored {
            reason: IgnoreReason::NothingToSettle
        }]
    );
    assert!(state.objectives[&stricter.key].ledger.entries.is_empty());
    assert_eq!(
        notices(&settle(&mut state, &stricter, running, Decision::Done)),
        vec![Notice::ObjectiveDone {
            objective: stricter.key.clone()
        }]
    );
    assert_eq!(state.objectives[&stricter.key].ledger.entries.len(), 1);
}

#[test]
fn a_check_redefined_as_another_kind_is_judged_on_its_own() {
    let looping = objective(
        "speed",
        Mode::Loop { plateau: 3 },
        vec![command("size", Role::Ratchet)],
    );
    let mut state = State::default();
    define_and_approve(&mut state, &looping);
    let first = requested(&request(&mut state, &looping, "r0"));
    finish(&mut state, &looping, first, "size", "r0", exit_run(0));
    settle(&mut state, &looping, first, Decision::Keep);

    let mut measured = looping.clone();
    measured.checks[0] = metric("size", Role::Ratchet, Direction::Lower);
    define_and_approve(&mut state, &measured);
    let second = requested(&request(&mut state, &measured, "r1"));
    let ready = finish(&mut state, &measured, second, "size", "r1", value_run(10.0));
    assert_eq!(
        notices(&ready),
        vec![Notice::EvaluationReady {
            objective: measured.key.clone(),
            evaluation: second,
            revision: Revision::new("r1"),
            verdict: VerdictKind::Keepable,
        }]
    );
    assert!(notices(&settle(&mut state, &measured, second, Decision::Keep)).is_empty());
    let baseline = state.objectives[&measured.key].ledger.baseline.as_ref();
    assert_eq!(baseline.unwrap().revision, Revision::new("r1"));
}

#[test]
fn a_standard_change_keeps_only_baseline_results_of_unchanged_checks() {
    let looping = objective(
        "speed",
        Mode::Loop { plateau: 3 },
        vec![
            metric("latency", Role::Ratchet, Direction::Lower),
            command("lint", Role::Ratchet),
        ],
    );
    let mut state = State::default();
    define_and_approve(&mut state, &looping);
    let first = requested(&request(&mut state, &looping, "r0"));
    finish(
        &mut state,
        &looping,
        first,
        "latency",
        "r0",
        value_run(100.0),
    );
    finish(&mut state, &looping, first, "lint", "r0", exit_run(0));
    settle(&mut state, &looping, first, Decision::Keep);

    let relinted = with_command(&looping, 1, "run stricter lint");
    define_and_approve(&mut state, &relinted);
    let second = requested(&request(&mut state, &relinted, "r1"));
    finish(
        &mut state,
        &relinted,
        second,
        "latency",
        "r1",
        value_run(120.0),
    );
    finish(&mut state, &relinted, second, "lint", "r1", exit_run(1));
    let verdict = state.objectives[&relinted.key]
        .latest_verdict
        .clone()
        .unwrap();
    // The unchanged latency check still compares with its baseline and regresses; the
    // redefined lint command has no baseline result of its own yet.
    assert_eq!(verdict.kind, VerdictKind::Regression);
    assert!(matches!(verdict.reasons[0].finding, Finding::Compared(_)));
    assert!(verdict.reasons[0].regresses);
    assert!(matches!(verdict.reasons[1].finding, Finding::NoBaseline(_)));
    assert!(!verdict.reasons[1].regresses);
}
