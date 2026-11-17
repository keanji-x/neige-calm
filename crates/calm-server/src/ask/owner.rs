//! Conversation owner resolution for creation, answers and sweeps. No responder is persisted.
use super::*;
use calm_truth::decision_gate::WriteTx;
use calm_types::worker::WorkerContract;

pub(crate) async fn conversation_owner_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session: &WorkerSessionId,
) -> Result<(ActorId, EventScope)> {
    let live = tx
        .read_worker_session(session)
        .await?
        .filter(|row| row.state.is_active_authority())
        .ok_or_else(|| CalmError::Conflict("conversation ask session is no longer live".into()))?;
    let actor = match live.contract {
        WorkerContract::Planner => ActorId::AiPlannerSession(session.clone()),
        WorkerContract::Assistant => ActorId::AiManagedSession(session.clone()),
        _ => {
            return Err(CalmError::Forbidden(
                "session cannot own conversation ask".into(),
            ));
        }
    };
    let card = live
        .card_id
        .ok_or_else(|| CalmError::Conflict("conversation ask owner is missing".into()))?;
    let track = calm_truth::db::sqlite::track_get_tx(tx, &live.track_id).await?;
    Ok((
        actor,
        EventScope::Card {
            card,
            track: track.id,
            area: track.area_id,
        },
    ))
}

/// Re-read the exact source inside the answer transaction and reuse Truth's live
/// authorization. Only historical Planner Holds may carry Track scope.
pub(crate) async fn require_live_ask_tx(
    tx: &mut Transaction<'_, Sqlite>,
    ask_id: i64,
) -> Result<()> {
    if !super::is_open_tx(tx, ask_id).await? {
        return Err(CalmError::Conflict(
            "conversation ask is no longer open".into(),
        ));
    }
    let source = calm_truth::db::sqlite::event_inputs::event_input_tx(tx, ask_id)
        .await?
        .envelope;
    let session = match &source.actor {
        ActorId::AiPlannerSession(session) | ActorId::AiManagedSession(session) => session,
        _ => {
            return Err(CalmError::Conflict(
                "conversation ask has no session owner".into(),
            ));
        }
    };
    let (actor, scope) = conversation_owner_tx(tx, session).await?;
    let own_scope = source.scope == scope
        || matches!((&source.scope, &scope, &actor),
            (EventScope::Track { track: t, area: a }, EventScope::Card { track, area, .. }, ActorId::AiPlannerSession(_)) if t == track && a == area);
    if actor != source.actor || !own_scope {
        return Err(CalmError::Conflict("conversation ask owner changed".into()));
    }
    calm_truth::decision_gate::enforce_role_resolving_session_from_tx(
        tx,
        &actor,
        &source.event,
        &scope,
    )
    .await
    .map_err(|e| CalmError::Conflict(e.to_string()))
}

/// Source provenance shared by projection, live routing and durable replay.
/// Historical card actors have no original session; session actors never fall
/// back to a current Planner when their recorded binding is missing.
pub(crate) struct AskTarget {
    pub card: CardId,
    pub session: Option<WorkerSessionId>,
}
impl AskTarget {
    /// The one original-card/session rule for canonical admission and historical replay.
    pub(crate) fn is_for(&self, card: &CardId, session: &str) -> bool {
        self.card == *card
            && self
                .session
                .as_ref()
                .is_none_or(|id| id.as_str() == session)
    }
}
pub(crate) fn source_target(actor: &ActorId, session_card: Option<CardId>) -> Option<AskTarget> {
    match actor {
        ActorId::AiPlannerSession(id) | ActorId::AiManagedSession(id) => Some(AskTarget {
            card: session_card?,
            session: Some(id.clone()),
        }),
        ActorId::AiPlanner(card) => Some(AskTarget {
            card: card.clone(),
            session: None,
        }),
        _ => None,
    }
}
