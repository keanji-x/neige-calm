//! A Claude Planner's model catalog (#1822, amending design #1791 §5.8 and #1810): the Claude
//! CLI's own `/model` list, as its `initialize` answer reports it (`catalog_fetch`), cached by the
//! availability check (`availability`). There is no fixed list. `GET /api/models` answers it, and
//! `PUT /planner/model` and track create advise a selection against it as they do against Codex's
//! (6′); at issue a turn only reads the stored selection (`turn_selection`).

use serde_json::Value;

use crate::planner_model::{CardModelSelection, TurnModelSelection};

/// The `value` of the CLI's own default entry. It is the `null` selection, never a stored one.
pub const DEFAULT_VALUE: &str = "default";

/// One entry of the CLI's model list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeModel {
    /// The CLI's `value`: what `--model` is given, verbatim.
    pub value: String,
    /// The CLI's `resolvedModel`: the model this entry runs.
    pub resolved_model: String,
    pub display_name: String,
    pub description: String,
    /// The CLI's `supportedEffortLevels`, in its order; empty when it declares none, and then no
    /// effort is accepted.
    pub effort_levels: Vec<String>,
}

/// The CLI's model list as one `initialize` answered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeCatalog {
    /// The CLI's [`DEFAULT_VALUE`] entry: what the `null` selection runs, and its effort levels.
    pub default: ClaudeModel,
    /// Every other entry, in the CLI's order: the values a selection may name.
    pub models: Vec<ClaudeModel>,
    /// Wall-clock ms at which the list was fetched.
    pub fetched_at_ms: i64,
}

impl ClaudeCatalog {
    /// The catalog of the CLI's entries: exactly one [`DEFAULT_VALUE`], at least one other entry,
    /// and no value twice. `Err` says which rule the list breaks.
    pub fn from_entries(entries: Vec<ClaudeModel>, fetched_at_ms: i64) -> Result<Self, String> {
        let mut default = None;
        let mut models: Vec<ClaudeModel> = Vec::new();
        for entry in entries {
            if entry.value == DEFAULT_VALUE {
                if default.replace(entry).is_some() {
                    return Err(format!("lists `{DEFAULT_VALUE}` twice"));
                }
            } else if models.iter().any(|m| m.value == entry.value) {
                return Err(format!("lists `{}` twice", entry.value));
            } else {
                models.push(entry);
            }
        }
        let default = default.ok_or_else(|| format!("lists no `{DEFAULT_VALUE}` entry"))?;
        if models.is_empty() {
            return Err("lists no model besides the default".into());
        }
        Ok(Self {
            default,
            models,
            fetched_at_ms,
        })
    }
}

/// What a Claude Planner card's payload says its next turn runs, read at issue time: a type check
/// of the stored keys only (#1822 6′). The catalog judged the selection when it was written; at
/// issue the CLI is the judge of the model, and refuses one it cannot run with its own words. Each
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

    fn entry(value: &str, levels: &[&str]) -> ClaudeModel {
        ClaudeModel {
            value: value.into(),
            resolved_model: format!("resolved-{value}"),
            display_name: value.to_uppercase(),
            description: String::new(),
            effort_levels: levels.iter().map(|level| level.to_string()).collect(),
        }
    }

    #[test]
    fn a_list_without_one_default_or_another_entry_or_with_a_repeat_is_refused() {
        for entries in [
            vec![],
            vec![entry(DEFAULT_VALUE, &[])],
            vec![entry("haiku", &[])],
            vec![
                entry(DEFAULT_VALUE, &[]),
                entry(DEFAULT_VALUE, &[]),
                entry("haiku", &[]),
            ],
            vec![
                entry(DEFAULT_VALUE, &[]),
                entry("haiku", &[]),
                entry("haiku", &[]),
            ],
        ] {
            let count = entries.len();
            assert!(ClaudeCatalog::from_entries(entries, 1).is_err(), "{count}");
        }
    }

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
