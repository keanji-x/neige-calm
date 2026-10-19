//! `metric` checks: a command whose last output line is a number to push in one direction.

use serde::{Deserialize, Serialize};

use super::{CheckKind, Comparison, Trend};
use crate::diagnostic::Problem;

pub struct MetricCheck;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Higher,
    Lower,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetricParams {
    /// The shell command whose last non-empty output line is the metric.
    pub command: String,
    pub direction: Direction,
    /// Changes no larger than this count as unchanged. Absent means exact comparison.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerance: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetricOutput {
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetricParseError {
    NoOutput,
    NotANumber { line: String },
}

/// Parses a metric command's standard output: its last non-empty line must be a finite number.
pub fn parse_metric_output(stdout: &str) -> Result<MetricOutput, MetricParseError> {
    let line = stdout
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .ok_or(MetricParseError::NoOutput)?;
    match line.parse::<f64>() {
        Ok(value) if value.is_finite() => Ok(MetricOutput { value }),
        _ => Err(MetricParseError::NotANumber {
            line: line.to_owned(),
        }),
    }
}

impl CheckKind for MetricCheck {
    const NAME: &'static str = "metric";
    type Params = MetricParams;
    type Output = MetricOutput;

    fn validate(params: &MetricParams) -> Vec<Problem> {
        let mut problems = Vec::new();
        if params.command.trim().is_empty() {
            problems.push(Problem::EmptyCommand);
        }
        if params.tolerance.is_some_and(|t| !t.is_finite() || t < 0.0) {
            problems.push(Problem::InvalidTolerance);
        }
        problems
    }

    fn compare(
        params: &MetricParams,
        baseline: &MetricOutput,
        candidate: &MetricOutput,
    ) -> Comparison {
        let gain = match params.direction {
            Direction::Higher => candidate.value - baseline.value,
            Direction::Lower => baseline.value - candidate.value,
        };
        let tolerance = params.tolerance.unwrap_or(0.0);
        let trend = if gain > tolerance {
            Trend::Better
        } else if gain < -tolerance {
            Trend::Worse
        } else {
            Trend::Same
        };
        Comparison {
            trend,
            passed: trend != Trend::Worse,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(direction: Direction, tolerance: Option<f64>) -> MetricParams {
        MetricParams {
            command: "bench".into(),
            direction,
            tolerance,
        }
    }

    fn trend(params: &MetricParams, baseline: f64, candidate: f64) -> Trend {
        MetricCheck::compare(
            params,
            &MetricOutput { value: baseline },
            &MetricOutput { value: candidate },
        )
        .trend
    }

    #[test]
    fn direction_decides_what_improves() {
        let higher = params(Direction::Higher, None);
        assert_eq!(trend(&higher, 1.0, 2.0), Trend::Better);
        assert_eq!(trend(&higher, 2.0, 1.0), Trend::Worse);
        let lower = params(Direction::Lower, None);
        assert_eq!(trend(&lower, 1.0, 2.0), Trend::Worse);
        assert_eq!(trend(&lower, 2.0, 1.0), Trend::Better);
        assert_eq!(trend(&lower, 2.0, 2.0), Trend::Same);
    }

    #[test]
    fn changes_within_tolerance_are_unchanged() {
        let lower = params(Direction::Lower, Some(0.5));
        assert_eq!(trend(&lower, 10.0, 10.4), Trend::Same);
        assert_eq!(trend(&lower, 10.0, 9.6), Trend::Same);
        assert_eq!(trend(&lower, 10.0, 10.6), Trend::Worse);
        assert_eq!(trend(&lower, 10.0, 9.4), Trend::Better);
        let regressed = MetricCheck::compare(
            &lower,
            &MetricOutput { value: 1.0 },
            &MetricOutput { value: 2.0 },
        );
        assert!(!regressed.passed);
    }

    #[test]
    fn parses_the_last_non_empty_line() {
        assert_eq!(
            parse_metric_output("warming up\n12\n 3.5 \n\n"),
            Ok(MetricOutput { value: 3.5 })
        );
        assert_eq!(
            parse_metric_output("\n \n"),
            Err(MetricParseError::NoOutput)
        );
        assert_eq!(
            parse_metric_output("1\ndone"),
            Err(MetricParseError::NotANumber {
                line: "done".into()
            })
        );
        assert_eq!(
            parse_metric_output("inf"),
            Err(MetricParseError::NotANumber { line: "inf".into() })
        );
    }

    #[test]
    fn validation_rejects_bad_tolerance_and_blank_command() {
        let mut bad = params(Direction::Higher, Some(-1.0));
        bad.command = String::new();
        assert_eq!(
            MetricCheck::validate(&bad),
            vec![Problem::EmptyCommand, Problem::InvalidTolerance]
        );
        assert!(MetricCheck::validate(&params(Direction::Higher, Some(0.0))).is_empty());
    }
}
