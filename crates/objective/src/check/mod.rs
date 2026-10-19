//! Check kinds and their registry.
//!
//! A kind owns its parameters, its output and how a candidate output compares with a
//! baseline output. [`CheckConfig`] and [`CheckOutput`] are the registry: the only place
//! that dispatches on the kind, so no other module names a kind.

mod command;
mod evaluator;
mod fact;
mod metric;

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::diagnostic::Problem;

pub use command::{CommandCheck, CommandOutput, CommandParams};
pub use evaluator::{
    Aggregate, EvaluatorCheck, EvaluatorOutput, EvaluatorParams, EvaluatorSample, Evidence,
    Findings, Judgement, RubricItem,
};
pub use fact::{Fact, FactCheck, FactOutput, FactParams};
pub use metric::{
    Direction, MetricCheck, MetricOutput, MetricParams, MetricParseError, parse_metric_output,
};

/// One kind of check.
pub trait CheckKind {
    /// The registry name, also the serialized `kind` tag.
    const NAME: &'static str;
    type Params;
    type Output;

    fn validate(params: &Self::Params) -> Vec<Problem>;

    /// Compares a candidate output with the baseline output of the same check.
    fn compare(
        params: &Self::Params,
        baseline: &Self::Output,
        candidate: &Self::Output,
    ) -> Comparison;
}

/// Direction of a candidate relative to its baseline. Ordered worst first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trend {
    Worse,
    Same,
    Better,
}

impl Trend {
    /// The trend of a pass/fail outcome: fail to pass improves, pass to fail regresses.
    pub fn between(baseline_passed: bool, candidate_passed: bool) -> Self {
        match (baseline_passed, candidate_passed) {
            (false, true) => Self::Better,
            (true, false) => Self::Worse,
            _ => Self::Same,
        }
    }
}

impl From<Ordering> for Trend {
    /// `Greater` means the candidate is ahead of its baseline.
    fn from(ordering: Ordering) -> Self {
        match ordering {
            Ordering::Less => Self::Worse,
            Ordering::Equal => Self::Same,
            Ordering::Greater => Self::Better,
        }
    }
}

/// The result of comparing one candidate output with its baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comparison {
    pub trend: Trend,
    /// Whether the candidate passes on its own terms.
    pub passed: bool,
}

/// An output does not belong to the kind of the check it was reported for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KindMismatch;

/// The kind-specific configuration of one check: the kind registry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "params", rename_all = "snake_case")]
pub enum CheckConfig {
    Command(CommandParams),
    Metric(MetricParams),
    Evaluator(EvaluatorParams),
    Fact(FactParams),
}

/// The output of one check run, tagged like [`CheckConfig`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "output", rename_all = "snake_case")]
pub enum CheckOutput {
    Command(CommandOutput),
    Metric(MetricOutput),
    Evaluator(EvaluatorOutput),
    Fact(FactOutput),
}

impl CheckConfig {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Command(_) => CommandCheck::NAME,
            Self::Metric(_) => MetricCheck::NAME,
            Self::Evaluator(_) => EvaluatorCheck::NAME,
            Self::Fact(_) => FactCheck::NAME,
        }
    }

    /// How many independent runs one evaluation of this check takes.
    pub fn samples(&self) -> u32 {
        match self {
            Self::Evaluator(params) => params.samples,
            Self::Command(_) | Self::Metric(_) | Self::Fact(_) => 1,
        }
    }

    pub fn validate(&self) -> Vec<Problem> {
        match self {
            Self::Command(params) => CommandCheck::validate(params),
            Self::Metric(params) => MetricCheck::validate(params),
            Self::Evaluator(params) => EvaluatorCheck::validate(params),
            Self::Fact(params) => FactCheck::validate(params),
        }
    }

    /// Whether `output` was produced by this check's kind.
    pub fn accepts(&self, output: &CheckOutput) -> bool {
        matches!(
            (self, output),
            (Self::Command(_), CheckOutput::Command(_))
                | (Self::Metric(_), CheckOutput::Metric(_))
                | (Self::Evaluator(_), CheckOutput::Evaluator(_))
                | (Self::Fact(_), CheckOutput::Fact(_))
        )
    }

    pub fn compare(
        &self,
        baseline: &CheckOutput,
        candidate: &CheckOutput,
    ) -> Result<Comparison, KindMismatch> {
        match (self, baseline, candidate) {
            (Self::Command(p), CheckOutput::Command(b), CheckOutput::Command(c)) => {
                Ok(CommandCheck::compare(p, b, c))
            }
            (Self::Metric(p), CheckOutput::Metric(b), CheckOutput::Metric(c)) => {
                Ok(MetricCheck::compare(p, b, c))
            }
            (Self::Evaluator(p), CheckOutput::Evaluator(b), CheckOutput::Evaluator(c)) => {
                Ok(EvaluatorCheck::compare(p, b, c))
            }
            (Self::Fact(p), CheckOutput::Fact(b), CheckOutput::Fact(c)) => {
                Ok(FactCheck::compare(p, b, c))
            }
            _ => Err(KindMismatch),
        }
    }

    /// Combines the outputs of every sample of one evaluation, in sample order, into the
    /// check's result. Only evaluator checks take more than one sample.
    pub fn combine(&self, samples: Vec<CheckOutput>) -> Result<CheckOutput, KindMismatch> {
        if samples.len() != self.samples() as usize || !samples.iter().all(|s| self.accepts(s)) {
            return Err(KindMismatch);
        }
        if !matches!(self, Self::Evaluator(_)) {
            return samples.into_iter().next().ok_or(KindMismatch);
        }
        let mut combined = Vec::new();
        for sample in samples {
            if let CheckOutput::Evaluator(output) = sample {
                combined.extend(output.samples);
            }
        }
        Ok(CheckOutput::Evaluator(EvaluatorOutput {
            samples: combined,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_kind() -> Vec<(CheckConfig, CheckOutput)> {
        vec![
            (
                CheckConfig::Command(CommandParams {
                    command: "cargo test".into(),
                }),
                CheckOutput::Command(CommandOutput { exit_code: 0 }),
            ),
            (
                CheckConfig::Metric(MetricParams {
                    command: "bench".into(),
                    direction: Direction::Lower,
                    tolerance: None,
                }),
                CheckOutput::Metric(MetricOutput { value: 1.0 }),
            ),
            (
                CheckConfig::Evaluator(EvaluatorParams {
                    rubric: vec![RubricItem {
                        id: "clarity".into(),
                        criterion: "reads clearly".into(),
                    }],
                    samples: 1,
                    aggregate: Aggregate::Majority,
                }),
                CheckOutput::Evaluator(EvaluatorOutput {
                    samples: Vec::new(),
                }),
            ),
            (
                CheckConfig::Fact(FactParams {
                    fact: Fact::PrMerged,
                    subject: "#1".into(),
                }),
                CheckOutput::Fact(FactOutput { holds: true }),
            ),
        ]
    }

    #[test]
    fn serialized_kind_tag_is_the_registered_name() {
        for (config, output) in every_kind() {
            let config_json = serde_json::to_value(&config).unwrap();
            assert_eq!(config_json["kind"], config.kind_name());
            let output_json = serde_json::to_value(&output).unwrap();
            assert_eq!(output_json["kind"], config.kind_name());
        }
    }

    #[test]
    fn outputs_of_another_kind_are_refused() {
        let kinds = every_kind();
        for (config, _) in &kinds {
            for (other, output) in &kinds {
                let same = config.kind_name() == other.kind_name();
                assert_eq!(config.accepts(output), same);
                assert_eq!(config.compare(output, output).is_ok(), same);
            }
        }
    }

    #[test]
    fn combine_concatenates_evaluator_samples_in_order() {
        let config = CheckConfig::Evaluator(EvaluatorParams {
            rubric: vec![RubricItem {
                id: "a".into(),
                criterion: "c".into(),
            }],
            samples: 2,
            aggregate: Aggregate::Median,
        });
        let sample = |minor| {
            CheckOutput::Evaluator(EvaluatorOutput {
                samples: vec![EvaluatorSample {
                    judgements: Vec::new(),
                    findings: Findings {
                        blocking: 0,
                        major: 0,
                        minor,
                    },
                }],
            })
        };
        let CheckOutput::Evaluator(combined) = config.combine(vec![sample(1), sample(2)]).unwrap()
        else {
            panic!("evaluator output expected");
        };
        let minors: Vec<u32> = combined.samples.iter().map(|s| s.findings.minor).collect();
        assert_eq!(minors, vec![1, 2]);
        assert_eq!(config.combine(vec![sample(1)]), Err(KindMismatch));
    }

    #[test]
    fn combine_refuses_a_wrong_sample_count_or_kind() {
        let config = CheckConfig::Fact(FactParams {
            fact: Fact::TaskAccepted,
            subject: "t".into(),
        });
        let holds = CheckOutput::Fact(FactOutput { holds: true });
        assert_eq!(config.combine(vec![holds.clone()]), Ok(holds.clone()));
        assert_eq!(
            config.combine(vec![holds.clone(), holds]),
            Err(KindMismatch)
        );
        let command = CheckOutput::Command(CommandOutput { exit_code: 0 });
        assert_eq!(config.combine(vec![command]), Err(KindMismatch));
    }

    #[test]
    fn trend_between_pass_states() {
        assert_eq!(Trend::between(false, true), Trend::Better);
        assert_eq!(Trend::between(true, false), Trend::Worse);
        assert_eq!(Trend::between(true, true), Trend::Same);
        assert_eq!(Trend::between(false, false), Trend::Same);
        assert!(Trend::Worse < Trend::Same && Trend::Same < Trend::Better);
    }
}
