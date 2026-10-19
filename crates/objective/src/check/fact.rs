//! `fact` checks: a boolean fact about the outside world, such as a merged pull request.

use serde::{Deserialize, Serialize};

use super::{CheckKind, Comparison, Trend};
use crate::diagnostic::Problem;

pub struct FactCheck;

/// The closed set of facts a host can report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fact {
    TaskAccepted,
    PrMerged,
    AskAnswered,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactParams {
    pub fact: Fact,
    /// What the fact is about: a task id, a pull request reference or an ask id.
    pub subject: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactOutput {
    pub holds: bool,
}

impl CheckKind for FactCheck {
    const NAME: &'static str = "fact";
    type Params = FactParams;
    type Output = FactOutput;

    fn validate(params: &FactParams) -> Vec<Problem> {
        if params.subject.trim().is_empty() {
            vec![Problem::EmptyFactSubject]
        } else {
            Vec::new()
        }
    }

    fn compare(_: &FactParams, baseline: &FactOutput, candidate: &FactOutput) -> Comparison {
        Comparison {
            trend: Trend::between(baseline.holds, candidate.holds),
            passed: candidate.holds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fact_that_starts_holding_improves() {
        let params = FactParams {
            fact: Fact::AskAnswered,
            subject: "ask-1".into(),
        };
        let no = FactOutput { holds: false };
        let yes = FactOutput { holds: true };
        assert_eq!(
            FactCheck::compare(&params, &no, &yes),
            Comparison {
                trend: Trend::Better,
                passed: true
            }
        );
        assert_eq!(
            FactCheck::compare(&params, &yes, &no),
            Comparison {
                trend: Trend::Worse,
                passed: false
            }
        );
        assert!(FactCheck::validate(&params).is_empty());
        let blank = FactParams {
            fact: Fact::PrMerged,
            subject: String::new(),
        };
        assert_eq!(FactCheck::validate(&blank), vec![Problem::EmptyFactSubject]);
    }
}
