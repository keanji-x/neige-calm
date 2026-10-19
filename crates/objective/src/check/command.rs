//! `command` checks: a command that passes when it exits with status zero.

use serde::{Deserialize, Serialize};

use super::{CheckKind, Comparison, Trend};
use crate::diagnostic::Problem;

pub struct CommandCheck;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandParams {
    /// The shell command the host runs in the evaluated revision.
    pub command: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutput {
    pub exit_code: i32,
}

impl CommandOutput {
    pub fn passed(&self) -> bool {
        self.exit_code == 0
    }
}

impl CheckKind for CommandCheck {
    const NAME: &'static str = "command";
    type Params = CommandParams;
    type Output = CommandOutput;

    fn validate(params: &CommandParams) -> Vec<Problem> {
        if params.command.trim().is_empty() {
            vec![Problem::EmptyCommand]
        } else {
            Vec::new()
        }
    }

    fn compare(
        _: &CommandParams,
        baseline: &CommandOutput,
        candidate: &CommandOutput,
    ) -> Comparison {
        Comparison {
            trend: Trend::between(baseline.passed(), candidate.passed()),
            passed: candidate.passed(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(baseline: i32, candidate: i32) -> Comparison {
        let params = CommandParams {
            command: "make test".into(),
        };
        CommandCheck::compare(
            &params,
            &CommandOutput {
                exit_code: baseline,
            },
            &CommandOutput {
                exit_code: candidate,
            },
        )
    }

    #[test]
    fn exit_status_decides_pass_and_trend() {
        assert_eq!(
            run(0, 0),
            Comparison {
                trend: Trend::Same,
                passed: true
            }
        );
        assert_eq!(
            run(1, 0),
            Comparison {
                trend: Trend::Better,
                passed: true
            }
        );
        assert_eq!(
            run(0, 2),
            Comparison {
                trend: Trend::Worse,
                passed: false
            }
        );
        assert_eq!(
            run(1, 3),
            Comparison {
                trend: Trend::Same,
                passed: false
            }
        );
    }

    #[test]
    fn blank_command_is_a_problem() {
        let blank = CommandParams {
            command: "  ".into(),
        };
        assert_eq!(CommandCheck::validate(&blank), vec![Problem::EmptyCommand]);
    }
}
