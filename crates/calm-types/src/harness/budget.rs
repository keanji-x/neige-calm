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

/// Ownership of the immutable context fact for an issued Planner turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "text",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum FrozenContextUsage {
    Text(String),
    RenderPlan,
    /// Old Planner budgets did not record a context fact. Never resample or repair them.
    LegacyNotRecorded,
}

fn legacy_context_not_recorded() -> FrozenContextUsage {
    FrozenContextUsage::LegacyNotRecorded
}

/// Decode the released string/null format explicitly, without confusing absence with ownership.
fn deserialize_frozen_context<'de, D>(deserializer: D) -> Result<FrozenContextUsage, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wire {
        HistoricalText(String),
        Current(FrozenContextUsage),
    }
    Ok(match Option::<Wire>::deserialize(deserializer)? {
        Some(Wire::HistoricalText(text)) => FrozenContextUsage::Text(text),
        Some(Wire::Current(value)) => value,
        None => legacy_context_not_recorded(),
    })
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
        #[serde(
            default = "legacy_context_not_recorded",
            deserialize_with = "deserialize_frozen_context"
        )]
        context_usage: FrozenContextUsage,
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
                    context_usage: FrozenContextUsage::RenderPlan,
                },
                json!({
                    "kind": "planner",
                    "budget": 15,
                    "start_attempt": "attempt",
                    "warning_attempted": true,
                    "context_usage": {"kind": "render_plan"}
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
    fn frozen_budget_context_usage_preserves_legacy_and_ownership() {
        let base = json!({"kind":"planner", "budget":15,
            "start_attempt":"attempt", "warning_attempted":false});
        for (field, expected) in [
            (None, FrozenContextUsage::LegacyNotRecorded),
            (Some(json!(null)), FrozenContextUsage::LegacyNotRecorded),
            (
                Some(json!("frozen reading")),
                FrozenContextUsage::Text("frozen reading".into()),
            ),
            (
                Some(json!({"kind":"render_plan"})),
                FrozenContextUsage::RenderPlan,
            ),
            (
                Some(json!({"kind":"text", "text":"frozen reading"})),
                FrozenContextUsage::Text("frozen reading".into()),
            ),
            (
                Some(json!({"kind":"legacy_not_recorded"})),
                FrozenContextUsage::LegacyNotRecorded,
            ),
        ] {
            let mut wire = base.clone();
            if let Some(field) = field {
                wire["context_usage"] = field;
            }
            let value: FrozenBudget = serde_json::from_value(wire).unwrap();
            let FrozenBudget::Planner { context_usage, .. } = &value else {
                panic!("planner")
            };
            assert_eq!(context_usage, &expected);
            assert_eq!(
                serde_json::from_value::<FrozenBudget>(serde_json::to_value(&value).unwrap())
                    .unwrap(),
                value
            );
        }
        for invalid in [json!(17), json!({"kind":"text"}), json!({"kind":"unknown"})] {
            let mut wire = base.clone();
            wire["context_usage"] = invalid;
            assert!(serde_json::from_value::<FrozenBudget>(wire).is_err());
        }
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
