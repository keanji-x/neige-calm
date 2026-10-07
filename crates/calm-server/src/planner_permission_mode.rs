//! A Planner card's permission mode (#2348): which cards carry one, the value every Planner card is
//! created with, and how a stored value is read. Nothing consumes the mode yet; the provider
//! adapters will. The only writer of a stored value is `PUT /api/cards/{id}/planner/permission-mode`.

use serde_json::{Map, Value};

pub use calm_types::harness::PlannerPermissionMode;

use crate::harness::profile::{HarnessProfile, PlannerBinding};
use crate::model::{Card, CardRole};
use crate::validation::PLANNER_PERMISSION_MODE_PAYLOAD_KEY;

/// The mode every Planner card is created with, whoever creates it and whatever it asked for: a
/// Planner starts with no more permission than a person has given it.
pub const PLANNER_PERMISSION_MODE_AT_CREATION: PlannerPermissionMode = PlannerPermissionMode::Never;

/// Stamp the creation mode into a new Planner card's payload, replacing anything already there.
/// Every Planner-card creation path builds its payload through this.
pub(crate) fn mint_at_creation(payload: &mut Map<String, Value>) {
    payload.insert(
        PLANNER_PERMISSION_MODE_PAYLOAD_KEY.to_owned(),
        serde_json::json!(PLANNER_PERMISSION_MODE_AT_CREATION),
    );
}

/// Whether a card has a permission mode: exactly the cards [`PlannerBinding::from_card`] binds as
/// a Planner. PlainChat and Assistant conversations never ask.
pub(crate) fn card_has_permission_mode(card: &Card, role: CardRole) -> bool {
    PlannerBinding::from_card(card, role)
        .is_some_and(|binding| binding.profile == HarnessProfile::Planner)
}

/// A stored `permission_mode` that is not one of the modes, or no stored value at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedPermissionMode {
    /// What the payload holds under the key; `None` when the key is absent.
    pub found: Option<Value>,
}

impl std::fmt::Display for MalformedPermissionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.found {
            None => write!(
                f,
                "card payload has no `{PLANNER_PERMISSION_MODE_PAYLOAD_KEY}`"
            ),
            Some(value) => write!(
                f,
                "card payload key `{PLANNER_PERMISSION_MODE_PAYLOAD_KEY}` is {value}, which is not a permission mode"
            ),
        }
    }
}

/// Read a card payload's stored mode. An absent key is malformed too: every Planner card is
/// created with a mode, and the backfill migration stamped the ones created before it.
pub fn read(payload: &Value) -> Result<PlannerPermissionMode, MalformedPermissionMode> {
    let stored = payload
        .get(PLANNER_PERMISSION_MODE_PAYLOAD_KEY)
        .ok_or(MalformedPermissionMode { found: None })?;
    serde_json::from_value(stored.clone()).map_err(|_| MalformedPermissionMode {
        found: Some(stored.clone()),
    })
}

/// The mode a Planner card shows: its stored value, or `never` when that cannot be read. Fails
/// closed, so an unreadable value never reads as permission to ask; the stored value is kept as
/// it is (it is sticky), and choosing a mode overwrites it.
pub(crate) fn shown_for_planner(card: &Card) -> PlannerPermissionMode {
    read(&card.payload).unwrap_or_else(|malformed| {
        tracing::warn!(
            card_id = %card.id,
            error = %malformed,
            "planner permission mode unreadable; shown as never"
        );
        PlannerPermissionMode::Never
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_stored_mode_reads_back_and_anything_else_is_malformed() {
        assert_eq!(
            read(&json!({"permission_mode": "never"})),
            Ok(PlannerPermissionMode::Never)
        );
        assert_eq!(
            read(&json!({"permission_mode": "ask"})),
            Ok(PlannerPermissionMode::Ask)
        );
        assert_eq!(
            read(&json!({})),
            Err(MalformedPermissionMode { found: None })
        );
        for corrupt in [
            json!("full"),
            json!("Ask"),
            json!(null),
            json!(true),
            json!({}),
        ] {
            assert_eq!(
                read(&json!({"permission_mode": corrupt.clone()})),
                Err(MalformedPermissionMode {
                    found: Some(corrupt)
                })
            );
        }
    }

    #[test]
    fn creation_overwrites_whatever_the_payload_already_said() {
        let mut payload = Map::new();
        payload.insert("permission_mode".into(), json!("ask"));
        mint_at_creation(&mut payload);
        assert_eq!(payload["permission_mode"], json!("never"));
    }

    #[test]
    fn the_shared_planner_payload_mints_never_for_every_provider() {
        use crate::session_projection_repo::AgentProvider;
        for provider in [AgentProvider::Codex, AgentProvider::Claude] {
            let payload =
                crate::routes::tracks::planner_harness_card_payload(Some("goal".into()), provider);
            assert_eq!(payload["permission_mode"], json!("never"), "{payload}");
        }
    }

    fn card(payload: Value) -> Card {
        Card {
            id: "card".into(),
            track_id: "track".into(),
            kind: "codex".into(),
            title: None,
            sort: 0.0,
            payload,
            runtime: None,
            deletable: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn only_a_planner_conversation_has_a_permission_mode() {
        let planner = card(json!({"planner_provider": "claude"}));
        assert!(card_has_permission_mode(&planner, CardRole::Planner));
        let assistant = card(json!({"harness_profile": "assistant"}));
        assert!(!card_has_permission_mode(&assistant, CardRole::Assistant));
        let plain_chat = card(json!({"harness_profile": "plain_chat"}));
        assert!(!card_has_permission_mode(&plain_chat, CardRole::Worker));
        let unbound = card(json!({"planner_provider": "gpt"}));
        assert!(!card_has_permission_mode(&unbound, CardRole::Planner));
    }
}

#[cfg(test)]
mod migration_tests;
