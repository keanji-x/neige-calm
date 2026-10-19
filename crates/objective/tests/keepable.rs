//! The keepable() rules, through the public API.

mod common;

use common::{command, evaluator, exit, judged, metric, objective, results, value};
use objective::check::{Direction, Trend};
use objective::definition::{Mode, Role};
use objective::ids::CheckId;
use objective::verdict::{Finding, Results, VerdictKind, keepable};

fn regressing(verdict: &objective::Verdict) -> Vec<&str> {
    verdict
        .reasons
        .iter()
        .filter(|r| r.regresses)
        .map(|r| r.check.as_str())
        .collect()
}

#[test]
fn a_failing_invariant_is_a_regression() {
    let o = objective(
        "o",
        Mode::Converge,
        vec![
            command("build", Role::Invariant),
            command("tests", Role::Goal),
        ],
    );
    let verdict = keepable(
        &o,
        &results(vec![("build", exit(0)), ("tests", exit(0))]),
        &results(vec![("build", exit(1)), ("tests", exit(0))]),
    );
    assert_eq!(verdict.kind, VerdictKind::Regression);
    assert!(verdict.refuses_keep());
    assert_eq!(regressing(&verdict), vec!["build"]);
}

#[test]
fn an_invariant_must_pass_even_without_a_baseline() {
    let o = objective(
        "o",
        Mode::Converge,
        vec![command("build", Role::Invariant), command("t", Role::Goal)],
    );
    let verdict = keepable(
        &o,
        &Results::new(),
        &results(vec![("build", exit(2)), ("t", exit(0))]),
    );
    assert_eq!(verdict.kind, VerdictKind::Regression);
}

#[test]
fn a_missing_invariant_or_ratchet_result_is_a_regression() {
    let o = objective(
        "o",
        Mode::Converge,
        vec![
            command("build", Role::Invariant),
            metric("size", Role::Ratchet, Direction::Lower),
            command("tests", Role::Goal),
        ],
    );
    let verdict = keepable(&o, &Results::new(), &results(vec![("tests", exit(0))]));
    assert_eq!(verdict.kind, VerdictKind::Regression);
    assert_eq!(regressing(&verdict), vec!["build", "size"]);
    assert_eq!(verdict.reasons[0].finding, Finding::Missing);
}

#[test]
fn a_ratchet_may_not_regress_against_the_baseline() {
    let o = objective(
        "o",
        Mode::Converge,
        vec![
            metric("size", Role::Ratchet, Direction::Lower),
            command("tests", Role::Goal),
        ],
    );
    let baseline = results(vec![("size", value(100.0)), ("tests", exit(1))]);
    let worse = keepable(
        &o,
        &baseline,
        &results(vec![("size", value(101.0)), ("tests", exit(1))]),
    );
    assert_eq!(worse.kind, VerdictKind::Regression);
    assert_eq!(regressing(&worse), vec!["size"]);

    let same = keepable(
        &o,
        &baseline,
        &results(vec![("size", value(100.0)), ("tests", exit(1))]),
    );
    assert_eq!(same.kind, VerdictKind::Keepable);
}

#[test]
fn a_ratchet_without_a_baseline_result_cannot_regress() {
    let o = objective(
        "o",
        Mode::Converge,
        vec![command("lint", Role::Ratchet), command("t", Role::Goal)],
    );
    let verdict = keepable(
        &o,
        &Results::new(),
        &results(vec![("lint", exit(1)), ("t", exit(1))]),
    );
    assert_eq!(verdict.kind, VerdictKind::Keepable);
    assert!(matches!(verdict.reasons[0].finding, Finding::NoBaseline(_)));
}

#[test]
fn a_result_of_the_wrong_kind_regresses_a_ratchet() {
    let o = objective(
        "o",
        Mode::Converge,
        vec![command("lint", Role::Ratchet), command("t", Role::Goal)],
    );
    let verdict = keepable(
        &o,
        &results(vec![("lint", exit(0))]),
        &results(vec![("lint", value(1.0)), ("t", exit(0))]),
    );
    assert_eq!(verdict.kind, VerdictKind::Regression);
    assert_eq!(verdict.reasons[0].finding, Finding::KindMismatch);
}

#[test]
fn more_blocking_evaluator_findings_regress_a_ratchet() {
    let o = objective(
        "o",
        Mode::Converge,
        vec![
            evaluator("review", Role::Ratchet, 1),
            command("t", Role::Goal),
        ],
    );
    let baseline = results(vec![("review", judged(Trend::Same, 0))]);
    let verdict = keepable(
        &o,
        &baseline,
        &results(vec![("review", judged(Trend::Same, 1)), ("t", exit(0))]),
    );
    assert_eq!(verdict.kind, VerdictKind::Regression);
}

#[test]
fn converge_is_done_when_every_goal_passes() {
    let o = objective(
        "o",
        Mode::Converge,
        vec![
            command("build", Role::Invariant),
            command("a", Role::Goal),
            command("b", Role::Goal),
        ],
    );
    let base = Results::new();
    let one = keepable(
        &o,
        &base,
        &results(vec![("build", exit(0)), ("a", exit(0)), ("b", exit(1))]),
    );
    assert_eq!(one.kind, VerdictKind::Keepable);
    let open: Vec<&CheckId> = one.open_blockers().map(|r| &r.check).collect();
    assert_eq!(open, vec![&CheckId::new("b")]);

    let both = keepable(
        &o,
        &base,
        &results(vec![("build", exit(0)), ("a", exit(0)), ("b", exit(0))]),
    );
    assert_eq!(both.kind, VerdictKind::Done);
    assert_eq!(both.open_blockers().count(), 0);
}

#[test]
fn a_regression_outranks_done() {
    let o = objective(
        "o",
        Mode::Converge,
        vec![command("build", Role::Invariant), command("a", Role::Goal)],
    );
    let verdict = keepable(
        &o,
        &Results::new(),
        &results(vec![("build", exit(1)), ("a", exit(0))]),
    );
    assert_eq!(verdict.kind, VerdictKind::Regression);
}

#[test]
fn a_goal_that_gets_worse_does_not_refuse_keep() {
    let o = objective(
        "o",
        Mode::Converge,
        vec![command("a", Role::Goal), command("b", Role::Goal)],
    );
    let verdict = keepable(
        &o,
        &results(vec![("a", exit(0)), ("b", exit(1))]),
        &results(vec![("a", exit(1)), ("b", exit(1))]),
    );
    assert_eq!(verdict.kind, VerdictKind::Keepable);
    assert!(!verdict.refuses_keep());
}

#[test]
fn loop_mode_discards_a_candidate_that_improves_nothing() {
    let o = objective(
        "o",
        Mode::Loop { plateau: 3 },
        vec![metric("latency", Role::Ratchet, Direction::Lower)],
    );
    let baseline = results(vec![("latency", value(50.0))]);
    let flat = keepable(&o, &baseline, &results(vec![("latency", value(50.0))]));
    assert_eq!(flat.kind, VerdictKind::Discard);
    assert!(!flat.refuses_keep() && !flat.improved());

    let faster = keepable(&o, &baseline, &results(vec![("latency", value(40.0))]));
    assert_eq!(faster.kind, VerdictKind::Keepable);
    assert!(faster.improved());

    let first = keepable(
        &o,
        &Results::new(),
        &results(vec![("latency", value(50.0))]),
    );
    assert_eq!(
        first.kind,
        VerdictKind::Keepable,
        "the first baseline is not compared"
    );
}

#[test]
fn loop_mode_never_reports_done() {
    let o = objective(
        "o",
        Mode::Loop { plateau: 3 },
        vec![
            metric("latency", Role::Ratchet, Direction::Lower),
            command("tests", Role::Goal),
        ],
    );
    let verdict = keepable(
        &o,
        &results(vec![("latency", value(50.0)), ("tests", exit(1))]),
        &results(vec![("latency", value(45.0)), ("tests", exit(0))]),
    );
    assert_eq!(verdict.kind, VerdictKind::Keepable);
}

#[test]
fn reasons_follow_definition_order_with_roles() {
    let o = objective(
        "o",
        Mode::Converge,
        vec![command("z", Role::Goal), command("a", Role::Invariant)],
    );
    let verdict = keepable(
        &o,
        &Results::new(),
        &results(vec![("z", exit(0)), ("a", exit(0))]),
    );
    let order: Vec<(&str, Role)> = verdict
        .reasons
        .iter()
        .map(|r| (r.check.as_str(), r.role))
        .collect();
    assert_eq!(order, vec![("z", Role::Goal), ("a", Role::Invariant)]);
}
