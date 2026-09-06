//! #1505 S4-3 — which model a planner conversation's turns run with.
//!
//! Two things live here, and they are deliberately in one module because the
//! REST write port and the turn-issuing loop must agree on them exactly:
//!
//!  * [`CardModelSelection`] — the four keys this feature owns on
//!    `cards.payload_json`, and how they are read and written.
//!  * [`resolve_turn_selection`] — the pure rule that turns a card's stored
//!    selection into the `model` / `effort` members of a `turn/start` frame.
//!
//! ## Why `null` is not always "send nothing"
//!
//! Codex's `turn/start` model override is *sticky*: it applies to "this turn
//! and subsequent turns" (`v2/turn.rs`, `TurnStartParams::model`). A value we
//! sent once stays on the thread until something replaces it, and it is
//! replayed into the rollout's `TurnContextItem` so even a resume brings it
//! back.
//!
//! So "the person set X, sent a message, then chose *follow the default*
//! again" cannot be honoured by going quiet: the thread would keep running X
//! while the UI said "default". The only way back is to send the default
//! *explicitly*.
//!
//! But sending it explicitly costs a `config/read` round trip per turn, and
//! for the overwhelming majority of cards — the ones nobody ever touched the
//! picker on — it buys nothing, because we have never put a sticky value on
//! that thread in the first place.
//!
//! Hence the monotone marker: [`CardModelSelection::model_ever_set`] is
//! flipped true the first time a non-null model is stored and is **never**
//! cleared. It is exactly the predicate "this thread may be carrying a value
//! of ours", and it partitions `null` into the two cases above.
//!
//! Clearing it on a reset-to-default would reintroduce the bug it exists to
//! close, which is why nothing in this module can clear it.
//!
//! ## Failure is closed
//!
//! Every path that cannot determine what to send refuses to send the turn. A
//! conversation running under a model the person did not choose is worse than
//! one that has stopped: the first quietly produces work under the wrong
//! assumptions and bills for it. `resolve_turn_selection` therefore returns
//! [`UnresolvedSelection`] rather than falling back to omission — omission is
//! precisely the defect described above.
//!
//! Refusing is only half of it, and the half that is easy to get wrong. "The
//! second is obvious and recoverable" was written of `HarnessState::Wedged`
//! and stopped being true when the wedge was removed: a refusal that merely
//! retries is neither obvious nor self-recovering when the condition cannot
//! clear itself. So a refusal now says which kind it is —
//! [`UnresolvedSelection::clears_itself`] — and the run loop pairs a retry
//! with a reader-visible reason accordingly. Neither half stands alone: a
//! silent refusal is a conversation that looks healthy and never answers.

use serde_json::Value;

/// `cards.payload_json` key: the chosen model **slug**, or JSON `null` for
/// "follow the installation default".
///
/// A slug, never a preset id — see `CodexModel`'s doc, which owns that rule.
pub const PAYLOAD_MODEL: &str = "model";
/// `cards.payload_json` key: monotone "a model has been chosen on this card at
/// least once". See the module header; nothing clears it.
pub const PAYLOAD_MODEL_EVER_SET: &str = "model_ever_set";
/// `cards.payload_json` key: the chosen reasoning effort, or JSON `null`.
pub const PAYLOAD_EFFORT: &str = "reasoning_effort";
/// `cards.payload_json` key: the monotone marker for the effort, exactly
/// parallel to [`PAYLOAD_MODEL_EVER_SET`].
pub const PAYLOAD_EFFORT_EVER_SET: &str = "reasoning_effort_ever_set";

/// What a card's payload says about the model its turns run with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardModelSelection {
    /// The chosen slug, or `None` for "follow the default".
    pub model: Option<String>,
    /// See [`PAYLOAD_MODEL_EVER_SET`].
    pub model_ever_set: bool,
    /// The chosen reasoning effort, or `None` for "follow the default".
    ///
    /// A bare `String`, never a closed enum: codex's `ReasoningEffort` carries
    /// a `Custom(String)` variant and accepts any non-empty string on the
    /// wire, so a closed set here would start rejecting values the day codex
    /// ships a new one.
    pub reasoning_effort: Option<String>,
    /// See [`PAYLOAD_EFFORT_EVER_SET`].
    pub reasoning_effort_ever_set: bool,
}

/// A payload whose model keys are present but not of the type they must be.
///
/// This is its own outcome rather than "treat it as unset" on purpose: a
/// `model` holding `42` means somebody wrote something we do not understand
/// into the field that decides what the person is billed for, and guessing is
/// how a wrong model gets used silently.
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
    /// Read the four keys off a card payload.
    ///
    /// An absent key reads as "never set", which is what every card that
    /// predates this feature holds. A key of the wrong type is refused — see
    /// [`MalformedSelection`].
    pub fn from_payload(payload: &Value) -> Result<Self, MalformedSelection> {
        Ok(Self {
            model: read_nullable_string(payload, PAYLOAD_MODEL)?,
            model_ever_set: read_marker(payload, PAYLOAD_MODEL_EVER_SET)?,
            reasoning_effort: read_nullable_string(payload, PAYLOAD_EFFORT)?,
            reasoning_effort_ever_set: read_marker(payload, PAYLOAD_EFFORT_EVER_SET)?,
        })
    }

    /// Write a new selection into `payload`, in place.
    ///
    /// The `*_ever_set` markers only ever go up. Both value keys are written
    /// unconditionally — including as JSON `null` — so that "follow the
    /// default" is a stored fact rather than the absence of one, which is the
    /// same distinction the markers exist to preserve.
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

    /// Whether resolving this selection needs the installation's defaults read
    /// from codex.
    ///
    /// True only in the "chose something once, then chose the default again"
    /// case. Every other card resolves from the payload alone, which is why
    /// the common path costs no extra RPC.
    pub fn needs_installation_defaults(&self) -> bool {
        (self.model.is_none() && self.model_ever_set)
            || (self.reasoning_effort.is_none() && self.reasoning_effort_ever_set)
    }

    /// Whether resolving this selection needs the model catalog.
    ///
    /// Only the effort can want it, and only as the last step of its chain —
    /// see [`resolve_turn_selection`].
    pub fn needs_catalog(&self) -> bool {
        self.reasoning_effort.is_none() && self.reasoning_effort_ever_set
    }
}

/// The installation defaults, as codex's layer-merged `config/read` reports
/// them for the thread's workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallationDefaults {
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}

/// What a `turn/start` frame must say about the model.
///
/// `None` means **the key is not put on the frame at all**, which is a real
/// and distinct wire state from any value: it leaves whatever the thread
/// already carries alone. It is not "unset" standing in for a missing
/// required field — every caller has to produce a `TurnModelSelection`, and
/// [`TurnModelSelection::inherit`] is how a caller says "nothing to say"
/// out loud.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnModelSelection {
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl TurnModelSelection {
    /// Send neither key: let the thread keep whatever it has.
    ///
    /// Correct only where we have never put a sticky value on the thread —
    /// every non-planner `turn/start` caller in this kernel, and planner cards
    /// whose picker has never been touched.
    pub fn inherit() -> Self {
        Self::default()
    }
}

/// Which half of the selection could not be determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnresolvedSelection {
    /// The card follows the default model, has carried an explicit one before,
    /// and we could not learn what the default is.
    Model,
    /// The same, for the reasoning effort.
    Effort,
}

/// Whether waiting is a plan.
///
/// The first cut of #1505 S4 wedged on every refusal, and `HarnessState::Wedged`
/// has no exit — so a codex restart ended the conversation for good. The fix
/// replaced the wedge with a retry, and by treating all refusals as transient
/// it traded a lying message for no message at all: a card whose config names
/// no model retried forever, invisibly, with the person's sentence sitting in
/// the queue looking perfectly healthy.
///
/// Both were wrong in the same way — they answered "can this fix itself?" with
/// a constant. It is a property of the individual failure, so it is carried
/// here and the run loop reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The attempt may succeed if repeated unchanged, and nobody has anything
    /// to do meanwhile.
    ///
    /// Covers every way asking codex can fail short of codex answering — no
    /// connection, a closed socket, a timeout — and every local read that can
    /// fail transiently. It does NOT promise the turn eventually goes out: the
    /// card may have been deleted, in which case retrying is simply cheap and
    /// harmless. What it promises is only that repeating the attempt is a
    /// sensible thing to do and that no message to the reader is owed yet.
    Retryable,
    /// Codex answered, and its answer was a refusal of this input.
    ///
    /// Repeating it unchanged reproduces the refusal, so this is NOT
    /// `Retryable` however transient the underlying cause might be — and the
    /// reader must not be told the message is on its way. `PUT
    /// /planner/model` stores a slug codex has never heard of by design, so
    /// this is reachable from the picker.
    Rejected,
    /// The selection itself cannot be determined, so there is nothing to send
    /// yet.
    ///
    /// `config.model = null` is an explicitly supported codex state and a
    /// malformed payload is reachable too — **neither clears itself.** Waiting
    /// is not a plan; a person has to choose.
    NeedsAChoice,
}

impl UnresolvedSelection {
    /// Why the turn was not sent.
    ///
    /// Shown to the reader when [`Self::kind`] is [`FailureKind::NeedsAChoice`],
    /// because then it is the only thing standing between them and a
    /// conversation that never answers. It therefore names the one action that
    /// works — choosing a model — and nothing else. The first cut's sentence
    /// also offered "retry once codex is reachable", which was false twice
    /// over: the state it was written into could not be left, and this
    /// particular failure does not depend on codex being reachable.
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

/// Turn a stored selection into the frame members for one `turn/start`.
///
/// `defaults` is what `config/read` reported, or `None` when it was not read
/// (which a caller may only do when [`CardModelSelection::needs_installation_defaults`]
/// is false, or when the read failed). `catalog_default_effort` is the
/// `defaultReasoningEffort` the catalog gives for the model that will actually
/// run, or `None` when the catalog was not consulted or did not have it.
///
/// The model chain is: the card's own slug → the installation default →
/// refuse.
///
/// The effort chain is: the card's own effort → the installation default →
/// the running model's own preset default → refuse. The third step exists
/// because `TurnStartParams::effort` is a single-level `Option` and therefore
/// cannot express "clear the effort" — with no value to send there is no way
/// to undo a sticky one, so the preset default is the closest honest answer,
/// and it is codex's own number rather than one we invented.
pub fn resolve_turn_selection(
    card: &CardModelSelection,
    defaults: Option<&InstallationDefaults>,
    catalog_default_effort: Option<&str>,
) -> Result<TurnModelSelection, UnresolvedSelection> {
    // Every refusal reachable from here is a `NeedsAChoice`: the caller only
    // passes `defaults` once codex has ANSWERED, so arriving here means the
    // answer did not name a model. A codex that could not be asked never gets
    // this far — see `resolve_model_selection`.
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

/// The slug whose catalog entry decides the effort fallback: the card's own
/// choice if it has one, else the installation default.
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
