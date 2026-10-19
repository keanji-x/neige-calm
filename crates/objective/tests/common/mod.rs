//! Builders shared by the integration tests.

#![allow(dead_code)]

use objective::check::{
    Aggregate, CheckConfig, CheckOutput, CommandOutput, CommandParams, Direction, EvaluatorOutput,
    EvaluatorParams, EvaluatorSample, Evidence, Findings, Judgement, MetricOutput, MetricParams,
    RubricItem, SampleOutput, Trend,
};
use objective::definition::{Budget, CheckSpec, CompactPolicy, Mode, ObjectiveSpec, Role};
use objective::ids::{CheckId, ObjectiveKey};
use objective::verdict::Results;

pub fn command(id: &str, role: Role) -> CheckSpec {
    CheckSpec {
        id: CheckId::new(id),
        role,
        config: CheckConfig::Command(CommandParams {
            command: format!("run {id}"),
        }),
    }
}

pub fn metric(id: &str, role: Role, direction: Direction) -> CheckSpec {
    CheckSpec {
        id: CheckId::new(id),
        role,
        config: CheckConfig::Metric(MetricParams {
            command: format!("measure {id}"),
            direction,
            tolerance: None,
        }),
    }
}

pub fn evaluator(id: &str, role: Role, samples: u32) -> CheckSpec {
    CheckSpec {
        id: CheckId::new(id),
        role,
        config: CheckConfig::Evaluator(EvaluatorParams {
            rubric: vec![RubricItem {
                id: "clarity".into(),
                criterion: "the change reads clearly".into(),
            }],
            samples,
            aggregate: Aggregate::Majority,
        }),
    }
}

pub fn objective(key: &str, mode: Mode, checks: Vec<CheckSpec>) -> ObjectiveSpec {
    ObjectiveSpec {
        key: ObjectiveKey::new(key),
        title: format!("objective {key}"),
        mode,
        checks,
        constraints: vec!["keep the public API".into()],
        budget: Budget {
            turns: 10,
            tokens: 50_000,
            wall_secs: 3_600,
        },
        compact: CompactPolicy::EverySettle,
    }
}

pub fn exit(code: i32) -> CheckOutput {
    CheckOutput::Command(CommandOutput { exit_code: code })
}

pub fn value(value: f64) -> CheckOutput {
    CheckOutput::Metric(MetricOutput { value })
}

fn vote(trend: Trend, blocking: u32) -> EvaluatorSample {
    EvaluatorSample {
        judgements: vec![Judgement {
            item: "clarity".into(),
            trend,
            evidence: vec![Evidence {
                path: "src/lib.rs".into(),
                line: 12,
            }],
        }],
        findings: Findings {
            blocking,
            major: 1,
            minor: 2,
        },
    }
}

/// The combined result of one evaluator sample.
pub fn judged(trend: Trend, blocking: u32) -> CheckOutput {
    CheckOutput::Evaluator(EvaluatorOutput {
        samples: vec![vote(trend, blocking)],
    })
}

/// What one command run reports.
pub fn exit_run(code: i32) -> SampleOutput {
    SampleOutput::Command(CommandOutput { exit_code: code })
}

/// What one metric run reports.
pub fn value_run(value: f64) -> SampleOutput {
    SampleOutput::Metric(MetricOutput { value })
}

/// What one evaluator run reports.
pub fn judged_run(trend: Trend, blocking: u32) -> SampleOutput {
    SampleOutput::Evaluator(vote(trend, blocking))
}

pub fn results(pairs: Vec<(&str, CheckOutput)>) -> Results {
    pairs
        .into_iter()
        .map(|(id, output)| (CheckId::new(id), output))
        .collect()
}
