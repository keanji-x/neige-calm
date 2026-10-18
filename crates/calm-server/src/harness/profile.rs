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
        if kind != "codex"
            || payload.get("handoff_pending_activation").is_some()
            || payload.get("planner_history").is_some()
        {
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

/// Immutable creation configuration for promoting an existing managed conversation.
/// The target owns model/effort and origin; the source owns validated working instructions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlannerHandoffConfiguration {
    pub provider: AgentProvider,
    pub model: crate::planner_model::CardModelSelection,
    pub template_context: Option<crate::template_context::TemplateContext>,
    pub payload: Value,
}

pub(crate) fn plan_planner_handoff(
    source: &Card,
    target: &Card,
) -> crate::error::Result<PlannerHandoffConfiguration> {
    let conflict = || {
        crate::error::CalmError::Conflict("handoff configuration or managed profile changed".into())
    };
    let from = PlannerBinding::from_card(source, CardRole::Planner).ok_or_else(conflict)?;
    let to = PlannerBinding::from_card(target, CardRole::Assistant).ok_or_else(conflict)?;
    if from.profile != HarnessProfile::Planner
        || to.profile != HarnessProfile::Assistant
        || from.provider != to.provider
    {
        return Err(conflict());
    }
    let model = crate::planner_model::CardModelSelection::from_payload(&target.payload)
        .map_err(|_| conflict())?;
    let template_context =
        crate::template_context::TemplateContext::from_card_payload(&source.payload)?;
    let mut payload = target.payload.clone();
    let map = payload.as_object_mut().ok_or_else(conflict)?;
    map.remove(ASSISTANT_PROVIDER_PAYLOAD_KEY);
    map.remove("harness_profile");
    map.insert("planner_harness".into(), Value::Bool(true));
    map.insert(
        PLANNER_PROVIDER_PAYLOAD_KEY.into(),
        serde_json::to_value(&from.provider)?,
    );
    match &template_context {
        Some(context) => {
            map.insert(
                crate::validation::PLANNER_TEMPLATE_CONTEXT_PAYLOAD_KEY.into(),
                serde_json::to_value(context)?,
            );
        }
        None => {
            map.remove(crate::validation::PLANNER_TEMPLATE_CONTEXT_PAYLOAD_KEY);
        }
    }
    crate::planner_permission_mode::mint_at_creation(map);
    Ok(PlannerHandoffConfiguration {
        provider: from.provider,
        model,
        template_context,
        payload,
    })
}

/// Resolve live row/card identity once for managed issuance and transport authorization.
async fn persisted_binding(
    repo: &dyn crate::db::RouteRepo,
    session: &calm_types::worker::WorkerSession,
) -> crate::error::Result<Option<PlannerBinding>> {
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
    Ok(binding_matches_session(
        &binding,
        &card,
        session,
        current.is_some_and(|current| {
            current.card_id == *card_id && current.track_id == session.track_id
        }),
    )
    .then_some(binding))
}

fn binding_matches_session(
    binding: &PlannerBinding,
    card: &Card,
    session: &calm_types::worker::WorkerSession,
    current: bool,
) -> bool {
    use calm_types::worker::{SessionMode, WorkerContract};
    let contract = match binding.profile {
        HarnessProfile::Planner => WorkerContract::Planner,
        HarnessProfile::Assistant => WorkerContract::Assistant,
        HarnessProfile::PlainChat => WorkerContract::Executor,
    };
    current
        && session.mode == SessionMode::Resumable
        && card.track_id == session.track_id
        && session.contract == contract
        && calm_types::worker::WorkerProviderKind::from(&binding.provider) == session.provider
}

/// Transactional variant of the live identity owner for internal pause commands.
pub(crate) async fn current_managed_binding_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session_id: &str,
    card_id: &crate::ids::CardId,
    track_id: &crate::ids::TrackId,
) -> crate::error::Result<bool> {
    use calm_types::worker::WorkerSessionId;
    let Some(session) =
        crate::db::sqlite::session_get_tx(tx, &WorkerSessionId(session_id.into())).await?
    else {
        return Ok(false);
    };
    if !session.state.is_active_authority()
        || session.card_id.as_ref() != Some(card_id)
        || session.track_id != *track_id
    {
        return Ok(false);
    }
    let row = sqlx::query_as::<_, crate::db::rows::CardRow>(concat!(
        "SELECT id, track_id, kind, sort, payload, title, deletable, created_at, updated_at FROM ",
        "cards WHERE id=?1 AND session_id=?2",
    ))
    .bind(card_id.as_str())
    .bind(session_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(card) = row.map(Card::from) else {
        return Ok(false);
    };
    let role: String = sqlx::query_scalar("SELECT role FROM cards WHERE id=?1")
        .bind(card_id.as_str())
        .fetch_one(&mut **tx)
        .await?;
    let role: CardRole = serde_json::from_value(Value::String(role))?;
    let Some(binding) = PlannerBinding::from_card(&card, role) else {
        return Ok(false);
    };
    Ok(matches!(
        binding.profile,
        HarnessProfile::Planner | HarnessProfile::Assistant
    ) && binding_matches_session(&binding, &card, &session, true))
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
    if card.payload.get("handoff_pending_activation").is_some() {
        return Err(crate::error::CalmError::Conflict(
            "Planner identity awaits explicit activation".into(),
        ));
    }
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

/// The backend of a Track's Planner. A child track's Planner, and a Track another Planner adds
/// with `neige_track_add`, runs on its creator's backend; a Planner card that is not a harness card
/// (`PlannerBinding` is the one decoder) refuses the create rather than guessing.
pub(crate) async fn track_planner_provider<'e, E: sqlx::SqliteExecutor<'e>>(
    executor: E,
    track_id: &str,
) -> crate::error::Result<AgentProvider> {
    let card = sqlx::query_as::<_, crate::db::rows::CardRow>(
        "SELECT id, track_id, kind, sort, payload, title, deletable, created_at, updated_at \
           FROM cards WHERE track_id=?1 AND role='planner'",
    )
    .bind(track_id)
    .fetch_optional(executor)
    .await?
    .map(crate::model::Card::from);
    card.as_ref()
        .and_then(|card| {
            crate::harness::profile::PlannerBinding::from_card(card, CardRole::Planner)
        })
        .map(|binding| binding.provider)
        .ok_or_else(|| {
            crate::error::CalmError::Conflict(format!(
                "track {track_id} has no Planner card naming its planner_provider"
            ))
        })
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
