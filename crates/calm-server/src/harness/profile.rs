//! Conversation identity and capabilities, shared by creation and recovery.
use crate::model::{Card, CardRole};
use crate::session_projection_repo::AgentProvider;
use crate::validation::{ASSISTANT_PROVIDER_PAYLOAD_KEY, PLANNER_PROVIDER_PAYLOAD_KEY};
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

    pub(crate) fn session_kind(self) -> crate::session_projection_repo::WorkerSessionKind {
        use crate::session_projection_repo::WorkerSessionKind;
        match self {
            Self::Planner => WorkerSessionKind::SharedPlanner,
            Self::Assistant => WorkerSessionKind::ManagedAssistant,
            Self::PlainChat => WorkerSessionKind::CodexCard,
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
pub struct PlannerBinding {
    pub profile: HarnessProfile,
    pub provider: AgentProvider,
}

impl PlannerBinding {
    /// A Planner card names its backend in the server-owned `planner_provider` key; missing or
    /// unknown makes it not a harness card (fail closed). Assistant names its own backend; PlainChat is Codex.
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
            HarnessProfile::Assistant => {
                serde_json::from_value(payload.get(ASSISTANT_PROVIDER_PAYLOAD_KEY)?.clone()).ok()?
            }
            HarnessProfile::PlainChat => AgentProvider::Codex,
        };
        if profile == HarnessProfile::Assistant
            && !crate::harness::backend::PlannerBackend::metadata(&provider)
                .capabilities
                .managed_conversation
        {
            return None;
        }
        Some(Self { profile, provider })
    }
}

/// Resolve live row/card identity once for managed issuance and transport authorization.
async fn persisted_binding(
    repo: &dyn crate::db::RouteRepo,
    session: &calm_types::worker::WorkerSession,
) -> crate::error::Result<Option<PlannerBinding>> {
    use calm_types::worker::{SessionMode, WorkerContract};
    let Some(card_id) = session.card_id.as_ref() else {
        return Ok(None);
    };
    let Some(card) = repo.card_get(card_id.as_str()).await? else {
        return Ok(None);
    };
    let role = repo.card_role_get(card_id.as_str()).await?;
    let Some(binding) = role.and_then(|role| PlannerBinding::from_card(&card, role)) else {
        return Ok(None);
    };
    let current = repo
        .card_identity_get_by_session(session.id.as_str())
        .await?;
    let contract = match binding.profile {
        HarnessProfile::Planner => WorkerContract::Planner,
        HarnessProfile::Assistant => WorkerContract::Assistant,
        HarnessProfile::PlainChat => WorkerContract::Executor,
    };
    Ok((session.mode == SessionMode::Resumable
        && card.track_id == session.track_id
        && session.contract == contract
        && calm_types::worker::WorkerProviderKind::from(&binding.provider) == session.provider
        && current.is_some_and(|current| {
            current.card_id == *card_id && current.track_id == session.track_id
        }))
    .then_some(binding))
}

pub(crate) async fn live_binding(
    repo: &dyn crate::db::RouteRepo,
    session: &calm_types::worker::WorkerSession,
) -> crate::error::Result<Option<PlannerBinding>> {
    if !session.state.is_active_authority() {
        return Ok(None);
    }
    persisted_binding(repo, session).await
}

/// Resolve a declared card backend, including dormant cards, and reject a conflicting current row.
pub(crate) async fn checked_card_binding(
    repo: &dyn crate::db::RouteRepo,
    card: &Card,
    role: CardRole,
) -> crate::error::Result<PlannerBinding> {
    let binding = PlannerBinding::from_card(card, role).ok_or_else(|| {
        crate::error::CalmError::Forbidden("The conversation provider binding is invalid".into())
    })?;
    if let Some(runtime) = repo
        .session_projection_projectable_for_card(&card.id.to_string())
        .await?
    {
        let row = repo
            .session_get_by_id(&calm_types::worker::WorkerSessionId(runtime.id))
            .await?
            .ok_or_else(|| {
                crate::error::CalmError::Forbidden("The conversation session is missing".into())
            })?;
        if persisted_binding(repo, &row).await?.as_ref() != Some(&binding) {
            return Err(crate::error::CalmError::Forbidden(
                "The conversation provider and current session disagree".into(),
            ));
        }
    }
    Ok(binding)
}

pub(crate) async fn validate_live_assistant(
    repo: &dyn crate::db::RouteRepo,
    session: &calm_types::worker::WorkerSession,
) -> crate::error::Result<bool> {
    Ok(live_binding(repo, session)
        .await?
        .is_some_and(|binding| binding.profile == HarnessProfile::Assistant))
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
    fn assistant_provider_binding_requires_identity_while_plain_chat_stays_codex() {
        let plain = card("codex", json!({"harness_profile":"plain_chat"}));
        assert_eq!(
            PlannerBinding::from_card(&plain, CardRole::Worker)
                .unwrap()
                .provider,
            AgentProvider::Codex
        );
        for provider in crate::harness::backend::PlannerBackend::registered_providers() {
            let assistant = card(
                "codex",
                json!({"harness_profile":"assistant", "assistant_provider":provider}),
            );
            assert_eq!(
                PlannerBinding::from_card(&assistant, CardRole::Assistant),
                Some(PlannerBinding {
                    profile: HarnessProfile::Assistant,
                    provider
                })
            );
        }
        for bad in [
            json!({"harness_profile":"assistant"}),
            json!({"harness_profile":"assistant","assistant_provider":"unknown"}),
        ] {
            assert!(PlannerBinding::from_card(&card("codex", bad), CardRole::Assistant).is_none());
        }
    }
}
