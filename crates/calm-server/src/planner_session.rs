//! Authoritative Planner card and active historical harness lookup.
use crate::db::Repo;
use crate::error::Result;
use crate::ids::{CardId, TrackId};
use crate::model::CardRole;
use crate::session_projection_repo::{WorkerSessionKind, WorkerSessionProjection};
use crate::state::WriteContext;

pub(crate) async fn resolve_card(
    repo: &dyn Repo,
    write: &WriteContext,
    track: &TrackId,
) -> Result<Option<CardId>> {
    let cards = repo.cards_by_track(track.as_str()).await?;
    let roles = write.roles_snapshot(cards.iter().map(|card| &card.id));
    let Some(card) = cards
        .into_iter()
        .find(|card| roles.get(&card.id) == Some(&CardRole::Planner))
    else {
        return Ok(None);
    };
    Ok(Some(card.id))
}

/// Hydrate only for callers that consume the historical harness contract.
pub(crate) async fn historical_session(
    repo: &dyn Repo,
    card: &CardId,
) -> Result<Option<WorkerSessionProjection>> {
    let session = repo
        .session_projection_active_for_card(&card.to_string())
        .await?
        .filter(|session| {
            session.kind == WorkerSessionKind::SharedPlanner
                && session
                    .handle_state_json
                    .as_ref()
                    .is_some_and(crate::harness::is_harness_snapshot_value)
        });
    Ok(session)
}
