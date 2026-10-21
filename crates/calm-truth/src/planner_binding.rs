//! Persisted conversation binding shared by creation, authorization and replacement.
use crate::model::{Card, CardRole};
use crate::validation::{ASSISTANT_PROVIDER_PAYLOAD_KEY, PLANNER_PROVIDER_PAYLOAD_KEY};
use calm_types::{harness::HarnessProfile, runtime::AgentProvider};
use serde_json::Value;
pub const PLAIN_CHAT_MARKER: &str = "plain_chat";
pub const ASSISTANT_MARKER: &str = "assistant";
/// A harness card's identity at the construction boundary: what it is and which backend runs it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannerBinding {
    pub profile: HarnessProfile,
    pub provider: AgentProvider,
}

impl PlannerBinding {
    /// The row's declared contract must agree with the card's binding. Current
    /// card/track identity and active ownership are checked by the caller.
    pub fn matches_session_contract(&self, session: &calm_types::worker::WorkerSession) -> bool {
        use calm_types::worker::{SessionMode, WorkerContract, WorkerProviderKind};
        let contract = match self.profile {
            HarnessProfile::Planner => WorkerContract::Planner,
            HarnessProfile::Assistant => WorkerContract::Assistant,
            HarnessProfile::PlainChat => WorkerContract::Executor,
        };
        session.mode == SessionMode::Resumable
            && session.contract == contract
            && WorkerProviderKind::from(&self.provider) == session.provider
    }

    /// A Planner card names its backend in the server-owned `planner_provider` key; missing or
    /// unknown makes it not a harness card (fail closed). Assistant names its own backend; PlainChat is Codex.
    pub fn from_card(card: &Card, role: CardRole) -> Option<Self> {
        Self::from_shape(&card.kind, role, &card.payload)
    }

    pub fn from_shape(kind: &str, role: CardRole, payload: &Value) -> Option<Self> {
        let profile = profile_from_shape(kind, role, payload)?;
        let provider = match profile {
            HarnessProfile::Planner => {
                let stored = payload.get(PLANNER_PROVIDER_PAYLOAD_KEY)?;
                serde_json::from_value(stored.clone()).ok()?
            }
            HarnessProfile::Assistant => {
                serde_json::from_value(payload.get(ASSISTANT_PROVIDER_PAYLOAD_KEY)?.clone()).ok()?
            }
            HarnessProfile::PlainChat => AgentProvider::Codex,
        };
        Some(Self { profile, provider })
    }
}
pub fn profile_from_shape(kind: &str, role: CardRole, payload: &Value) -> Option<HarnessProfile> {
    if kind != "codex" {
        return None;
    }
    match role {
        CardRole::Planner => Some(HarnessProfile::Planner),
        CardRole::Worker
            if payload.get("harness_profile").and_then(Value::as_str)
                == Some(PLAIN_CHAT_MARKER) =>
        {
            Some(HarnessProfile::PlainChat)
        }
        CardRole::Assistant
            if payload.get("harness_profile").and_then(Value::as_str) == Some(ASSISTANT_MARKER) =>
        {
            Some(HarnessProfile::Assistant)
        }
        _ => None,
    }
}
