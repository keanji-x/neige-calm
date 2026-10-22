//! Authoritative Planner card and active historical harness lookup.
use crate::db::Repo;
use crate::error::Result;
use crate::ids::{CardId, TrackId};
use crate::model::CardRole;
use crate::session_projection_repo::{WorkerSessionKind, WorkerSessionProjection};
use crate::state::WriteContext;

pub(crate) async fn resolve(
    repo: &dyn Repo,
    write: &WriteContext,
    track: &TrackId,
) -> Result<Option<(CardId, Option<WorkerSessionProjection>)>> {
    let Some(card) = repo
        .cards_by_track(track.as_str())
        .await?
        .into_iter()
        .find(|card| write.verify_role(&card.id) == Some(CardRole::Planner))
    else {
        return Ok(None);
    };
    let session = repo
        .session_projection_active_for_card(&card.id.to_string())
        .await?
        .filter(|session| {
            session.kind == WorkerSessionKind::SharedPlanner
                && session
                    .handle_state_json
                    .as_ref()
                    .is_some_and(crate::harness::is_harness_snapshot_value)
        });
    Ok(Some((card.id, session)))
}
