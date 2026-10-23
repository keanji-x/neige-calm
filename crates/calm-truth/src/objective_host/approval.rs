//! Bind human approval to the exact, unanswered Planner ask inside the answer transaction.
use super::*;
use calm_types::event::{AskAction, Event as TrackEvent};
use calm_types::ids::ActorId;
use sqlx::Row;

pub(super) async fn verify_source_tx(
    tx: &mut Transaction<'_, Sqlite>,
    input: &ObjectiveInput,
) -> Result<()> {
    let (
        Event::ApprovalGranted {
            objective,
            approval,
        },
        InputSource::AskAnswered { ask_id },
    ) = (&input.event, &input.source)
    else {
        return Err(TruthError::Forbidden(
            "objective approval needs an ask".into(),
        ));
    };
    let forbidden = || {
        TruthError::Forbidden("objective approval does not match an unanswered Planner ask".into())
    };
    let row = sqlx::query(
        "SELECT r.actor, r.payload FROM events r \
         WHERE r.id=? AND r.kind='ask.requested' AND r.scope_track=? \
           AND NOT EXISTS(SELECT 1 FROM events a WHERE a.scope_track=r.scope_track \
               AND a.kind='ask.answered' AND json_extract(a.payload,'$.ask_id')=r.id)",
    )
    .bind(ask_id)
    .bind(input.track_id.as_str())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(forbidden)?;
    let actor: ActorId = serde_json::from_str(row.try_get("actor")?).map_err(|_| forbidden())?;
    let ActorId::AiPlanner(card) = actor else {
        return Err(forbidden());
    };
    let planner: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM cards WHERE id=? AND track_id=? AND role='planner')",
    )
    .bind(card.as_str())
    .bind(input.track_id.as_str())
    .fetch_one(&mut **tx)
    .await?;
    if !planner {
        return Err(forbidden());
    }
    let ask = TrackEvent::from_kind_and_payload(
        "ask.requested",
        serde_json::from_str(row.try_get("payload")?).map_err(|_| forbidden())?,
    )
    .map_err(|_| forbidden())?;
    match ask {
        TrackEvent::AskRequested {
            track_id,
            action:
                Some(AskAction::ObjectiveApproval {
                    objective: key,
                    standard_hash,
                }),
            ..
        } if track_id == input.track_id
            && key == objective.as_str()
            && standard_hash == approval.standard_hash.as_str() =>
        {
            Ok(())
        }
        _ => Err(forbidden()),
    }
}
