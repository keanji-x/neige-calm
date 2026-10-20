//! `evaluator` checks: a read-only agent judges a candidate against its baseline, item by
//! rubric item, and counts findings by severity. Several samples are aggregated.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{CheckKind, Comparison, Trend};
use crate::diagnostic::Problem;

pub struct EvaluatorCheck;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RubricItem {
    pub id: String,
    pub criterion: String,
}

/// How the samples of one evaluation are reduced to one trend per rubric item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aggregate {
    /// The trend more samples chose than any other; a tie is `Same`.
    Majority,
    /// The lower median trend, ordered worse to better.
    Median,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluatorParams {
    pub rubric: Vec<RubricItem>,
    #[serde(default = "one_sample")]
    pub samples: u32,
    pub aggregate: Aggregate,
}

fn one_sample() -> u32 {
    1
}

/// A source location an evaluator cites for a judgement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub path: String,
    #[serde(deserialize_with = "positive_line")]
    pub line: u32,
}

fn positive_line<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let line = u32::deserialize(deserializer)?;
    if line == 0 {
        return Err(serde::de::Error::custom("line must be at least 1"));
    }
    Ok(line)
}

/// One rubric item judged against the baseline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Judgement {
    pub item: String,
    pub trend: Trend,
    pub evidence: Vec<Evidence>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Findings {
    pub blocking: u32,
    pub major: u32,
    pub minor: u32,
}

/// What one evaluator run returned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluatorSample {
    pub judgements: Vec<Judgement>,
    pub findings: Findings,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluatorOutput {
    pub samples: Vec<EvaluatorSample>,
}

impl EvaluatorSample {
    /// The sample's trend for `item`. A judgement that claims a change without citing
    /// evidence, or a missing judgement, counts as `Same`.
    fn trend_for(&self, item: &str) -> Trend {
        self.judgements
            .iter()
            .find(|j| j.item == item)
            .filter(|j| j.trend == Trend::Same || !j.evidence.is_empty())
            .map_or(Trend::Same, |j| j.trend)
    }
}

impl EvaluatorOutput {
    /// The per-severity lower median of the samples' findings; zero without samples.
    pub fn findings(&self) -> Findings {
        let median_of = |severity: fn(&Findings) -> u32| {
            lower_median(self.samples.iter().map(|s| severity(&s.findings)).collect()).unwrap_or(0)
        };
        Findings {
            blocking: median_of(|f| f.blocking),
            major: median_of(|f| f.major),
            minor: median_of(|f| f.minor),
        }
    }

    fn trend_for(&self, item: &str, aggregate: Aggregate) -> Trend {
        let trends: Vec<Trend> = self.samples.iter().map(|s| s.trend_for(item)).collect();
        match aggregate {
            Aggregate::Majority => majority(&trends),
            Aggregate::Median => lower_median(trends).unwrap_or(Trend::Same),
        }
    }
}

fn lower_median<T: Ord + Copy>(mut values: Vec<T>) -> Option<T> {
    values.sort_unstable();
    values.get(values.len().saturating_sub(1) / 2).copied()
}

fn majority(trends: &[Trend]) -> Trend {
    let count = |t: Trend| trends.iter().filter(|&&x| x == t).count();
    let (worse, same, better) = (
        count(Trend::Worse),
        count(Trend::Same),
        count(Trend::Better),
    );
    if better > worse && better > same {
        Trend::Better
    } else if worse > better && worse > same {
        Trend::Worse
    } else {
        Trend::Same
    }
}

impl CheckKind for EvaluatorCheck {
    const NAME: &'static str = "evaluator";
    type Params = EvaluatorParams;
    type Output = EvaluatorOutput;

    fn validate(params: &EvaluatorParams) -> Vec<Problem> {
        let mut problems = Vec::new();
        if params.samples == 0 {
            problems.push(Problem::ZeroSamples);
        }
        if params.rubric.is_empty() {
            problems.push(Problem::EmptyRubric);
        }
        let mut seen = BTreeSet::new();
        for item in &params.rubric {
            if item.id.trim().is_empty() || item.criterion.trim().is_empty() {
                problems.push(Problem::EmptyRubricItem);
            }
            if !seen.insert(item.id.as_str()) {
                problems.push(Problem::DuplicateRubricItem {
                    item: item.id.clone(),
                });
            }
        }
        problems
    }

    /// The candidate's judgements are pairwise against the baseline. Any rubric item that
    /// aggregates to `Worse`, or more blocking findings than the baseline, regresses; otherwise
    /// an improved item or fewer blocking findings improves. It passes without blocking findings.
    fn compare(
        params: &EvaluatorParams,
        baseline: &EvaluatorOutput,
        candidate: &EvaluatorOutput,
    ) -> Comparison {
        if candidate.samples.is_empty() {
            return Comparison {
                trend: Trend::Same,
                passed: false,
            };
        }
        let mut trends: Vec<Trend> = params
            .rubric
            .iter()
            .map(|item| candidate.trend_for(&item.id, params.aggregate))
            .collect();
        let (before, after) = (baseline.findings(), candidate.findings());
        trends.push(after.blocking.cmp(&before.blocking).reverse().into());
        let trend = if trends.contains(&Trend::Worse) {
            Trend::Worse
        } else if trends.contains(&Trend::Better) {
            Trend::Better
        } else {
            Trend::Same
        };
        Comparison {
            trend,
            passed: after.blocking == 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(samples: u32, aggregate: Aggregate) -> EvaluatorParams {
        EvaluatorParams {
            rubric: vec![RubricItem {
                id: "clarity".into(),
                criterion: "the code reads clearly".into(),
            }],
            samples,
            aggregate,
        }
    }

    fn sample(trend: Trend, blocking: u32) -> EvaluatorSample {
        EvaluatorSample {
            judgements: vec![Judgement {
                item: "clarity".into(),
                trend,
                evidence: vec![Evidence {
                    path: "src/lib.rs".into(),
                    line: 7,
                }],
            }],
            findings: Findings {
                blocking,
                major: 0,
                minor: 0,
            },
        }
    }

    fn output(samples: Vec<EvaluatorSample>) -> EvaluatorOutput {
        EvaluatorOutput { samples }
    }

    #[test]
    fn majority_and_median_aggregate_samples() {
        let base = output(Vec::new());
        let votes = output(vec![
            sample(Trend::Better, 0),
            sample(Trend::Better, 0),
            sample(Trend::Worse, 0),
        ]);
        let by_majority = EvaluatorCheck::compare(&params(3, Aggregate::Majority), &base, &votes);
        assert_eq!(by_majority.trend, Trend::Better);
        let by_median = EvaluatorCheck::compare(&params(3, Aggregate::Median), &base, &votes);
        assert_eq!(by_median.trend, Trend::Better);

        let split = output(vec![sample(Trend::Better, 0), sample(Trend::Worse, 0)]);
        let tie = EvaluatorCheck::compare(&params(2, Aggregate::Majority), &base, &split);
        assert_eq!(tie.trend, Trend::Same);
        let low = EvaluatorCheck::compare(&params(2, Aggregate::Median), &base, &split);
        assert_eq!(low.trend, Trend::Worse);
    }

    #[test]
    fn a_change_without_evidence_counts_as_unchanged() {
        let mut unsupported = sample(Trend::Better, 0);
        unsupported.judgements[0].evidence.clear();
        let result = EvaluatorCheck::compare(
            &params(1, Aggregate::Majority),
            &output(Vec::new()),
            &output(vec![unsupported]),
        );
        assert_eq!(result.trend, Trend::Same);
        assert!(result.passed);
    }

    #[test]
    fn blocking_findings_fail_and_their_increase_regresses() {
        let p = params(1, Aggregate::Majority);
        let clean = output(vec![sample(Trend::Same, 0)]);
        let blocked = output(vec![sample(Trend::Same, 2)]);
        let worse = EvaluatorCheck::compare(&p, &clean, &blocked);
        assert_eq!(
            worse,
            Comparison {
                trend: Trend::Worse,
                passed: false
            }
        );
        let fixed = EvaluatorCheck::compare(&p, &blocked, &clean);
        assert_eq!(
            fixed,
            Comparison {
                trend: Trend::Better,
                passed: true
            }
        );
    }

    #[test]
    fn findings_use_the_lower_median_per_severity() {
        let out = output(vec![
            sample(Trend::Same, 5),
            sample(Trend::Same, 1),
            sample(Trend::Same, 3),
        ]);
        assert_eq!(out.findings().blocking, 3);
        assert_eq!(output(Vec::new()).findings(), Findings::default());
    }

    #[test]
    fn no_samples_fails() {
        let result = EvaluatorCheck::compare(
            &params(1, Aggregate::Majority),
            &output(Vec::new()),
            &output(Vec::new()),
        );
        assert!(!result.passed);
    }

    #[test]
    fn validation_catches_zero_samples_and_rubric_problems() {
        let mut bad = params(0, Aggregate::Majority);
        bad.rubric.push(bad.rubric[0].clone());
        bad.rubric.push(RubricItem {
            id: " ".into(),
            criterion: "x".into(),
        });
        assert_eq!(
            EvaluatorCheck::validate(&bad),
            vec![
                Problem::ZeroSamples,
                Problem::DuplicateRubricItem {
                    item: "clarity".into()
                },
                Problem::EmptyRubricItem,
            ]
        );
        let empty = EvaluatorParams {
            rubric: Vec::new(),
            samples: 1,
            aggregate: Aggregate::Median,
        };
        assert_eq!(EvaluatorCheck::validate(&empty), vec![Problem::EmptyRubric]);
    }

    #[test]
    fn samples_default_to_one() {
        let parsed: EvaluatorParams =
            serde_json::from_str(r#"{"rubric":[],"aggregate":"majority"}"#).unwrap();
        assert_eq!(parsed.samples, 1);
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn evaluation_sample_wire_is_closed_and_lines_are_positive() {
        let valid = json!({"judgements": [{"item": "clarity", "trend": "better",
            "evidence": [{"path": "src/lib.rs", "line": 1}]}],
            "findings": {"blocking": 0, "major": 0, "minor": 0}});
        assert!(serde_json::from_value::<EvaluatorSample>(valid.clone()).is_ok());
        for pointer in ["", "/judgements/0", "/judgements/0/evidence/0", "/findings"] {
            let mut invalid = valid.clone();
            invalid
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("extra".into(), json!(true));
            assert!(
                serde_json::from_value::<EvaluatorSample>(invalid).is_err(),
                "{pointer}"
            );
        }
        let mut invalid = valid;
        invalid["judgements"][0]["evidence"][0]["line"] = json!(0);
        assert!(serde_json::from_value::<EvaluatorSample>(invalid).is_err());
    }
}
