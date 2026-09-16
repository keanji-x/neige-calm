//! Conversation identity and capabilities, shared by creation and recovery.
use crate::model::{Card, CardRole};
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
    pub(crate) fn from_card(card: &Card, role: CardRole) -> Option<Self> {
        Self::from_shape(&card.kind, role, &card.payload)
    }

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
