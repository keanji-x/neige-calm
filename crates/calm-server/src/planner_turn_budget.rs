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

/// Server-owned settings loading and policy text for the shared persisted budget value.
pub(crate) trait PlannerTurnBudgetExt: Sized {
    fn warning_remaining(self) -> Duration;
    fn initial_input(self) -> String;
    async fn load(repo: &dyn crate::db::RepoRead) -> Result<Self, LoadError>;
}

impl PlannerTurnBudgetExt for PlannerTurnBudget {
    fn warning_remaining(self) -> Duration {
        Duration::from_secs(300).min(self.limit() / 5)
    }
    fn initial_input(self) -> String {
        format!(
            "[Trusted Planner turn budget]\nThis turn's execution budget is {} minutes, excluding held requests. Begin wrapping up with {} seconds remaining; finish or hand off work before the hard stop. This budget is frozen for this turn.",
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
