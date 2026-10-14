//! Owner of the app-global Planner execution budget. Storage is never repaired implicitly.
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const KEY: &str = "planner_max_turn_duration_minutes";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct PlannerTurnBudget(u16);

impl Default for PlannerTurnBudget {
    fn default() -> Self {
        Self(30)
    }
}
impl TryFrom<u16> for PlannerTurnBudget {
    type Error = String;
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        if (1..=240).contains(&value) {
            Ok(Self(value))
        } else {
            Err(format!("{KEY}: must be in the range 1..240 minutes"))
        }
    }
}
impl From<PlannerTurnBudget> for u16 {
    fn from(value: PlannerTurnBudget) -> Self {
        value.0
    }
}
/// Only malformed stored values invite a reset; repository failures keep their original cause.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("{0}")]
    InvalidSetting(String),
    #[error(transparent)]
    Read(#[from] crate::error::CalmError),
}

impl PlannerTurnBudget {
    pub fn parse(value: &str) -> Result<Self, String> {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!(
                "{KEY}: must contain only ASCII decimal digits (1..240)"
            ));
        }
        let minutes = value
            .parse::<u16>()
            .map_err(|_| format!("{KEY}: decimal value overflows the supported range 1..240"))?;
        Self::try_from(minutes)
    }
    pub fn canonical(self) -> String {
        self.0.to_string()
    }
    pub fn limit(self) -> Duration {
        Duration::from_secs(u64::from(self.0) * 60)
    }
    pub fn warning_remaining(self) -> Duration {
        Duration::from_secs(300).min(self.limit() / 5)
    }
    pub fn initial_input(self) -> String {
        format!(
            "[Trusted Planner turn budget]\nThis turn's execution budget is {} minutes, excluding held requests. Begin wrapping up with {} seconds remaining; finish or hand off work before the hard stop. This budget is frozen for this turn.",
            self.0,
            self.warning_remaining().as_secs()
        )
    }
    pub async fn load(repo: &dyn crate::db::RepoRead) -> Result<Self, LoadError> {
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

/// An explicit legacy state preserves the existing recovery policy without guessing a budget.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FrozenBudget {
    #[default]
    Legacy,
    NotApplicable,
    Planner {
        budget: PlannerTurnBudget,
        start_attempt: crate::harness::queue::QueueEntryId,
        warning_attempted: bool,
    },
}
