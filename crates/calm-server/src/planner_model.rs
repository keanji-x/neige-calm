//! Which model a planner conversation's turns run with: the card payload keys and the pure rule turning a stored selection into `turn/start`'s `model` / `effort`.
//! Codex's `turn/start` model override is sticky, so "follow the default" after an explicit choice must send the default explicitly; the monotone `*_ever_set` markers are the predicate for that and nothing clears them.

use serde_json::Value;

/// `cards.payload_json` key: the chosen model **slug** (never a preset id), or JSON `null` for "follow the installation default".
pub const PAYLOAD_MODEL: &str = "model";
/// `cards.payload_json` key: monotone "a model has been chosen on this card at least once"; nothing clears it.
pub const PAYLOAD_MODEL_EVER_SET: &str = "model_ever_set";
/// `cards.payload_json` key: the chosen reasoning effort, or JSON `null`.
pub const PAYLOAD_EFFORT: &str = "reasoning_effort";
/// `cards.payload_json` key: the monotone marker for the effort, parallel to [`PAYLOAD_MODEL_EVER_SET`].
pub const PAYLOAD_EFFORT_EVER_SET: &str = "reasoning_effort_ever_set";

/// What a card's payload says about the model its turns run with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardModelSelection {
    /// The chosen slug, or `None` for "follow the default".
    pub model: Option<String>,
    pub model_ever_set: bool,
    /// The chosen reasoning effort, or `None` for "follow the default". A bare `String`: codex accepts any non-empty string on the wire (`ReasoningEffort::Custom`).
    pub reasoning_effort: Option<String>,
    pub reasoning_effort_ever_set: bool,
}

/// A payload whose model keys are present but not of the type they must be. Its own outcome rather than "treat as unset": guessing is how a wrong model gets used silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedSelection {
    pub key: &'static str,
    pub found: String,
}

impl std::fmt::Display for MalformedSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "card payload key `{}` is a {}, which is not a value this card's model selection can \
             be read from",
            self.key, self.found
        )
    }
}

fn type_name(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(_) => "boolean".into(),
        Value::Number(_) => "number".into(),
        Value::String(_) => "string".into(),
        Value::Array(_) => "array".into(),
        Value::Object(_) => "object".into(),
    }
}

fn read_nullable_string(
    payload: &Value,
    key: &'static str,
) -> Result<Option<String>, MalformedSelection> {
    match payload.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(MalformedSelection {
            key,
            found: type_name(other),
        }),
    }
}

fn read_marker(payload: &Value, key: &'static str) -> Result<bool, MalformedSelection> {
    match payload.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(MalformedSelection {
            key,
            found: type_name(other),
        }),
    }
}

impl CardModelSelection {
    /// Read the four keys off a card payload. An absent key reads as "never set"; a key of the wrong type is refused.
    pub fn from_payload(payload: &Value) -> Result<Self, MalformedSelection> {
        Ok(Self {
            model: read_nullable_string(payload, PAYLOAD_MODEL)?,
            model_ever_set: read_marker(payload, PAYLOAD_MODEL_EVER_SET)?,
            reasoning_effort: read_nullable_string(payload, PAYLOAD_EFFORT)?,
            reasoning_effort_ever_set: read_marker(payload, PAYLOAD_EFFORT_EVER_SET)?,
        })
    }

    /// Write a new selection into `payload`, in place. The `*_ever_set` markers only ever go up; both value keys are written unconditionally, including as JSON `null`, so "follow the default" is a stored fact rather than the absence of one.
    pub fn apply_to_payload(
        payload: &mut serde_json::Map<String, Value>,
        model: Option<&str>,
        reasoning_effort: Option<&str>,
    ) {
        payload.insert(
            PAYLOAD_MODEL.into(),
            model.map_or(Value::Null, |m| Value::String(m.to_string())),
        );
        payload.insert(
            PAYLOAD_EFFORT.into(),
            reasoning_effort.map_or(Value::Null, |e| Value::String(e.to_string())),
        );
        if model.is_some() {
            payload.insert(PAYLOAD_MODEL_EVER_SET.into(), Value::Bool(true));
        }
        if reasoning_effort.is_some() {
            payload.insert(PAYLOAD_EFFORT_EVER_SET.into(), Value::Bool(true));
        }
    }

    /// Which of the two selections forces the installation defaults read; separate because a person can only unstick it by fixing the disjunct that is actually true.
    pub fn defaults_needed_for(&self) -> DefaultsNeededFor {
        DefaultsNeededFor {
            model: self.model.is_none() && self.model_ever_set,
            effort: self.reasoning_effort.is_none() && self.reasoning_effort_ever_set,
        }
    }

    /// True only in the "chose something once, then chose the default again" case, for either half; every other card resolves from the payload alone with no extra RPC.
    pub fn needs_installation_defaults(&self) -> bool {
        let needed = self.defaults_needed_for();
        needed.model || needed.effort
    }

    /// Only the effort can want the catalog, and only as the last step of its chain.
    pub fn needs_catalog(&self) -> bool {
        self.reasoning_effort.is_none() && self.reasoning_effort_ever_set
    }
}

/// Which halves of a card's selection follow the installation default having once been chosen explicitly, so the reader-facing message is derived from the reason the read happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultsNeededFor {
    pub model: bool,
    pub effort: bool,
}

impl DefaultsNeededFor {
    /// What the reader must choose, named; both when both are, because fixing one still leaves the other entering the same branch.
    pub fn choice_to_make(self) -> Option<&'static str> {
        match (self.model, self.effort) {
            (true, true) => Some("a model and a reasoning effort"),
            (true, false) => Some("a model"),
            (false, true) => Some("a reasoning effort"),
            (false, false) => None,
        }
    }

    /// The same, as the subject of "the default … cannot be resolved".
    pub fn subject(self) -> Option<&'static str> {
        match (self.model, self.effort) {
            (true, true) => Some("model and reasoning effort"),
            (true, false) => Some("model"),
            (false, true) => Some("reasoning effort"),
            (false, false) => None,
        }
    }
}

/// The installation defaults, as codex's layer-merged `config/read` reports them for the thread's workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallationDefaults {
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}

/// What a `turn/start` frame must say about the model. `None` means the key is not put on the frame at all, leaving whatever the thread already carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnModelSelection {
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl TurnModelSelection {
    /// Send neither key. Correct only where we have never put a sticky value on the thread.
    pub fn inherit() -> Self {
        Self::default()
    }
}

/// Which half of the selection could not be determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnresolvedSelection {
    /// The card follows the default model, has carried an explicit one before, and we could not learn what the default is.
    Model,
    /// The same, for the reasoning effort.
    Effort,
}

/// Whether waiting is a plan: a property of the individual failure, not a constant, and the run loop reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The attempt may succeed if repeated unchanged, and no message to the reader is owed yet. Covers every way asking codex can fail short of codex answering.
    Retryable,
    /// Codex answered with a refusal of this input; repeating unchanged reproduces it. Reachable from the picker, which stores slugs codex has never heard of by design.
    Rejected,
    /// The selection itself cannot be determined; `config.model = null` and a malformed payload are both reachable and neither clears itself.
    NeedsAChoice,
}

impl UnresolvedSelection {
    /// Why the turn was not sent; shown to the reader for `NeedsAChoice`, so it names the one action that works and nothing else.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Model => {
                "This conversation follows the default model, and codex's configuration does not \
                 name one. Pick a model to start it again."
            }
            Self::Effort => {
                "This conversation follows the default reasoning effort, and neither codex's \
                 configuration nor the model catalog names one. Pick a reasoning effort to start \
                 it again."
            }
        }
    }

    /// The same failure as a log line, with no advice in it.
    pub fn log_reason(self) -> &'static str {
        match self {
            Self::Model => {
                "the model this conversation follows could not be determined from the card or \
                 from codex's effective config"
            }
            Self::Effort => {
                "the reasoning effort this conversation follows could not be determined from the \
                 card, codex's effective config, or the model catalog"
            }
        }
    }
}

/// Turn a stored selection into the frame members for one `turn/start`. Model chain: card slug → installation default → refuse. Effort chain: card effort → installation default → the running model's preset default → refuse.
/// The effort's third step exists because `TurnStartParams::effort` cannot express "clear the effort", so the preset default is the closest honest answer.
pub fn resolve_turn_selection(
    card: &CardModelSelection,
    defaults: Option<&InstallationDefaults>,
    catalog_default_effort: Option<&str>,
) -> Result<TurnModelSelection, UnresolvedSelection> {
    // Every refusal reachable from here is a `NeedsAChoice`: the caller only passes `defaults` once codex has answered.
    let model = match (&card.model, card.model_ever_set) {
        (Some(slug), _) => Some(slug.clone()),
        (None, false) => None,
        (None, true) => Some(
            defaults
                .and_then(|d| d.model.clone())
                .ok_or(UnresolvedSelection::Model)?,
        ),
    };
    let effort = match (&card.reasoning_effort, card.reasoning_effort_ever_set) {
        (Some(effort), _) => Some(effort.clone()),
        (None, false) => None,
        (None, true) => Some(
            defaults
                .and_then(|d| d.reasoning_effort.clone())
                .or_else(|| catalog_default_effort.map(ToOwned::to_owned))
                .ok_or(UnresolvedSelection::Effort)?,
        ),
    };
    Ok(TurnModelSelection { model, effort })
}

/// The slug whose catalog entry decides the effort fallback: the card's own choice if it has one, else the installation default.
pub fn effective_model_for_catalog_lookup<'a>(
    card: &'a CardModelSelection,
    defaults: Option<&'a InstallationDefaults>,
) -> Option<&'a str> {
    card.model
        .as_deref()
        .or_else(|| defaults.and_then(|d| d.model.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_card_nobody_touched_reads_as_untouched_and_sends_nothing() {
        let card = CardModelSelection::from_payload(&json!({ "codex_thread_id": "t1" })).unwrap();
        assert_eq!(card, CardModelSelection::default());
        assert!(!card.needs_installation_defaults());
        assert_eq!(
            resolve_turn_selection(&card, None, None).unwrap(),
            TurnModelSelection::inherit()
        );
    }

    #[test]
    fn an_explicit_slug_travels_verbatim() {
        let card =
            CardModelSelection::from_payload(&json!({ "model": "gpt-5", "model_ever_set": true }))
                .unwrap();
        assert!(!card.needs_installation_defaults());
        let sel = resolve_turn_selection(&card, None, None).unwrap();
        assert_eq!(sel.model.as_deref(), Some("gpt-5"));
        assert_eq!(sel.effort, None);
    }

    #[test]
    fn null_after_a_choice_resolves_the_default_rather_than_going_quiet() {
        let card =
            CardModelSelection::from_payload(&json!({ "model": null, "model_ever_set": true }))
                .unwrap();
        assert!(card.needs_installation_defaults());
        let defaults = InstallationDefaults {
            model: Some("gpt-5-codex".into()),
            reasoning_effort: None,
        };
        let sel = resolve_turn_selection(&card, Some(&defaults), None).unwrap();
        assert_eq!(
            sel.model.as_deref(),
            Some("gpt-5-codex"),
            "a card that has carried a sticky model must be told the default explicitly"
        );
    }

    #[test]
    fn null_without_a_prior_choice_stays_quiet() {
        let card =
            CardModelSelection::from_payload(&json!({ "model": null, "model_ever_set": false }))
                .unwrap();
        assert!(!card.needs_installation_defaults());
        let defaults = InstallationDefaults {
            model: Some("gpt-5-codex".into()),
            reasoning_effort: None,
        };
        assert_eq!(
            resolve_turn_selection(&card, Some(&defaults), None)
                .unwrap()
                .model,
            None,
            "we have never put a value on this thread, so omission IS the default"
        );
    }

    #[test]
    fn an_unreadable_default_refuses_the_turn() {
        let card =
            CardModelSelection::from_payload(&json!({ "model": null, "model_ever_set": true }))
                .unwrap();
        assert_eq!(
            resolve_turn_selection(&card, None, None),
            Err(UnresolvedSelection::Model)
        );
        assert_eq!(
            resolve_turn_selection(&card, Some(&InstallationDefaults::default()), None),
            Err(UnresolvedSelection::Model),
            "a successful read with no model in it is still no answer"
        );
    }

    #[test]
    fn effort_falls_through_config_then_catalog_then_refuses() {
        let card = CardModelSelection::from_payload(
            &json!({ "reasoning_effort": null, "reasoning_effort_ever_set": true }),
        )
        .unwrap();
        assert!(card.needs_catalog());

        let from_config = InstallationDefaults {
            model: None,
            reasoning_effort: Some("medium".into()),
        };
        assert_eq!(
            resolve_turn_selection(&card, Some(&from_config), Some("high"))
                .unwrap()
                .effort
                .as_deref(),
            Some("medium"),
            "the installation's own value outranks the model preset"
        );
        assert_eq!(
            resolve_turn_selection(&card, Some(&InstallationDefaults::default()), Some("high"))
                .unwrap()
                .effort
                .as_deref(),
            Some("high")
        );
        assert_eq!(
            resolve_turn_selection(&card, Some(&InstallationDefaults::default()), None),
            Err(UnresolvedSelection::Effort)
        );
    }

    #[test]
    fn an_unknown_effort_string_is_carried_through_untouched() {
        let card = CardModelSelection::from_payload(
            &json!({ "reasoning_effort": "ludicrous", "reasoning_effort_ever_set": true }),
        )
        .unwrap();
        assert_eq!(
            resolve_turn_selection(&card, None, None).unwrap().effort,
            Some("ludicrous".to_string()),
            "codex accepts any non-empty effort string; we must not filter to a set we invented"
        );
    }

    #[test]
    fn a_wrongly_typed_key_is_refused_rather_than_read_as_unset() {
        let err = CardModelSelection::from_payload(&json!({ "model": 42 })).unwrap_err();
        assert_eq!(err.key, PAYLOAD_MODEL);
        assert_eq!(err.found, "number");
        let err =
            CardModelSelection::from_payload(&json!({ "model_ever_set": "yes" })).unwrap_err();
        assert_eq!(err.key, PAYLOAD_MODEL_EVER_SET);
    }

    #[test]
    fn resetting_to_the_default_stores_null_and_keeps_the_marker() {
        let mut payload = serde_json::Map::new();
        CardModelSelection::apply_to_payload(&mut payload, Some("gpt-5"), Some("high"));
        let stored = Value::Object(payload.clone());
        assert_eq!(
            CardModelSelection::from_payload(&stored).unwrap(),
            CardModelSelection {
                model: Some("gpt-5".into()),
                model_ever_set: true,
                reasoning_effort: Some("high".into()),
                reasoning_effort_ever_set: true,
            }
        );

        CardModelSelection::apply_to_payload(&mut payload, None, None);
        assert_eq!(payload.get(PAYLOAD_MODEL), Some(&Value::Null));
        assert_eq!(payload.get(PAYLOAD_EFFORT), Some(&Value::Null));
        let stored = Value::Object(payload);
        let after = CardModelSelection::from_payload(&stored).unwrap();
        assert!(
            after.model_ever_set && after.reasoning_effort_ever_set,
            "the markers are monotone; clearing them would restore the drift bug"
        );
        assert!(after.needs_installation_defaults());
    }

    #[test]
    fn the_catalog_lookup_uses_the_model_that_will_actually_run() {
        let chosen = CardModelSelection {
            model: Some("gpt-5".into()),
            model_ever_set: true,
            ..Default::default()
        };
        let defaults = InstallationDefaults {
            model: Some("gpt-5-codex".into()),
            reasoning_effort: None,
        };
        assert_eq!(
            effective_model_for_catalog_lookup(&chosen, Some(&defaults)),
            Some("gpt-5")
        );
        let following = CardModelSelection {
            model: None,
            model_ever_set: true,
            ..Default::default()
        };
        assert_eq!(
            effective_model_for_catalog_lookup(&following, Some(&defaults)),
            Some("gpt-5-codex")
        );
    }
}
