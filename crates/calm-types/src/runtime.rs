//! Runtime projection vocabulary: the TS-exported wire types of calm-server's session projection.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use utoipa::ToSchema;

use crate::worker::WorkerSessionState;

pub type TimestampMs = i64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum WorkerSessionKind {
    #[serde(rename = "terminal")]
    Terminal,
    #[serde(rename = "codex")]
    CodexCard,
    #[serde(rename = "claude")]
    ClaudeCard,
    #[serde(rename = "shared-spec")]
    SharedPlanner,
    #[serde(rename = "managed-assistant")]
    ManagedAssistant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum AgentProvider {
    #[serde(rename = "opencode")]
    OpenCode,
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "claude")]
    Claude,
}

/// The protocol a provider's managed backend speaks. An intentional closed set owned here: each
/// variant has exactly one protocol implementation, and consumers branch on it rather than on a
/// provider's identity, so a provider speaking an existing protocol needs no new branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentProtocol {
    Codex,
    Claude,
    Acp,
}

impl AgentProtocol {
    /// The providers speaking this protocol, in [`AgentProvider::ALL`] order.
    pub fn providers(self) -> impl Iterator<Item = AgentProvider> {
        AgentProvider::ALL
            .into_iter()
            .filter(move |provider| provider.protocol() == self)
    }
}

impl AgentProvider {
    /// Every provider. A new variant is added here too: the exhaustive matches below do not see
    /// this list, so a test checks it against the enum's derived schema.
    pub const ALL: [Self; 3] = [Self::Codex, Self::Claude, Self::OpenCode];

    /// The protocol this provider's managed backend speaks.
    pub fn protocol(&self) -> AgentProtocol {
        match self {
            Self::Codex => AgentProtocol::Codex,
            Self::Claude => AgentProtocol::Claude,
            Self::OpenCode => AgentProtocol::Acp,
        }
    }

    /// The provider's name on the wire, as its serde form spells it.
    pub fn wire_name(&self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::OpenCode => "opencode",
        }
    }
}

/// `GET /api/claude/installed-version` (#2516): the first token the server's last
/// `claude --version` run printed; null until a run has answered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ClaudeInstalledVersion {
    #[schema(required = true)]
    pub version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct WorkerSessionProjection {
    pub id: String,
    pub card_id: String,
    pub kind: WorkerSessionKind,
    pub agent_provider: Option<AgentProvider>,
    pub status: WorkerSessionState,
    pub terminal_run_id: Option<String>,
    pub thread_id: Option<String>,
    pub session_id: Option<String>,
    pub active_turn_id: Option<String>,
    #[ts(type = "unknown | null")]
    pub handle_state_json: Option<Value>,
    pub created_at_ms: TimestampMs,
    pub updated_at_ms: TimestampMs,
    pub completed_at_ms: Option<TimestampMs>,
    /// When the card's last non-interrupted turn ended; `None` when the card has no completed turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_turn_completed_ms: Option<TimestampMs>,
}

#[cfg(test)]
mod tests {
    use super::{AgentProtocol, AgentProvider};

    #[test]
    fn agent_provider_protocol_maps_each_provider() {
        assert_eq!(AgentProvider::Codex.protocol(), AgentProtocol::Codex);
        assert_eq!(AgentProvider::Claude.protocol(), AgentProtocol::Claude);
        assert_eq!(AgentProvider::OpenCode.protocol(), AgentProtocol::Acp);
        assert!(
            AgentProtocol::Acp
                .providers()
                .any(|p| p == AgentProvider::OpenCode)
        );
    }

    /// `ALL` is a hand-written list, so it is checked against the variant set the derived
    /// OpenAPI schema enumerates from the enum itself: a variant missing from `ALL` fails here.
    #[test]
    fn agent_provider_all_lists_every_variant_by_its_wire_name() {
        let schema =
            serde_json::to_value(<AgentProvider as utoipa::PartialSchema>::schema()).unwrap();
        let names = schema["enum"]
            .as_array()
            .expect("AgentProvider schema enum");
        assert_eq!(names.len(), AgentProvider::ALL.len(), "{schema}");
        for name in names {
            let provider: AgentProvider = serde_json::from_value(name.clone()).unwrap();
            assert!(AgentProvider::ALL.contains(&provider), "{provider:?}");
            assert_eq!(provider.wire_name(), name, "{provider:?}");
        }
    }
}
