//! Validated persisted budget values. Loading settings and policy rendering stay in the server.
use serde::{Deserialize, Serialize};

pub const PLANNER_TURN_BUDGET_KEY: &str = "planner_max_turn_duration_minutes";

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
            Err(format!(
                "{PLANNER_TURN_BUDGET_KEY}: must be in the range 1..240 minutes"
            ))
        }
    }
}
impl From<PlannerTurnBudget> for u16 {
    fn from(value: PlannerTurnBudget) -> Self {
        value.0
    }
}

impl PlannerTurnBudget {
    pub fn parse(value: &str) -> Result<Self, String> {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!(
                "{PLANNER_TURN_BUDGET_KEY}: must contain only ASCII decimal digits (1..240)"
            ));
        }
        let minutes = value.parse::<u16>().map_err(|_| {
            format!("{PLANNER_TURN_BUDGET_KEY}: decimal value overflows the supported range 1..240")
        })?;
        Self::try_from(minutes)
    }
    pub fn canonical(self) -> String {
        self.0.to_string()
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
        start_attempt: super::snapshot::QueueEntryId,
        warning_attempted: bool,
    },
}
