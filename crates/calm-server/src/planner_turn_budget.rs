//! Owner of the app-global Planner execution budget. Storage is never repaired implicitly.
use std::time::Duration;

pub use calm_types::harness::budget::PLANNER_TURN_BUDGET_KEY as KEY;
pub use calm_types::harness::budget::{FrozenBudget, PlannerTurnBudget};

/// Only malformed stored values invite a reset; repository failures keep their original cause.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("{0}")]
    InvalidSetting(String),
    #[error(transparent)]
    Read(#[from] crate::error::CalmError),
}

pub(crate) trait PlannerTurnBudgetExt: Sized {
    fn warning_remaining(self) -> Duration;
    fn warning_input(self) -> String;
    fn initial_input(self) -> String;
    async fn load(repo: &dyn crate::db::RepoRead) -> Result<Self, LoadError>;
}

impl PlannerTurnBudgetExt for PlannerTurnBudget {
    fn warning_remaining(self) -> Duration {
        Duration::from_secs(300).min(self.limit() / 5)
    }
    fn warning_input(self) -> String {
        format!(
            "[Trusted Planner turn budget reminder]\nAt most {} seconds of execution remain. Wrap up now before the hard stop.",
            self.warning_remaining().as_secs()
        )
    }
    fn initial_input(self) -> String {
        format!(
            "[回合预算] {} 分钟，剩 {} 秒时收尾",
            u16::from(self),
            self.warning_remaining().as_secs()
        )
    }
    async fn load(repo: &dyn crate::db::RepoRead) -> Result<Self, LoadError> {
        let rows = repo
            .settings_get_all()
            .await
            .map_err(crate::error::CalmError::from)?;
        match rows.into_iter().find(|(key, _)| key == KEY) {
            None => Ok(Self::default()),
            Some((_, value)) => Self::parse(&value).map_err(LoadError::InvalidSetting),
        }
    }
}

/// One closed execution-policy notice. Construction does not authorize a send:
/// the budget owner must persist its once-only flag and fence the original carrier.
/// No request/tool payload can supply arbitrary text or attachments here.
#[derive(Clone, Debug)]
pub(crate) struct BudgetControlNotice {
    budget: PlannerTurnBudget,
    thread_id: String,
    turn_id: String,
    client_id: String,
}

impl BudgetControlNotice {
    pub(crate) fn for_turn(
        budget: PlannerTurnBudget,
        thread_id: &str,
        turn_id: &str,
        start_attempt: &str,
    ) -> Result<Self, &'static str> {
        if [thread_id, turn_id, start_attempt]
            .iter()
            .any(|value| value.trim().is_empty())
        {
            return Err("budget control requires its original thread, turn and start attempt");
        }
        Ok(Self {
            budget,
            thread_id: thread_id.into(),
            turn_id: turn_id.into(),
            // This is a native control correlation, not an accepted input ID.
            client_id: format!("{start_attempt}:budget-warning"),
        })
    }

    pub(crate) fn thread_id(&self) -> &str {
        &self.thread_id
    }

    pub(crate) fn turn_id(&self) -> &str {
        &self.turn_id
    }

    pub(crate) fn client_id(&self) -> &str {
        &self.client_id
    }

    pub(crate) fn text(&self) -> String {
        self.budget.warning_input()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn turn_budget_initial_is_one_line_and_keeps_final_warning() {
        let budget = PlannerTurnBudget::default();
        assert_eq!(
            budget.initial_input(),
            "[回合预算] 30 分钟，剩 300 秒时收尾"
        );
        assert!(
            budget
                .warning_input()
                .contains("Wrap up now before the hard stop.")
        );
    }
}
