//! Check kinds and their registry.
//!
//! A kind owns its parameters, its output and how a candidate output compares with a
//! baseline output. [`CheckConfig`], [`SampleOutput`] and [`CheckOutput`] are the registry:
//! the only place that dispatches on the kind, so no other module names a kind.

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

/// What one run of a check reports: exactly one sample, tagged like [`CheckConfig`]. An
/// evaluator run is one [`EvaluatorSample`], so one run can neither cast several votes nor
/// erase itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "output", rename_all = "snake_case")]
pub enum SampleOutput {
    Command(CommandOutput),
    Metric(MetricOutput),
    Evaluator(EvaluatorSample),
    Fact(FactOutput),
}

/// The result of one check in one evaluation, combined from its samples and tagged like
/// [`CheckConfig`].
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

    /// Whether `sample` was produced by this check's kind.
    pub fn accepts(&self, sample: &SampleOutput) -> bool {
        matches!(
            (self, sample),
            (Self::Command(_), SampleOutput::Command(_))
                | (Self::Metric(_), SampleOutput::Metric(_))
                | (Self::Evaluator(_), SampleOutput::Evaluator(_))
                | (Self::Fact(_), SampleOutput::Fact(_))
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

    /// Combines every sample of one evaluation, in sample order, into the check's result.
    /// Only evaluator checks take more than one sample; each counts once.
    pub fn combine(&self, samples: Vec<SampleOutput>) -> Result<CheckOutput, KindMismatch> {
        if samples.len() != self.samples() as usize {
            return Err(KindMismatch);
        }
        if let Self::Evaluator(_) = self {
            let runs = samples
                .into_iter()
                .map(|sample| match sample {
                    SampleOutput::Evaluator(run) => Ok(run),
                    _ => Err(KindMismatch),
                })
                .collect::<Result<_, _>>()?;
            return Ok(CheckOutput::Evaluator(EvaluatorOutput { samples: runs }));
        }
        let [sample] = <[SampleOutput; 1]>::try_from(samples).map_err(|_| KindMismatch)?;
        match (self, sample) {
            (Self::Command(_), SampleOutput::Command(output)) => Ok(CheckOutput::Command(output)),
            (Self::Metric(_), SampleOutput::Metric(output)) => Ok(CheckOutput::Metric(output)),
            (Self::Fact(_), SampleOutput::Fact(output)) => Ok(CheckOutput::Fact(output)),
            _ => Err(KindMismatch),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_kind() -> Vec<(CheckConfig, SampleOutput)> {
        vec![
            (
                CheckConfig::Command(CommandParams {
                    command: "cargo test".into(),
                }),
                SampleOutput::Command(CommandOutput { exit_code: 0 }),
            ),
            (
                CheckConfig::Metric(MetricParams {
                    command: "bench".into(),
                    direction: Direction::Lower,
                    tolerance: None,
                }),
                SampleOutput::Metric(MetricOutput { value: 1.0 }),
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
                SampleOutput::Evaluator(EvaluatorSample {
                    judgements: Vec::new(),
                    findings: Findings::default(),
                }),
            ),
            (
                CheckConfig::Fact(FactParams {
                    fact: Fact::PrMerged,
                    subject: "#1".into(),
                }),
                SampleOutput::Fact(FactOutput { holds: true }),
            ),
        ]
    }

    fn evaluator(samples: u32, aggregate: Aggregate) -> CheckConfig {
        CheckConfig::Evaluator(EvaluatorParams {
            rubric: vec![RubricItem {
                id: "a".into(),
                criterion: "c".into(),
            }],
            samples,
            aggregate,
        })
    }

    #[test]
    fn serialized_kind_tag_is_the_registered_name() {
        for (config, sample) in every_kind() {
            let config_json = serde_json::to_value(&config).unwrap();
            assert_eq!(config_json["kind"], config.kind_name());
            let sample_json = serde_json::to_value(&sample).unwrap();
            assert_eq!(sample_json["kind"], config.kind_name());
            let output = config.combine(vec![sample]).unwrap();
            let output_json = serde_json::to_value(&output).unwrap();
            assert_eq!(output_json["kind"], config.kind_name());
        }
    }

    #[test]
    fn outputs_of_another_kind_are_refused() {
        let kinds = every_kind();
        for (config, _) in &kinds {
            for (other, sample) in &kinds {
                let same = config.kind_name() == other.kind_name();
                assert_eq!(config.accepts(sample), same);
                assert_eq!(config.combine(vec![sample.clone()]).is_ok(), same);
                let output = other.combine(vec![sample.clone()]).unwrap();
                assert_eq!(config.compare(&output, &output).is_ok(), same);
            }
        }
    }

    #[test]
    fn combine_keeps_evaluator_samples_in_order() {
        let config = evaluator(2, Aggregate::Median);
        let sample = |minor| {
            SampleOutput::Evaluator(EvaluatorSample {
                judgements: Vec::new(),
                findings: Findings {
                    blocking: 0,
                    major: 0,
                    minor,
                },
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
    fn one_completion_counts_as_exactly_one_sample() {
        let vote = |trend| EvaluatorSample {
            judgements: vec![Judgement {
                item: "a".into(),
                trend,
                evidence: vec![Evidence {
                    path: "src/lib.rs".into(),
                    line: 1,
                }],
            }],
            findings: Findings::default(),
        };
        // A completion is one sample: one that carries several votes, or none, is not a
        // sample output at all, so it can neither outvote nor erase the other runs.
        let better = serde_json::to_value(vote(Trend::Better)).unwrap();
        for samples in [vec![better.clone(); 3], Vec::new()] {
            let completion = serde_json::json!({
                "kind": "evaluator",
                "output": { "samples": samples },
            });
            assert!(serde_json::from_value::<SampleOutput>(completion).is_err());
        }
        // Three completions are three votes: two runs that got worse outvote one that improved.
        let config = evaluator(3, Aggregate::Majority);
        let runs = [Trend::Better, Trend::Worse, Trend::Worse]
            .map(|trend| SampleOutput::Evaluator(vote(trend)))
            .to_vec();
        let combined = config.combine(runs).unwrap();
        let baseline = CheckOutput::Evaluator(EvaluatorOutput {
            samples: Vec::new(),
        });
        let comparison = config.compare(&baseline, &combined).unwrap();
        assert_eq!(comparison.trend, Trend::Worse);
    }

    #[test]
    fn combine_refuses_a_wrong_sample_count_or_kind() {
        let config = CheckConfig::Fact(FactParams {
            fact: Fact::TaskAccepted,
            subject: "t".into(),
        });
        let holds = SampleOutput::Fact(FactOutput { holds: true });
        assert_eq!(
            config.combine(vec![holds.clone()]),
            Ok(CheckOutput::Fact(FactOutput { holds: true }))
        );
        assert_eq!(
            config.combine(vec![holds.clone(), holds]),
            Err(KindMismatch)
        );
        let command = SampleOutput::Command(CommandOutput { exit_code: 0 });
        assert_eq!(config.combine(vec![command.clone()]), Err(KindMismatch));
        assert_eq!(
            evaluator(1, Aggregate::Majority).combine(vec![command]),
            Err(KindMismatch)
        );
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
