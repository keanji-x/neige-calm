//! A Claude Planner's model catalog (#1822, amending design #1791 §5.8 and #1810): the Claude
//! CLI's own `/model` list, as its `initialize` answer reports it (`catalog_fetch`), cached by the
//! availability check (`availability`). There is no fixed list. `GET /api/models` answers it, and
//! `PUT /planner/model` and track create advise a selection against it as they do against Codex's
//! (6′); at issue a turn only reads the stored selection (`turn_selection`).

use serde_json::Value;

use crate::planner_model::CardModelSelection;
use provider::TurnModelSelection;
use provider::claude::discovery::ClaudeModel;

/// The validated CLI list together with host-owned fetch time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeCatalog {
    pub default: ClaudeModel,
    pub models: Vec<ClaudeModel>,
    pub fetched_at_ms: i64,
}

/// What a Claude Planner card's payload says its next turn runs, read at issue time: a type check
/// of the stored keys only (#1822 6′). The write advised the selection against the catalog (an
/// unlisted model is stored, flagged `unknown_model`); at issue the CLI is the judge of the model,
/// and refuses one it cannot run with its own words. Each
/// turn is a fresh process, so `null` is simply the CLI default: no `*_ever_set` resolution is
/// needed. `Err((log, reader))` for a payload that cannot be read; the turn is not sent.
pub fn turn_selection(payload: &Value) -> Result<TurnModelSelection, (String, String)> {
    let card = CardModelSelection::from_payload(payload).map_err(|e| {
        (
            e.to_string(),
            "This conversation's saved model selection cannot be read. Pick a model to replace it."
                .to_string(),
        )
    })?;
    Ok(TurnModelSelection {
        model: card.model,
        effort: card.reasoning_effort,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_turn_carries_the_stored_value_and_effort_and_refuses_only_an_unreadable_payload() {
        let got = turn_selection(&json!({"model": "claude-fable-5-1[1m]", "reasoning_effort": "low", "model_ever_set": true}))
            .unwrap();
        assert_eq!(got.model.as_deref(), Some("claude-fable-5-1[1m]"));
        assert_eq!(got.effort.as_deref(), Some("low"));
        // A card that chose once and then chose the default again runs the CLI default.
        let got = turn_selection(&json!({"model": null, "model_ever_set": true})).unwrap();
        assert_eq!(got, TurnModelSelection::inherit());
        // A value no catalog lists still reaches the CLI, which judges it.
        let got =
            turn_selection(&json!({"model": "claude-bogus-9-9", "model_ever_set": true})).unwrap();
        assert_eq!(got.model.as_deref(), Some("claude-bogus-9-9"));
        for payload in [json!({"model": 42}), json!({"reasoning_effort": ["high"]})] {
            assert!(turn_selection(&payload).is_err(), "{payload}");
        }
    }
}
