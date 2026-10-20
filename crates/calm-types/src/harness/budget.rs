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
    pub fn limit(self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.0) * 60)
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
        /// Historical issuance freezes this render text with the start attempt.
        /// Canonical issuance stores it in its submission render plan instead.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_usage: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frozen_budget_wire_names_round_trip() {
        let cases = [
            (FrozenBudget::Legacy, json!({"kind": "legacy"})),
            (
                FrozenBudget::NotApplicable,
                json!({"kind": "not_applicable"}),
            ),
            (
                FrozenBudget::Planner {
                    budget: PlannerTurnBudget::try_from(15).unwrap(),
                    start_attempt: super::super::snapshot::QueueEntryId::from_wire(
                        "attempt".into(),
                    ),
                    warning_attempted: true,
                    context_usage: None,
                },
                json!({
                    "kind": "planner",
                    "budget": 15,
                    "start_attempt": "attempt",
                    "warning_attempted": true
                }),
            ),
        ];
        for (value, wire) in cases {
            assert_eq!(serde_json::to_value(&value).unwrap(), wire);
            assert_eq!(serde_json::from_value::<FrozenBudget>(wire).unwrap(), value);
        }
        assert_eq!(FrozenBudget::default(), FrozenBudget::Legacy);
    }

    #[test]
    fn planner_turn_budget_wire_rejects_out_of_range_minutes() {
        assert_eq!(PlannerTurnBudget::default().canonical(), "30");
        for minutes in [0_u16, 241] {
            assert!(serde_json::from_value::<PlannerTurnBudget>(json!(minutes)).is_err());
        }
        assert_eq!(
            serde_json::from_value::<PlannerTurnBudget>(json!(240)).unwrap(),
            PlannerTurnBudget::parse("240").unwrap()
        );
    }
}
