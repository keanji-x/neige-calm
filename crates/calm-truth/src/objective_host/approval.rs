//! Bind human approval to the exact, unanswered conversation ask inside the answer transaction.
use super::*;
use crate::decision_gate::{
    PrincipalDecisionGate, WriteTx, enforce_role_resolving_session_from_tx,
};
use calm_types::event::{AskAction, AskDelivery, Event as TrackEvent};
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
    match &actor {
        ActorId::AiPlanner(card) => {
            // Explicit historical card provenance has no original session. Keep it readable;
            // new asks always retain the real session actor and take the live gate below.
            let role: Option<String> =
                sqlx::query_scalar("SELECT role FROM cards WHERE id=? AND track_id=?")
                    .bind(card.as_str())
                    .bind(input.track_id.as_str())
                    .fetch_optional(&mut **tx)
                    .await?;
            if !role
                .and_then(|role| crate::model::CardRole::try_from(role).ok())
                .is_some_and(|role| role.capability_role() == crate::model::CardRole::Planner)
            {
                return Err(forbidden());
            }
        }
        ActorId::AiPlannerSession(session) | ActorId::AiManagedSession(session) => {
            let source = crate::db::sqlite::event_inputs::event_input_tx(tx, *ask_id)
                .await?
                .envelope;
            let card = source.scope.card_id().ok_or_else(forbidden)?;
            let role = tx.read_card_role(card).await?.ok_or_else(forbidden)?;
            if role.capability_role() != crate::model::CardRole::Planner
                || source.scope.area_id() != tx.read_track_area(&input.track_id).await?.as_ref()
            {
                return Err(forbidden());
            }
            PrincipalDecisionGate::new(Principal::Agent {
                session_id: session.clone(),
                track_id: input.track_id.clone(),
                area_id: source.scope.area_id().cloned().ok_or_else(forbidden)?,
            })
            .decide_card_write(tx, &input.track_id, card, role)
            .await?
            .into_result()?;
            // The shared role gate also binds the actor variant to the real session contract.
            enforce_role_resolving_session_from_tx(tx, &actor, &source.event, &source.scope)
                .await
                .map_err(|_| forbidden())?;
        }
        _ => return Err(forbidden()),
    }
    let ask = TrackEvent::from_kind_and_payload(
        "ask.requested",
        serde_json::from_str(row.try_get("payload")?).map_err(|_| forbidden())?,
    )
    .map_err(|_| forbidden())?;
    match ask {
        TrackEvent::AskRequested {
            track_id,
            delivery: AskDelivery::Wake,
            action:
                Some(
                    AskAction::ObjectiveApproval {
                        objective: key,
                        standard_hash,
                    }
                    | AskAction::ObjectiveApprovalV2 {
                        objective: key,
                        standard_hash,
                    },
                ),
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
