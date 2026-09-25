//! A Claude Planner's model choice (#1810, amending design #1791 §5.8): the one list of the CLI's
//! model aliases. The CLI resolves each alias to its current model, so the list does not go stale
//! and needs no configuration. `null` is the CLI's own default and sends no `--model`; there is no
//! reasoning-effort choice. `GET /api/models`, `PUT /planner/model`, track create and the run loop
//! all judge a selection here.

use serde_json::Value;

use crate::planner_model::{CardModelSelection, TurnModelSelection};

/// One alias a Claude Planner can run.
#[derive(Debug, PartialEq, Eq)]
pub struct ClaudeModel {
    /// What `--model` is given.
    pub alias: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
}

/// Every alias, in the order a picker lists them.
pub const MODELS: &[ClaudeModel] = &[
    ClaudeModel {
        alias: "opus",
        display_name: "Opus",
        description: "The most capable Claude model.",
    },
    ClaudeModel {
        alias: "sonnet",
        display_name: "Sonnet",
        description: "Balances capability and speed.",
    },
    ClaudeModel {
        alias: "haiku",
        display_name: "Haiku",
        description: "The fastest Claude model.",
    },
];

/// Why a selection cannot run on a Claude Planner. Refused, never adjusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidSelection {
    UnknownModel(String),
    Effort(String),
}

impl std::fmt::Display for InvalidSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownModel(model) => write!(
                f,
                "`{model}` is not a Claude model alias; choose one of {}, or null for the Claude \
                 CLI's default",
                MODELS
                    .iter()
                    .map(|m| m.alias)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Effort(effort) => write!(
                f,
                "a Claude Planner has no reasoning-effort choice; `reasoning_effort` must be null, \
                 not `{effort}`"
            ),
        }
    }
}

/// The alias `model` names (`None` for the CLI default), or why the pair is refused.
pub fn resolve(
    model: Option<&str>,
    reasoning_effort: Option<&str>,
) -> Result<Option<&'static ClaudeModel>, InvalidSelection> {
    if let Some(effort) = reasoning_effort {
        return Err(InvalidSelection::Effort(effort.to_string()));
    }
    model
        .map(|model| {
            MODELS
                .iter()
                .find(|m| m.alias == model)
                .ok_or_else(|| InvalidSelection::UnknownModel(model.to_string()))
        })
        .transpose()
}

/// What a Claude Planner card's payload says its next turn runs, read at issue time. Each turn is
/// a fresh process, so `null` is simply the CLI default: no `*_ever_set` resolution is needed.
/// `Err((log, reader))` for a payload that cannot be run; the turn is not sent.
pub fn turn_selection(payload: &Value) -> Result<TurnModelSelection, (String, String)> {
    let card = CardModelSelection::from_payload(payload).map_err(|e| {
        (
            e.to_string(),
            "This conversation's saved model selection cannot be read. Pick a model to replace it."
                .to_string(),
        )
    })?;
    let model = resolve(card.model.as_deref(), card.reasoning_effort.as_deref()).map_err(|e| {
        (
            format!("the card's saved Claude selection cannot run: {e}"),
            format!("This conversation's saved model selection cannot run: {e}. Pick a model to replace it."),
        )
    })?;
    Ok(TurnModelSelection {
        model: model.map(|m| m.alias.to_string()),
        effort: None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn an_alias_or_null_resolves_and_anything_else_is_refused() {
        assert_eq!(resolve(None, None), Ok(None));
        assert_eq!(resolve(Some("sonnet"), None).unwrap().unwrap().alias, "sonnet");
        assert_eq!(
            resolve(Some("claude-sonnet-4-5"), None),
            Err(InvalidSelection::UnknownModel("claude-sonnet-4-5".into()))
        );
        assert_eq!(
            resolve(Some("opus"), Some("high")),
            Err(InvalidSelection::Effort("high".into()))
        );
        assert_eq!(
            resolve(None, Some("high")),
            Err(InvalidSelection::Effort("high".into()))
        );
    }

    #[test]
    fn the_turn_carries_the_alias_and_never_an_effort() {
        let got = turn_selection(&json!({"model": "haiku", "model_ever_set": true})).unwrap();
        assert_eq!(got.model.as_deref(), Some("haiku"));
        assert_eq!(got.effort, None);
        // A card that chose once and then chose the default again runs the CLI default.
        let got = turn_selection(&json!({"model": null, "model_ever_set": true})).unwrap();
        assert_eq!(got, TurnModelSelection::inherit());
        for payload in [
            json!({"model": "gpt-5"}),
            json!({"model": null, "reasoning_effort": "high"}),
            json!({"model": 42}),
        ] {
            assert!(turn_selection(&payload).is_err(), "{payload}");
        }
    }
}
