//! A Claude Planner's model catalog (#1822, amending design #1791 §5.8 and #1810): the Claude
//! CLI's own `/model` list, as its `initialize` answer reports it (`catalog_fetch`), cached beside
//! the availability check (`availability`). There is no fixed list. `GET /api/models`,
//! `PUT /planner/model`, track create and the run loop all judge a selection here.

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

    /// What a turn runs for the selection, or why the catalog refuses it. `model` must be a listed
    /// value (`null` is the CLI's default), and an effort must be one that entry declares.
    pub fn judge(
        &self,
        model: Option<&str>,
        reasoning_effort: Option<&str>,
    ) -> Result<TurnModelSelection, InvalidSelection> {
        let entry = match model {
            None => &self.default,
            Some(model) => self
                .models
                .iter()
                .find(|m| m.value == model)
                .ok_or_else(|| InvalidSelection::UnknownModel {
                    model: model.to_string(),
                    listed: self.models.iter().map(|m| m.value.clone()).collect(),
                })?,
        };
        if let Some(effort) = reasoning_effort
            && !entry.effort_levels.iter().any(|level| level == effort)
        {
            return Err(InvalidSelection::Effort {
                effort: effort.to_string(),
                model: model.map(str::to_string),
                supported: entry.effort_levels.clone(),
            });
        }
        Ok(TurnModelSelection {
            model: model.map(str::to_string),
            effort: reasoning_effort.map(str::to_string),
        })
    }

    /// What a Claude Planner card's payload says its next turn runs, read at issue time. Each turn
    /// is a fresh process, so `null` is simply the CLI default: no `*_ever_set` resolution is
    /// needed. `Err((log, reader))` for a payload that cannot run; the turn is not sent.
    pub fn turn_selection(&self, payload: &Value) -> Result<TurnModelSelection, (String, String)> {
        let card = CardModelSelection::from_payload(payload).map_err(|e| {
            (
                e.to_string(),
                "This conversation's saved model selection cannot be read. Pick a model to replace it."
                    .to_string(),
            )
        })?;
        self.judge(card.model.as_deref(), card.reasoning_effort.as_deref())
            .map_err(|e| {
                (
                    format!("the card's saved Claude selection cannot run: {e}"),
                    format!(
                        "This conversation's saved model selection cannot run: {e}. Pick a model \
                         to replace it."
                    ),
                )
            })
    }
}

/// Why a selection cannot run on a Claude Planner. Refused, never adjusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidSelection {
    /// `model` is not a value of the catalog, which lists `listed`.
    UnknownModel { model: String, listed: Vec<String> },
    /// `effort` is not one the entry (`model`, `None` for the CLI default) declares.
    Effort {
        effort: String,
        model: Option<String>,
        supported: Vec<String>,
    },
}

impl std::fmt::Display for InvalidSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownModel { model, listed } => write!(
                f,
                "`{model}` is not in the Claude CLI's model list; choose one of {}, or null for \
                 the Claude CLI's default",
                listed.join(", ")
            ),
            Self::Effort {
                effort,
                model,
                supported,
            } => {
                let named = model.as_deref().map_or_else(
                    || "the Claude CLI's default".to_string(),
                    |model| format!("`{model}`"),
                );
                if supported.is_empty() {
                    write!(
                        f,
                        "{named} declares no reasoning effort; `reasoning_effort` must be null, \
                         not `{effort}`"
                    )
                } else {
                    write!(
                        f,
                        "{named} does not support reasoning effort `{effort}`; choose one of {}, \
                         or null",
                        supported.join(", ")
                    )
                }
            }
        }
    }
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

    fn catalog() -> ClaudeCatalog {
        ClaudeCatalog::from_entries(
            vec![
                entry(DEFAULT_VALUE, &["low", "high"]),
                entry("claude-fable-5-1[1m]", &["low", "max"]),
                entry("haiku", &[]),
            ],
            1,
        )
        .expect("a well-formed list")
    }

    #[test]
    fn a_listed_value_or_null_runs_with_a_declared_effort_and_nothing_else_does() {
        let catalog = catalog();
        assert_eq!(catalog.judge(None, None), Ok(TurnModelSelection::inherit()));
        let fable = catalog
            .judge(Some("claude-fable-5-1[1m]"), Some("max"))
            .unwrap();
        assert_eq!(fable.model.as_deref(), Some("claude-fable-5-1[1m]"));
        assert_eq!(fable.effort.as_deref(), Some("max"));
        // The null selection takes the default entry's levels.
        assert_eq!(
            catalog.judge(None, Some("high")).unwrap().effort.as_deref(),
            Some("high")
        );
        let unknown = catalog.judge(Some("opus"), None).unwrap_err();
        assert!(
            unknown
                .to_string()
                .contains("choose one of claude-fable-5-1[1m], haiku"),
            "{unknown}"
        );
        // `default` is the null selection, never a value to store.
        assert!(matches!(
            catalog.judge(Some(DEFAULT_VALUE), None),
            Err(InvalidSelection::UnknownModel { .. })
        ));
        assert!(matches!(
            catalog.judge(Some("haiku"), Some("low")),
            Err(InvalidSelection::Effort { .. })
        ));
        assert!(matches!(
            catalog.judge(Some("claude-fable-5-1[1m]"), Some("high")),
            Err(InvalidSelection::Effort { .. })
        ));
        assert!(matches!(
            catalog.judge(None, Some("max")),
            Err(InvalidSelection::Effort { .. })
        ));
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
    fn the_turn_carries_the_value_and_the_effort() {
        let catalog = catalog();
        let got = catalog
            .turn_selection(&json!({"model": "claude-fable-5-1[1m]", "reasoning_effort": "low", "model_ever_set": true}))
            .unwrap();
        assert_eq!(got.model.as_deref(), Some("claude-fable-5-1[1m]"));
        assert_eq!(got.effort.as_deref(), Some("low"));
        // A card that chose once and then chose the default again runs the CLI default.
        let got = catalog
            .turn_selection(&json!({"model": null, "model_ever_set": true}))
            .unwrap();
        assert_eq!(got, TurnModelSelection::inherit());
        for payload in [
            json!({"model": "gpt-5"}),
            json!({"model": "haiku", "reasoning_effort": "high"}),
            json!({"model": 42}),
        ] {
            assert!(catalog.turn_selection(&payload).is_err(), "{payload}");
        }
    }
}
