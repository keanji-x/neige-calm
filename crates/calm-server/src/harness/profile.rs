//! Conversation identity and capabilities, shared by creation and recovery.
use crate::model::{Card, CardRole};
use crate::session_projection_repo::AgentProvider;
use crate::validation::PLANNER_PROVIDER_PAYLOAD_KEY;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const PLAIN_CHAT_MARKER: &str = "plain_chat";
pub(crate) const ASSISTANT_MARKER: &str = "assistant";

/// Serialized names are persisted in start operations and remain unchanged.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessProfile {
    #[default]
    #[serde(rename = "spec")]
    Planner,
    PlainChat,
    Assistant,
}

impl HarnessProfile {
    pub(crate) fn from_shape(kind: &str, role: CardRole, payload: &Value) -> Option<Self> {
        if kind != "codex" {
            return None;
        }
        match role {
            CardRole::Planner => Some(Self::Planner),
            CardRole::Worker
                if payload.get("harness_profile").and_then(Value::as_str)
                    == Some(PLAIN_CHAT_MARKER) =>
            {
                Some(Self::PlainChat)
            }
            CardRole::Assistant
                if payload.get("harness_profile").and_then(Value::as_str)
                    == Some(ASSISTANT_MARKER) =>
            {
                Some(Self::Assistant)
            }
            _ => None,
        }
    }

    /// None is an explicit no-MCP capability, never a missing role/default.
    pub(crate) fn mcp_role(self) -> Option<CardRole> {
        match self {
            Self::Planner => Some(CardRole::Planner),
            Self::Assistant => Some(CardRole::Assistant),
            Self::PlainChat => None,
        }
    }
}

/// A harness card's identity at the construction boundary: what it is and which backend runs it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlannerBinding {
    pub(crate) profile: HarnessProfile,
    pub(crate) provider: AgentProvider,
}

impl PlannerBinding {
    /// A Planner card names its backend in the server-owned `planner_provider` key; missing or
    /// unknown makes it not a harness card (fail closed). PlainChat and Assistant run on Codex.
    pub(crate) fn from_card(card: &Card, role: CardRole) -> Option<Self> {
        Self::from_shape(&card.kind, role, &card.payload)
    }

    pub(crate) fn from_shape(kind: &str, role: CardRole, payload: &Value) -> Option<Self> {
        let profile = HarnessProfile::from_shape(kind, role, payload)?;
        let provider = match profile {
            HarnessProfile::Planner => {
                let stored = payload.get(PLANNER_PROVIDER_PAYLOAD_KEY)?;
                serde_json::from_value(stored.clone()).ok()?
            }
            HarnessProfile::PlainChat | HarnessProfile::Assistant => AgentProvider::Codex,
        };
        Some(Self { profile, provider })
    }
}

/// The backend a recovered Planner row runs on: its own provider, and only when the card's
/// persisted role is Planner and its `planner_provider` names that same provider (#1791 §4.4 Boot).
/// Anything else (a mismatched row, a card naming another or no backend, a card that is not a
/// Planner) is not recovered.
pub(crate) fn planner_row_provider(
    row_provider: Option<&AgentProvider>,
    card: &Card,
    role: Option<CardRole>,
) -> Option<AgentProvider> {
    let row_provider = row_provider?;
    (role == Some(CardRole::Planner)
        && PlannerBinding::from_card(card, CardRole::Planner)
            .is_some_and(|binding| &binding.provider == row_provider))
    .then(|| row_provider.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn card(kind: &str, payload: Value) -> Card {
        Card {
            id: "card".into(),
            track_id: "track".into(),
            kind: kind.into(),
            title: None,
            sort: 0.0,
            payload,
            runtime: None,
            deletable: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn planner(provider: Option<Value>) -> Card {
        let mut payload = json!({"schemaVersion": 1, "planner_harness": true});
        if let Some(provider) = provider {
            payload[PLANNER_PROVIDER_PAYLOAD_KEY] = provider;
        }
        card("codex", payload)
    }

    #[test]
    fn a_planner_card_binds_the_provider_its_key_names() {
        for (value, provider) in [
            ("codex", AgentProvider::Codex),
            ("claude", AgentProvider::Claude),
        ] {
            assert_eq!(
                PlannerBinding::from_card(&planner(Some(json!(value))), CardRole::Planner),
                Some(PlannerBinding {
                    profile: HarnessProfile::Planner,
                    provider
                })
            );
        }
    }

    #[test]
    fn a_planner_card_without_a_known_provider_is_not_a_harness_card() {
        for provider in [
            None,
            Some(json!("gpt")),
            Some(json!("Codex")),
            Some(json!(true)),
            Some(Value::Null),
            Some(json!({"codex": {}})),
        ] {
            assert_eq!(
                PlannerBinding::from_card(&planner(provider.clone()), CardRole::Planner),
                None,
                "{provider:?}"
            );
        }
    }

    #[test]
    fn conversations_bind_codex_without_a_provider_key() {
        for (role, marker, profile) in [
            (
                CardRole::Worker,
                PLAIN_CHAT_MARKER,
                HarnessProfile::PlainChat,
            ),
            (
                CardRole::Assistant,
                ASSISTANT_MARKER,
                HarnessProfile::Assistant,
            ),
        ] {
            let card = card(
                "codex",
                json!({"schemaVersion": 1, "harness_profile": marker}),
            );
            assert_eq!(
                PlannerBinding::from_card(&card, role),
                Some(PlannerBinding {
                    profile,
                    provider: AgentProvider::Codex
                })
            );
        }
    }
}
